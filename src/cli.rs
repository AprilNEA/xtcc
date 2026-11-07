//! Argument parsing must finish before checking for root or building, making RPCs, or attaching.

use crate::protocol::{Connection, Operation, MAX_BODY};
use crate::{bootstrap, broker, client, ne, process};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct Connections(Vec<Connection>);

#[derive(Debug, Parser)]
#[command(
    name = "xtcc",
    version,
    about = "Local TCC and NetworkExtension research controller; never invokes sudo automatically",
    long_about = "Local TCC and NetworkExtension research controller. Runtime commands require root, but the program never invokes sudo automatically.\nTCC injection uses LLDB with embedded Python. NE controls require SIP-disabled development Macs. doctor only checks files in read-only mode; it does not probe privileges or attach to processes."
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Output JSON; logs use newline-delimited JSON"
    )]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Build the local native libraries and controller")]
    Build {
        #[arg(
            long,
            help = "Rebuild xtcc release artifacts, including embedded native libraries"
        )]
        force: bool,
    },
    #[command(
        visible_alias = "enable",
        about = "Enable or renew; defaults to 30 minutes"
    )]
    Start {
        #[arg(default_value = "30", value_parser = parse_minutes)]
        minutes: u32,
    },
    #[command(visible_alias = "disable", about = "Disable the existing bypass")]
    Stop,
    #[command(about = "Query the latest status of all connected tccd processes")]
    Status,
    #[command(about = "Replay and follow logs; filter out Apple responsible processes by default")]
    Logs {
        #[arg(long, help = "Read only the current snapshot")]
        snapshot: bool,
        #[arg(
            long,
            help = "Include logs whose responsible.identifier starts with com.apple."
        )]
        all: bool,
    },
    #[command(
        about = "Check runtime tools and embedded native libraries in read-only mode; root is not required"
    )]
    Doctor,
    #[command(about = "Manage Network Extensions on SIP-disabled development Macs")]
    Ne {
        #[arg(long, global = true, default_value = "30", value_parser = clap::value_parser!(u32).range(1..=300), help = "Timeout in seconds for each one-shot NE client")]
        timeout: u32,
        #[command(subcommand)]
        command: ne::Command,
    },
    #[command(name = "_serve", hide = true)]
    Serve {
        #[arg(value_parser = parse_connections)]
        connections: Connections,
        #[arg(long, default_value = "1024", value_parser = parse_capacity)]
        capacity: usize,
    },
    #[command(name = "_attach", hide = true)]
    Attach {
        #[arg(value_parser = parse_connections)]
        connections: Connections,
    },
}

fn unsigned_decimal(
    value: &str,
    minimum: u64,
    maximum: u64,
    message: &str,
) -> std::result::Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return Err(message.into());
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n >= minimum && *n <= maximum)
        .ok_or_else(|| message.into())
}
fn parse_minutes(value: &str) -> std::result::Result<u32, String> {
    unsigned_decimal(value, 1, 1440, "minutes_must_be_1_to_1440").map(|n| n as u32)
}
fn parse_capacity(value: &str) -> std::result::Result<usize, String> {
    unsigned_decimal(value, 1, 65536, "capacity_must_be_1_to_65536").map(|n| n as usize)
}
fn parse_connections(value: &str) -> std::result::Result<Connections, String> {
    let invalid = || "invalid_connections".to_string();
    let parsed: Value = serde_json::from_str(value).map_err(|_| invalid())?;
    let items = parsed
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(invalid)?;
    // Attach requests must also fit in the wire body. Validate before making any RPCs.
    let body = json!({"connections": parsed});
    if serde_json::to_vec(&body).map_err(|_| invalid())?.len() > MAX_BODY {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .filter(|o| o.len() == 2)
            .ok_or_else(invalid)?;
        let pid = object
            .get("pid")
            .and_then(Value::as_i64)
            .filter(|p| *p > 0 && *p <= i32::MAX as i64)
            .ok_or_else(invalid)? as i32;
        let receive = object
            .get("receive")
            .and_then(Value::as_u64)
            .filter(|p| *p > 0 && *p < u32::MAX as u64)
            .ok_or_else(invalid)? as u32;
        if !seen.insert(pid) {
            return Err(invalid());
        }
        result.push(Connection { pid, receive });
    }
    Ok(Connections(result))
}

pub fn run() -> Result<i32> {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    let json_requested = arguments.iter().any(|a| a == "--json" || a == "_attach");
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                error.print()?;
                return Ok(0);
            }
            if json_requested {
                return client::print_error(&error.to_string(), client::EX_USAGE, true);
            }
            error.print()?;
            return Ok(client::EX_USAGE);
        }
    };
    let json_output = cli.json || matches!(cli.command, Command::Attach { .. });
    match dispatch(cli) {
        Ok(code) => Ok(code),
        Err(error) => {
            client::print_error(&error.to_string(), client::exit_code(&error), json_output)
        }
    }
}

