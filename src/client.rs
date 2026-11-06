//! Mach client and terminal output. This module neither starts the broker nor elevates privileges.

use crate::mach::{self, Event, Message, ReceiveRight, ReplyPort, SendRight};
use crate::protocol::{AuditToken, Operation, MAX_BODY};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

pub const EX_USAGE: i32 = 64;
pub const EX_UNAVAILABLE: i32 = 69;
pub const EX_SOFTWARE: i32 = 70;
pub const EX_NOPERM: i32 = 77;
pub const EX_INTERRUPT: i32 = 130;

#[derive(Debug)]
struct ClientError {
    name: &'static str,
    code: i32,
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}
impl std::error::Error for ClientError {}
fn failure(name: &'static str, code: i32) -> anyhow::Error {
    ClientError { name, code }.into()
}
pub fn exit_code(error: &anyhow::Error) -> i32 {
    error.downcast_ref::<ClientError>().map_or(1, |e| e.code)
}

/// JSON always goes to stdout. Plain-text errors always go to stderr.
pub fn print_error(name: &str, code: i32, json_output: bool) -> Result<i32> {
    if json_output {
        write_json(&json!({"ok": false, "error": name}))?;
    } else {
        let style = Style::terminal();
        let mut stream = io::stderr().lock();
        writeln!(
            stream,
            "{}",
            style.err(
                &format!("Error: {}", error_text(&Value::String(name.into()))),
                "1;31"
            )
        )?;
        stream.flush()?;
    }
    Ok(code)
}

struct Session {
    server: SendRight,
    receive: ReceiveRight,
    streaming: bool,
}
impl Session {
    fn open(operation: Operation, body: &Value) -> Result<Self> {
        // Encoding and length validation must precede port creation.
        if !body.is_object() {
            return Err(failure("invalid_request", EX_USAGE));
        }
        if serde_json::to_vec(body)?.len() > MAX_BODY {
            return Err(failure("request_too_large", EX_USAGE));
        }
        let server =
            mach::lookup_service().map_err(|_| failure("helper_unavailable", EX_UNAVAILABLE))?;
        let receive =
            ReceiveRight::new().map_err(|_| failure("receive_port_failed", EX_SOFTWARE))?;
        server
            .watch_dead(&receive)
            .map_err(|_| failure("dead_name_watch_failed", EX_SOFTWARE))?;
        let session = Self {
            server,
            receive,
            streaming: operation == Operation::Subscribe,
        };
        let reply = if session.streaming {
            ReplyPort::Send(&session.receive)
        } else {
            ReplyPort::Once(&session.receive)
        };
        session
            .server
            .send(operation, 1, body, reply, 100)
            .map_err(|_| failure("helper_send_failed", EX_UNAVAILABLE))?;
        Ok(session)
    }

    fn next(&self) -> Result<Option<Message>> {
        let timeout = if self.streaming { 1000 } else { 4000 };
        let event = mach::receive(self.receive.name(), timeout)
            .map_err(|_| failure("helper_timeout", EX_SOFTWARE))?;
        match event {
            Event::Message(message) => Ok(Some(message)),
            Event::Timeout if self.streaming => Ok(None),
            Event::Timeout => Err(failure("helper_timeout", EX_SOFTWARE)),
            Event::DeadName(name) if name == self.server.name() => {
                Err(failure("helper_disconnected", EX_UNAVAILABLE))
            }
            _ => Err(failure("invalid_helper_reply", EX_SOFTWARE)),
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Cancel notifications before releasing send/receive rights.
        let _ = self.server.unwatch_dead();
    }
}

fn validate_reply(
    message: &Message,
    operation: Operation,
    audit: Option<AuditToken>,
) -> Result<()> {
    if message.audit.euid() != 0
        || audit.is_some_and(|a| a != message.audit)
        || message.reply.is_some()
    {
        return Err(failure("invalid_helper_reply", EX_SOFTWARE));
    }
    if !message.body.is_object() {
        return Err(failure("invalid_reply_body", EX_SOFTWARE));
    }
    let valid = if operation == Operation::Subscribe {
        matches!(
            message.operation,
            Operation::BypassLog
                | Operation::ReplayBegin
                | Operation::ReplayEnd
                | Operation::StreamEnd
                | Operation::Gap
                | Operation::Error
        )
    } else {
        message.operation == operation
            || message.operation == Operation::Error
            || (operation == Operation::Attach && message.operation == Operation::Status)
    };
    if !valid {
        return Err(failure("invalid_helper_reply", EX_SOFTWARE));
    }
    Ok(())
}

/// Bootstrap uses a silent, single-request RPC. Failures preserve the client exit code.
pub fn request(operation: Operation, body: Value) -> Result<Value> {
    if operation == Operation::Subscribe {
        return Err(failure("stream_requires_exchange", EX_USAGE));
    }
    let session = Session::open(operation, &body)?;
    let message = session
        .next()?
        .ok_or_else(|| failure("helper_timeout", EX_SOFTWARE))?;
    validate_reply(&message, operation, None)?;
    Ok(message.body)
}

pub fn exchange(
    operation: Operation,
    body: Value,
    snapshot: bool,
    all: bool,
    json_output: bool,
) -> Result<i32> {
    match exchange_inner(operation, body, snapshot, all, json_output) {
        Ok(code) => Ok(code),
        Err(error) => print_error(&error.to_string(), exit_code(&error), json_output),
    }
}

fn exchange_inner(
    operation: Operation,
    body: Value,
    snapshot: bool,
    all: bool,
    json_output: bool,
) -> Result<i32> {
    let streaming = operation == Operation::Subscribe;
    // Session validates the request first. Invalid requests must not change the SIGINT handler.
    let session = Session::open(operation, &body)?;
    let _interrupt = if streaming {
        Some(InterruptGuard::new()?)
    } else {
        None
    };
    let mut audit = None;
    loop {
        if streaming && INTERRUPTED.load(Ordering::Relaxed) {
            return Ok(EX_INTERRUPT);
        }
        let next = session.next();
        if streaming && INTERRUPTED.load(Ordering::Relaxed) {
            return Ok(EX_INTERRUPT);
        }
        let Some(message) = next? else { continue };
        validate_reply(&message, operation, audit)?;
        audit = Some(message.audit);
        print_reply(&message.body, operation, snapshot, all, json_output)?;
        if !streaming {
            return Ok(if truth(&message.body["ok"]) { 0 } else { 1 });
        }
        if message.operation == Operation::Error
            || message.operation == Operation::StreamEnd
            || (snapshot && message.operation == Operation::ReplayEnd)
        {
            return Ok(
                if message.operation == Operation::Error || has_error(&message.body) {
                    1
                } else {
                    0
                },
            );
        }
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt_handler(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
struct InterruptGuard(libc::sigaction);
impl InterruptGuard {
    fn new() -> Result<Self> {
        INTERRUPTED.store(false, Ordering::Relaxed);
        // Install only for interactive log streaming. Drop restores the caller's handler.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let mut previous: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = interrupt_handler as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(anyhow!(io::Error::last_os_error()));
            }
            Ok(Self(previous))
        }
    }
}
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &self.0, std::ptr::null_mut());
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Style {
    stdout: bool,
    stderr: bool,
}
impl Style {
    fn terminal() -> Self {
        let term = std::env::var("TERM").ok();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        Self {
            stdout: color_enabled(io::stdout().is_terminal(), no_color, term.as_deref()),
            stderr: color_enabled(io::stderr().is_terminal(), no_color, term.as_deref()),
        }
    }
    fn out(self, value: &str, color: &str) -> String {
        paint(value, color, self.stdout)
    }
    fn err(self, value: &str, color: &str) -> String {
        paint(value, color, self.stderr)
    }
}
fn color_enabled(tty: bool, no_color: bool, term: Option<&str>) -> bool {
    tty && !no_color && term != Some("dumb")
}
fn paint(value: &str, color: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{color}m{value}\x1b[0m")
    } else {
        value.into()
    }
}
fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(|s| {
        s.chars()
            .map(|c| {
                if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                    '?'
                } else {
                    c
                }
            })
            .collect()
    })
}
fn truth(value: &Value) -> bool {
    value
        .as_bool()
        .unwrap_or_else(|| value.as_f64().is_some_and(|n| n != 0.0))
}
fn numeric(value: &Value) -> String {
    if value.is_number() {
        value.to_string()
    } else {
        "?".into()
    }
}
fn error_text(value: &Value) -> String {
    text(value)
        .map(|s| s.replace('_', " "))
        .unwrap_or_else(|| "unknown error".into())
}
fn has_error(reply: &Value) -> bool {
    reply.get("error").is_some_and(|v| !v.is_null())
}