fn require_root(json_output: bool) -> Result<Option<i32>> {
    if process::require_root().is_err() {
        return Ok(Some(client::print_error(
            "root_required; rerun explicitly with sudo",
            client::EX_NOPERM,
            json_output,
        )?));
    }
    Ok(None)
}
fn dispatch(cli: Cli) -> Result<i32> {
    // clap value parsers have validated all arguments. Side effects are allowed only from here on.
    let json_output = cli.json || matches!(cli.command, Command::Attach { .. });
    if !matches!(cli.command, Command::Build { .. } | Command::Doctor) {
        if let Some(code) = require_root(json_output)? {
            return Ok(code);
        }
    }
    match cli.command {
        Command::Build { force } => build(force, json_output),
        Command::Start { minutes } => {
            bootstrap::ensure_broker()?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before Unix epoch")?
                .as_secs();
            let expires_at = now
                .checked_add(u64::from(minutes) * 60)
                .context("expiry overflow")?;
            client::exchange(
                Operation::Enable,
                json!({"expires_at": expires_at}),
                false,
                false,
                json_output,
            )
        }
        Command::Stop => client::exchange(Operation::Disable, json!({}), false, false, json_output),
        Command::Status => {
            client::exchange(Operation::Status, json!({}), false, false, json_output)
        }
        Command::Logs { snapshot, all } => client::exchange(
            Operation::Subscribe,
            json!({"snapshot": snapshot}),
            snapshot,
            all,
            json_output,
        ),
        Command::Doctor => doctor(json_output),
        Command::Ne { command, timeout } => ne::run(command, timeout, json_output),
        Command::Serve {
            connections,
            capacity,
        } => {
            if let Err(error) = broker::run(connections.0, capacity) {
                crate::lifecycle::log(&format!("broker failed: {error:#}"));
                return Err(error);
            }
            Ok(0)
        }
        Command::Attach { connections } => client::exchange(
            Operation::Attach,
            json!({"connections": connections.0}),
            false,
            false,
            true,
        ),
    }
}
fn build(force: bool, json_output: bool) -> Result<i32> {
    let mut command = ProcessCommand::new("/usr/bin/make");
    command.current_dir(env!("CARGO_MANIFEST_DIR"));
    command.arg(if force { "rebuild" } else { "all" });
    if json_output {
        let output = command.output().context("could not run /usr/bin/make")?;
        let ok = output.status.success();
        let value = json!({"ok": ok, "command":"build", "force":force, "exit_code":output.status.code(),
            "stdout":String::from_utf8_lossy(&output.stdout), "stderr":String::from_utf8_lossy(&output.stderr)});
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &value)?;
        writeln!(stdout)?;
        Ok(if ok { 0 } else { 1 })
    } else {
        Ok(
            if command
                .status()
                .context("could not run /usr/bin/make")?
                .success()
            {
                0
            } else {
                1
            },
        )
    }
}

fn executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
fn find_tool(name: &str, fixed: &[PathBuf]) -> Option<PathBuf> {
    let mut paths = fixed.to_vec();
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path).map(|p| p.join(name)));
    }
    paths.into_iter().find(|p| executable_file(p))
}