/// Keep the local time zone and millisecond precision without changing the process TZ.
fn timestamp(value: &Value) -> String {
    let Some(seconds) = value
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0 && *v < i64::MAX as f64)
    else {
        return "unknown time".into();
    };
    let whole = seconds.floor() as libc::time_t;
    let millis = ((seconds - seconds.floor()) * 1000.0).floor() as u32;
    unsafe {
        let mut time: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&whole, &mut time).is_null() {
            return "unknown time".into();
        }
        let mut date = [0_u8; 64];
        let mut zone = [0_u8; 16];
        let n = libc::strftime(
            date.as_mut_ptr().cast(),
            date.len(),
            c"%Y-%m-%d %H:%M:%S".as_ptr(),
            &time,
        );
        let z = libc::strftime(zone.as_mut_ptr().cast(), zone.len(), c"%z".as_ptr(), &time);
        if n == 0 || z == 0 {
            return "unknown time".into();
        }
        format!(
            "{}.{millis:03} {}",
            String::from_utf8_lossy(&date[..n]),
            String::from_utf8_lossy(&zone[..z])
        )
    }
}

fn process_label(process: &Value) -> String {
    let name = text(&process["identifier"])
        .or_else(|| {
            text(&process["binary_path"])
                .or_else(|| text(&process["responsible_path"]))
                .and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                })
        })
        .unwrap_or_else(|| "unknown process".into());
    format!("{name} (PID {})", numeric(&process["pid"]))
}
fn permission_label(value: &Value) -> String {
    let Some(service) = text(value) else {
        return "unknown permission".into();
    };
    let name = match service.as_str() {
        "kTCCServiceMicrophone" => "Microphone",
        "kTCCServiceCamera" => "Camera",
        "kTCCServiceScreenCapture" => "Screen Recording",
        "kTCCServiceAccessibility" => "Accessibility",
        "kTCCServiceListenEvent" => "Input Monitoring",
        "kTCCServicePostEvent" => "Post Events",
        "kTCCServiceAppleEvents" => "Automation",
        "kTCCServiceSystemPolicyAllFiles" => "Full Disk Access",
        "kTCCServiceSystemPolicyDesktopFolder" => "Desktop Folder",
        "kTCCServiceSystemPolicyDocumentsFolder" => "Documents Folder",
        "kTCCServiceSystemPolicyDownloadsFolder" => "Downloads Folder",
        "kTCCServiceAddressBook" => "Contacts",
        "kTCCServiceCalendar" => "Calendars",
        "kTCCServiceReminders" => "Reminders",
        "kTCCServicePhotos" => "Photos",
        "kTCCServiceLocation" => "Location",
        _ => return service,
    };
    format!("{name} [{service}]")
}

/// By default, filter only the com.apple. prefix in responsible.identifier.
/// JSON and text share this rule; other roles do not affect filtering.
pub fn visible_log(reply: &Value, all: bool) -> bool {
    all || reply["type"].as_str() != Some("record")
        || !reply["record"]["responsible"]["identifier"]
            .as_str()
            .is_some_and(|id| id.starts_with("com.apple."))
}

#[derive(Default)]
struct Formatted {
    stdout: Vec<String>,
    stderr: Vec<String>,
}
fn format_reply(reply: &Value, operation: Operation, snapshot: bool, style: Style) -> Formatted {
    let mut output = Formatted::default();
    match reply["type"].as_str() {
        Some("record") => format_record(reply, style, &mut output.stdout),
        Some("replay_begin") => output.stdout.push(style.out(
            &format!(
                "Replaying logs · {} buffered · {} evicted · times are local",
                numeric(&reply["ring"]["records"]),
                numeric(&reply["ring"]["evicted"])
            ),
            "2",
        )),
        Some("replay_end") => output.stdout.push(style.out(
            if snapshot {
                "End of snapshot"
            } else {
                "Following live permission checks… (Ctrl-C to stop)"
            },
            "2",
        )),
        Some("gap") => output.stdout.push(style.out(
            &format!(
                "Log gap: records {}–{} unavailable ({})",
                numeric(&reply["from"]),
                numeric(&reply["to"]),
                error_text(&reply["reason"])
            ),
            "33",
        )),
        Some("end") => {
            let line = format!(
                "Log stream ended: {}",
                error_text(
                    reply
                        .get("error")
                        .filter(|v| !v.is_null())
                        .or_else(|| reply.get("reason"))
                        .unwrap_or(&Value::Null)
                )
            );
            if has_error(reply) {
                output.stderr.push(style.err(&line, "31"));
            } else {
                output.stdout.push(style.out(&line, "33"));
            }
        }
        _ => format_status(reply, operation, style, &mut output),
    }
    output
}
fn format_record(reply: &Value, style: Style, output: &mut Vec<String>) {
    let record = &reply["record"];
    let actor = if truth(&record["accessing"]["available"]) {
        &record["accessing"]
    } else {
        &record["checked_process"]
    };
    let original = record
        .get("original_entitlement_result")
        .filter(|v| v.is_boolean() || v.is_number());
    let bypass = original.is_some_and(|v| !truth(v));
    output.push(format!(
        "{}  {}  {} → {}",
        style.out(&timestamp(&record["timestamp"]), "2"),
        style.out(
            if bypass { "BYPASS" } else { "CHECK" },
            if bypass { "1;32" } else { "1;33" }
        ),
        style.out(&process_label(actor), "1;36"),
        style.out(&permission_label(&record["service"]), "1;35")
    ));
    if let Some(path) = text(&actor["binary_path"]) {
        output.push(format!("    binary: {path}"));
    }
    for role in ["requesting", "responsible", "checked_process"] {
        let process = &record[role];
        if truth(&process["available"]) && process != actor {
            let label = if role == "checked_process" {
                "checked"
            } else {
                role
            };
            output.push(format!("    {label}: {}", process_label(process)));
        }
    }
    let entitlement = match original {
        None => "unknown",
        Some(v) if truth(v) => "already present",
        Some(_) => "supplied (originally absent)",
    };
    let mut details = vec![
        format!("TCC allow entitlement: {entitlement}"),
        format!(
            "tccd PID {}",
            numeric(record.get("tccd_pid").unwrap_or(&reply["source_pid"]))
        ),
    ];
    if let Some(id) = text(&record["request_id"]) {
        details.push(format!("request {id}"));
    }
    if truth(&record["preflight"]) {
        details.push("preflight".into());
    }
    output.push(format!("    {}", details.join(" · ")));
}
fn format_status(reply: &Value, operation: Operation, style: Style, output: &mut Formatted) {
    let ok = truth(&reply["ok"]);
    let enabled = truth(&reply["enabled"]);
    let title = match (ok, enabled, operation) {
        (true, true, _) => "Bypass enabled",
        (true, false, _) => "Bypass disabled",
        (false, _, Operation::Enable) => "Could not enable bypass on every tccd",
        (false, _, Operation::Disable) => "Could not disable bypass on every tccd",
        _ => "Status incomplete",
    };
    output.stdout.push(style.out(
        title,
        if !ok {
            "1;31"
        } else if enabled {
            "1;32"
        } else {
            "1;33"
        },
    ));
    if reply["helper_pid"].is_number() {
        output.stdout.push(format!(
            "  Helper PID {} · connected {}/{} · acknowledged {}",
            numeric(&reply["helper_pid"]),
            numeric(&reply["connected"]),
            numeric(&reply["expected"]),
            numeric(&reply["acknowledged"])
        ));
    }
    if let Some(states) = reply["results"].as_array() {
        for state in states {
            let active = truth(&state["enabled"]);
            let status = style.out(
                if active { "enabled" } else { "disabled" },
                if active { "32" } else { "33" },
            );
            let expiry = if active {
                format!(" · expires {}", timestamp(&state["expires_at"]))
            } else {
                String::new()
            };
            output.stdout.push(format!(
                "  tccd PID {} (UID {}) · {status}{expiry}",
                numeric(&state["pid"]),
                numeric(&state["uid"])
            ));
            output.stdout.push(format!(
                "    Logs: {} generated · {} sent · {} dropped · {} oversized",
                numeric(&state["logs_generated"]),
                numeric(&state["logs_sent"]),
                numeric(&state["logs_dropped"]),
                numeric(&state["logs_oversized"])
            ));
        }
    }
    if reply["ring"].as_object().is_some_and(|o| !o.is_empty()) {
        let ring = &reply["ring"];
        output.stdout.push(format!(
            "  Log buffer: {} records · {} bytes · {} evicted · {} oversized",
            numeric(&ring["records"]),
            numeric(&ring["bytes"]),
            numeric(&ring["evicted"]),
            numeric(&ring["oversized"])
        ));
    }
    if has_error(reply) {
        output
            .stderr
            .push(style.err(&format!("Error: {}", error_text(&reply["error"])), "31"));
    }
    if let Some(errors) = reply["errors"].as_array() {
        for error in errors {
            output.stderr.push(style.err(
                &format!(
                    "  tccd PID {}: {}",
                    numeric(&error["pid"]),
                    error_text(&error["error"])
                ),
                "31",
            ));
        }
    }
    if let Some(missing) = reply["missing_pids"].as_array() {
        for pid in missing {
            output
                .stderr
                .push(style.err(&format!("  tccd PID {}: not connected", numeric(pid)), "31"));
        }
    }
}
fn write_json(value: &Value) -> Result<()> {
    let mut stream = io::stdout().lock();
    serde_json::to_writer(&mut stream, value)?;
    writeln!(stream)?;
    stream.flush()?;
    Ok(())
}
fn print_reply(
    reply: &Value,
    operation: Operation,
    snapshot: bool,
    all: bool,
    json_output: bool,
) -> Result<()> {
    if !visible_log(reply, all) {
        return Ok(());
    }
    if json_output {
        return write_json(reply);
    }
    let output = format_reply(reply, operation, snapshot, Style::terminal());
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    for line in output.stdout {
        writeln!(stdout, "{line}")?;
    }
    for line in output.stderr {
        writeln!(stderr, "{line}")?;
    }
    stdout.flush()?;
    stderr.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_requests_fail_without_mach_lookup() {
        let invalid = request(Operation::Status, json!([])).unwrap_err();
        assert_eq!(exit_code(&invalid), EX_USAGE);
        assert_eq!(invalid.to_string(), "invalid_request");
        let oversized =
            request(Operation::Status, json!({"data":"x".repeat(MAX_BODY)})).unwrap_err();
        assert_eq!(exit_code(&oversized), EX_USAGE);
        assert_eq!(oversized.to_string(), "request_too_large");
        assert_eq!(
            exit_code(&request(Operation::Subscribe, json!({})).unwrap_err()),
            EX_USAGE
        );
        assert_eq!(
            exit_code(&failure("helper_unavailable", EX_UNAVAILABLE).context("bootstrap")),
            EX_UNAVAILABLE
        );
    }
    #[test]
    fn replies_require_root_consistent_audit_and_expected_operation() {
        let root = AuditToken {
            val: [0, 0, 0, 0, 0, 10, 0, 1],
        };
        let mut message = Message {
            local_port: 0,
            operation: Operation::Status,
            request: 12,
            body: json!({"ok":true}),
            audit: root,
            reply: None,
        };
        assert!(validate_reply(&message, Operation::Status, None).is_ok());
        assert!(validate_reply(&message, Operation::Attach, None).is_ok());
        message.audit.val[1] = 501;
        assert_eq!(
            exit_code(&validate_reply(&message, Operation::Status, None).unwrap_err()),
            EX_SOFTWARE
        );
        message.audit = root;
        let mut changed = root;
        changed.val[7] += 1;
        assert!(validate_reply(&message, Operation::Status, Some(changed)).is_err());
        message.operation = Operation::Hello;
        assert!(validate_reply(&message, Operation::Status, None).is_err());
        message.operation = Operation::ReplayEnd;
        assert!(validate_reply(&message, Operation::Subscribe, Some(root)).is_ok());
        message.body = json!([]);
        assert!(validate_reply(&message, Operation::Subscribe, None).is_err());
    }
    #[test]
    fn log_filter_only_uses_responsible_identifier() {
        let apple =
            json!({"type":"record","record":{"responsible":{"identifier":"com.apple.finder"}}});
        assert!(!visible_log(&apple, false));
        assert!(visible_log(&apple, true));
        assert!(visible_log(
            &json!({"type":"record","record":{"accessing":{"identifier":"com.apple.finder"}}}),
            false
        ));
        for id in ["com.apple", "Com.apple.finder", "org.example.app", ""] {
            assert!(visible_log(
                &json!({"type":"record","record":{"responsible":{"identifier":id}}}),
                false
            ));
        }
        assert!(visible_log(&json!({"type":"gap"}), false));
        assert!(visible_log(
            &json!({"type":"record","record":{"responsible":{"identifier":12}}}),
            false
        ));
    }
    #[test]
    fn color_policy_honors_no_color_and_non_terminal() {
        assert!(color_enabled(true, false, Some("xterm-256color")));
        assert!(!color_enabled(false, false, None));
        assert!(!color_enabled(true, true, None));
        assert!(!color_enabled(true, false, Some("dumb")));
        assert_eq!(paint("safe", "31", false), "safe");
    }
    #[test]
    fn hostile_text_cannot_inject_terminal_controls() {
        assert_eq!(text(&json!("a\n\x1b[31m\tb")), Some("a??[31m?b".into()));
        assert_eq!(
            error_text(&json!("helper_unavailable")),
            "helper unavailable"
        );
        assert_eq!(numeric(&json!("123")), "?");
        assert_eq!(
            permission_label(&json!("kTCCServiceCamera")),
            "Camera [kTCCServiceCamera]"
        );
    }
    #[test]
    fn timestamps_reject_invalid_values_and_include_zone() {
        for value in [Value::Null, json!("time"), json!(0), json!(-1), json!(1e30)] {
            assert_eq!(timestamp(&value), "unknown time");
        }
        let rendered = timestamp(&json!(1700000000.125));
        assert!(rendered.contains(".125 "));
        let zone = rendered.split_whitespace().last().unwrap();
        assert_eq!(zone.len(), 5);
        assert!(zone.starts_with('+') || zone.starts_with('-'));
    }
    #[test]
    fn record_format_preserves_actor_details_without_color() {
        let reply = json!({"type":"record","source_pid":88,"record":{
            "timestamp":0,"service":"kTCCServiceCamera","original_entitlement_result":false,
            "accessing":{"available":true,"identifier":"org.example.app","pid":44,"binary_path":"/tmp/App"},
            "responsible":{"available":true,"identifier":"org.example.owner","pid":45},
            "request_id":"r1","preflight":true
        }});
        let lines = format_reply(&reply, Operation::Subscribe, false, Style::default())
            .stdout
            .join("\n");
        assert!(lines.contains("BYPASS  org.example.app (PID 44) → Camera"));
        assert!(lines.contains("binary: /tmp/App"));
        assert!(lines.contains("responsible: org.example.owner (PID 45)"));
        assert!(
            lines.contains("supplied (originally absent) · tccd PID 88 · request r1 · preflight")
        );
        assert!(!lines.contains('\x1b'));
    }
    #[test]
    fn status_errors_have_separate_streams() {
        let reply = json!({"ok":false,"error":"incomplete_or_stale_connections","missing_pids":[7],
            "errors":[{"pid":8,"error":"port_dead"}],"ring":{"records":3,"bytes":50,"evicted":1,"oversized":0}});
        let output = format_reply(&reply, Operation::Enable, false, Style::default());
        assert_eq!(output.stdout[0], "Could not enable bypass on every tccd");
        assert!(output
            .stdout
            .iter()
            .any(|l| l.contains("Log buffer: 3 records")));
        assert_eq!(output.stderr.len(), 3);
        assert!(output.stderr[2].contains("PID 7: not connected"));
    }
    #[test]
    fn replay_gap_and_end_format_are_explicit() {
        let style = Style::default();
        assert_eq!(
            format_reply(
                &json!({"type":"replay_end"}),
                Operation::Subscribe,
                true,
                style
            )
            .stdout[0],
            "End of snapshot"
        );
        assert!(format_reply(
            &json!({"type":"gap","from":2,"to":9,"reason":"ring_evicted"}),
            Operation::Subscribe,
            false,
            style
        )
        .stdout[0]
            .contains("2–9 unavailable (ring evicted)"));
        let end = format_reply(
            &json!({"type":"end","error":"root_required"}),
            Operation::Subscribe,
            false,
            style,
        );
        assert!(end.stdout.is_empty());
        assert_eq!(end.stderr[0], "Log stream ended: root required");
    }
}