/// Does not run tools, check SIP, query TCC, or discover or attach to tccd processes.
fn doctor(json_output: bool) -> Result<i32> {
    let mut developer_dirs = vec![
        PathBuf::from("/Library/Developer/CommandLineTools"),
        PathBuf::from("/Applications/Xcode.app/Contents/Developer"),
    ];
    if let Some(path) = std::env::var_os("DEVELOPER_DIR") {
        developer_dirs.insert(0, path.into());
    }
    let tools: Vec<Value> = ["xcrun", "lldb"]
        .iter()
        .map(|name| {
            let mut fixed = vec![PathBuf::from("/usr/bin").join(name)];
            fixed.extend(developer_dirs.iter().map(|p| p.join("usr/bin").join(name)));
            let path = find_tool(name, &fixed);
            json!({"name": name, "found": path.is_some(), "path": path})
        })
        .collect();
    let tcc_library = bootstrap::NativeLibrary::TccInject;
    let ne_library = bootstrap::NativeLibrary::NetworkExtension;
    let current_executable = std::env::current_exe().ok();
    let ok = tools.iter().all(|t| t["found"] == true)
        && !tcc_library.bytes().is_empty()
        && !ne_library.bytes().is_empty();
    let value = json!({"ok":ok,"tools":tools,"files":{
        "executable":current_executable,
        "library":{
            "name":tcc_library.file_name(),
            "embedded":true,
            "bytes":tcc_library.bytes().len(),
            "runtime_path":tcc_library.runtime_path(),
        },
        "ne_library":{
            "name":ne_library.file_name(),
            "embedded":true,
            "bytes":ne_library.bytes().len(),
            "runtime_path":ne_library.runtime_path(),
        }},
        "lldb_python":"LLDB still depends on its embedded Python; runtime compatibility is not tested",
        "checks":"read-only file metadata and embedded payloads; no privilege probes, no SIP checks, no process attachment"});
    let mut stdout = io::stdout().lock();
    if json_output {
        serde_json::to_writer(&mut stdout, &value)?;
        writeln!(stdout)?;
    } else {
        writeln!(
            stdout,
            "Doctor: {}",
            if ok {
                "tools and embedded libraries ready"
            } else {
                "required tools missing"
            }
        )?;
        for tool in value["tools"].as_array().unwrap() {
            writeln!(
                stdout,
                "  {}: {}",
                tool["name"].as_str().unwrap(),
                tool["path"].as_str().unwrap_or("not found")
            )?;
        }
        for key in ["library", "ne_library"] {
            let entry = &value["files"][key];
            writeln!(
                stdout,
                "  {key}: embedded ({} bytes -> {})",
                entry["bytes"].as_u64().unwrap_or(0),
                entry["runtime_path"].as_str().unwrap_or("unknown")
            )?;
        }
        writeln!(
            stdout,
            "LLDB still depends on embedded Python; runtime compatibility is unverified."
        )?;
        writeln!(
            stdout,
            "No privilege probes, SIP checks, process discovery, or attachment were performed."
        )?;
    }
    stdout.flush()?;
    Ok(if ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_and_enable_share_validated_minutes() {
        for command in ["start", "enable"] {
            let cli = Cli::try_parse_from(["xtcc", command]).unwrap();
            assert!(matches!(cli.command, Command::Start { minutes: 30 }));
            for minutes in ["1", "1440"] {
                assert!(Cli::try_parse_from(["xtcc", command, minutes, "--json"]).is_ok());
            }
            for invalid in [
                "0",
                "1441",
                "-1",
                "+1",
                "1.5",
                "NaN",
                " 30",
                "18446744073709551616",
            ] {
                assert!(
                    Cli::try_parse_from(["xtcc", command, invalid]).is_err(),
                    "{invalid}"
                );
            }
        }
    }
    #[test]
    fn aliases_and_global_json_are_supported() {
        assert!(matches!(
            Cli::try_parse_from(["xtcc", "disable"]).unwrap().command,
            Command::Stop
        ));
        assert!(
            Cli::try_parse_from(["xtcc", "--json", "status"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["xtcc", "logs", "--all", "--snapshot", "--json"])
                .unwrap()
                .json
        );
        assert!(Cli::try_parse_from(["xtcc", "build", "--force", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["xtcc", "doctor", "--json"]).is_ok());
    }
    #[test]
    fn unrelated_arguments_are_rejected_before_dispatch() {
        for args in [
            vec!["xtcc", "stop", "30"],
            vec!["xtcc", "status", "--force"],
            vec!["xtcc", "logs", "--unknown"],
            vec!["xtcc", "doctor", "--probe"],
            vec!["xtcc", "start", "30", "extra"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
    #[test]
    fn connections_are_strictly_validated() {
        let good = r#"[{"pid":12,"receive":34}]"#;
        assert_eq!(parse_connections(good).unwrap().0[0].pid, 12);
        assert!(Cli::try_parse_from(["xtcc", "_attach", good]).is_ok());
        for bad in [
            "{}",
            "[]",
            "not-json",
            r#"[{"pid":0,"receive":2}]"#,
            r#"[{"pid":1,"receive":0}]"#,
            r#"[{"pid":"1","receive":2}]"#,
            r#"[{"pid":1,"receive":4294967295}]"#,
            r#"[{"pid":1.5,"receive":2}]"#,
            r#"[{"pid":1,"receive":2,"extra":3}]"#,
            r#"[{"pid":1,"receive":2},{"pid":1,"receive":3}]"#,
        ] {
            assert!(parse_connections(bad).is_err(), "{bad}");
        }
        let many: Vec<Value> = (1..=1000)
            .map(|pid| json!({"pid":pid,"receive":1}))
            .collect();
        assert!(parse_connections(&serde_json::to_string(&many).unwrap()).is_err());
    }
    #[test]
    fn serve_capacity_is_bounded() {
        let connections = r#"[{"pid":1,"receive":2}]"#;
        assert!(matches!(
            Cli::try_parse_from(["xtcc", "_serve", connections])
                .unwrap()
                .command,
            Command::Serve { capacity: 1024, .. }
        ));
        for cap in ["1", "65536"] {
            assert!(
                Cli::try_parse_from(["xtcc", "_serve", connections, "--capacity", cap]).is_ok()
            );
        }
        for cap in ["0", "65537", "+1", "-1", "abc"] {
            assert!(
                Cli::try_parse_from(["xtcc", "_serve", connections, "--capacity", cap]).is_err()
            );
        }
    }
    #[test]
    fn internal_commands_are_hidden_from_help() {
        use clap::CommandFactory;
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("_serve"));
        assert!(!help.contains("_attach"));
        assert!(help.contains("doctor"));
    }
}
