//! Rust only orchestrates LLDB, which still depends on its embedded Python runtime.
//! Offline tests must not call ensure_broker, inject, or spawn_broker.

use crate::protocol::{Connection, Operation};
use crate::{client, process};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUNTIME_DIR: &str = "/private/var/run/xtcc";
const RUNTIME_MODE: u32 = 0o755;
const TCC_LIBRARY_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libxtcc_inject.dylib"));
const NE_LIBRARY_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libxtcc_ne.dylib"));
const LLDB_SCRIPT: &str = include_str!("../native/lldb_bootstrap.py");

/// Embedded `arm64e` native library materialized into a root-owned runtime directory on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeLibrary {
    TccInject,
    NetworkExtension,
}

impl NativeLibrary {
    /// Returns the Mach-O dynamic library filename.
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::TccInject => "libxtcc_inject.dylib",
            Self::NetworkExtension => "libxtcc_ne.dylib",
        }
    }

    /// Returns the embedded `arm64e` Mach-O payload compiled by `build.rs`.
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::TccInject => TCC_LIBRARY_BYTES,
            Self::NetworkExtension => NE_LIBRARY_BYTES,
        }
    }

    /// Returns the default runtime materialization path under `/private/var/run/xtcc`.
    pub fn runtime_path(self) -> PathBuf {
        Path::new(RUNTIME_DIR).join(self.file_name())
    }

    /// Writes the embedded library to `/private/var/run/xtcc` if missing or outdated.
    pub fn materialize(self) -> Result<PathBuf> {
        self.materialize_in(Path::new(RUNTIME_DIR))
    }

    pub(crate) fn materialize_in(self, directory: &Path) -> Result<PathBuf> {
        DirBuilder::new()
            .recursive(true)
            .mode(RUNTIME_MODE)
            .create(directory)
            .with_context(|| {
                format!("could not create runtime directory {}", directory.display())
            })?;
        fs::set_permissions(directory, fs::Permissions::from_mode(RUNTIME_MODE)).with_context(
            || {
                format!(
                    "could not set permissions on runtime directory {}",
                    directory.display()
                )
            },
        )?;
        let target = directory.join(self.file_name());
        if fs::read(&target).ok().as_deref() == Some(self.bytes()) {
            fs::set_permissions(&target, fs::Permissions::from_mode(RUNTIME_MODE))
                .with_context(|| format!("could not set permissions on {}", target.display()))?;
            return validate_library(&target);
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = directory.join(format!(
            ".{}.{}.{nonce}.tmp",
            self.file_name(),
            std::process::id()
        ));
        let write_result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(RUNTIME_MODE)
                .open(&temporary)
                .with_context(|| format!("could not create {}", temporary.display()))?;
            file.write_all(self.bytes())
                .with_context(|| format!("could not write {}", temporary.display()))?;
            file.set_permissions(fs::Permissions::from_mode(RUNTIME_MODE))
                .with_context(|| format!("could not set permissions on {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("could not sync {}", temporary.display()))?;
            fs::rename(&temporary, &target).with_context(|| {
                format!(
                    "could not move {} to {}",
                    temporary.display(),
                    target.display()
                )
            })
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;
        validate_library(&target)
    }
}

/// Validate that a materialized `.dylib` file exists and has a valid UTF-8 path.
fn validate_library(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("library not found: {}", path.display()))?;
    let metadata = canonical
        .metadata()
        .context("could not read library metadata")?;
    if !metadata.is_file() || canonical.extension().is_none_or(|e| e != "dylib") {
        bail!(
            "library must be a regular .dylib file: {}",
            canonical.display()
        );
    }
    let text = canonical
        .to_str()
        .context("library path is not valid UTF-8")?;
    if text.as_bytes().contains(&0) {
        bail!("library path contains NUL");
    }
    Ok(canonical)
}

fn healthy(reply: &Value) -> bool {
    let connected = reply["connected"].as_u64().unwrap_or(0);
    reply["ok"].as_bool() == Some(true)
        && reply["helper_pid"].as_i64().is_some_and(|p| p > 0)
        && connected > 0
        && reply["expected"].as_u64() == Some(connected)
        && reply["acknowledged"].as_u64() == Some(connected)
}

/// Request fresh broker status first. Load the library only if the broker is missing or explicitly stale.
/// Errors, timeouts, and unauthenticated replies must not trigger a new process attachment.
pub fn ensure_broker() -> Result<Value> {
    process::require_root()?;
    match client::request(Operation::Status, json!({})) {
        Ok(reply) if healthy(&reply) => return Ok(reply),
        Ok(_) => {}
        Err(error) if client::exit_code(&error) == client::EX_UNAVAILABLE => {}
        Err(error) => {
            return Err(error.context("cannot verify broker health; no attachment attempted"))
        }
    }
    let library = NativeLibrary::TccInject.materialize()?;
    let pids = process::live_tccd()?;
    if pids.is_empty() {
        bail!("no running tccd");
    }
    let mut seen = HashSet::new();
    for &pid in &pids {
        if pid <= 0 || !seen.insert(pid) {
            bail!("invalid or duplicate tccd pid");
        }
    }
    let mut connections = Vec::with_capacity(pids.len());
    for pid in pids {
        connections.push(inject(pid, &library)?);
    }
    let body = json!({"connections": connections});
    match client::request(Operation::Attach, body.clone()) {
        Ok(reply) => require_healthy(reply),
        Err(error) if client::exit_code(&error) == client::EX_UNAVAILABLE => {
            start_and_attach(&connections, body)
        }
        Err(error) => Err(error),
    }
}

fn require_healthy(reply: Value) -> Result<Value> {
    if healthy(&reply) {
        Ok(reply)
    } else {
        bail!(
            "broker setup incomplete: {}",
            serde_json::to_string(&reply)?
        );
    }
}

fn lldb_program(pid: i32, library: &Path) -> Result<String> {
    if pid <= 0 {
        bail!("invalid pid");
    }
    let library = library
        .to_str()
        .context("library path is not valid UTF-8")?;
    Ok(format!(
        "pid={pid}\nlibrary={}\nexpected_tccd={}\n{LLDB_SCRIPT}",
        serde_json::to_string(library)?,
        serde_json::to_string(process::TCCD_PATH)?
    ))
}

fn inject(pid: i32, library: &Path) -> Result<Connection> {
    process::verify_tccd(pid)?;
    let library = validate_library(library)?;
    let program = lldb_program(pid, &library)?;
    let command = format!("script exec({})", serde_json::to_string(&program)?);
    let output = Command::new("/usr/bin/xcrun")
        .args([
            "lldb",
            "--batch",
            "--no-lldbinit",
            "-o",
            &command,
            "-o",
            "quit",
        ])
        .output();
    // Perform job-control cleanup only after LLDB exits, including when loading fails.
    // Revalidate the actual path to avoid sending SIGCONT to another process that reused the PID.
    let cleanup = resume_after_detach(pid);
    let output = match (output, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) => return Err(error).context("could not run LLDB"),
        (Ok(_), Err(error)) => return Err(error).context("LLDB detach cleanup failed"),
        (Err(error), Err(cleanup)) => {
            return Err(anyhow!(
                "could not run LLDB: {error}; detach cleanup failed: {cleanup:#}"
            ))
        }
    };
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_injection_report(pid, output.status.success(), &text)
}

fn resume_after_detach(pid: i32) -> Result<()> {
    process::verify_tccd(pid)?;
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .context("could not read post-detach process state")?;
    if !output.status.success() {
        bail!("could not read post-detach process state");
    }
    let state = String::from_utf8_lossy(&output.stdout);
    if state.contains('T') {
        process::verify_tccd(pid)?;
        if unsafe { libc::kill(pid, libc::SIGCONT) } != 0 {
            return Err(io::Error::last_os_error()).context("could not resume detached tccd");
        }
    }
    Ok(())
}

fn parse_injection_report(pid: i32, succeeded: bool, output: &str) -> Result<Connection> {
    let records: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix("INJECT_RESULT "))
        .collect();
    let errors: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix("INJECT_ERROR "))
        .collect();
    if !succeeded || records.len() != 1 || !errors.is_empty() {
        let detail = if errors.is_empty() {
            output.trim().to_string()
        } else {
            errors.join("\n")
        };
        bail!(
            "LLDB bootstrap failed: {}",
            if detail.is_empty() {
                "missing readiness report"
            } else {
                &detail
            }
        );
    }
    let value: Value = serde_json::from_str(records[0]).context("invalid LLDB readiness JSON")?;
    let reported_pid = value["pid"].as_i64().context("invalid readiness pid")?;
    let receive = value["receive"]
        .as_u64()
        .filter(|p| *p > 0 && *p < u32::MAX as u64)
        .context("invalid readiness port")?;
    if reported_pid != i64::from(pid) {
        bail!("readiness pid mismatch");
    }
    if value.as_object().is_none_or(|o| o.len() != 2) {
        bail!("invalid readiness fields");
    }
    Ok(Connection {
        pid,
        receive: receive as u32,
    })
}

struct StartingChild {
    child: Child,
    ready: bool,
}
impl Drop for StartingChild {
    fn drop(&mut self) {
        if !self.ready {
            // Clean up only the child created here that failed to become ready, not a reused broker.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn spawn_broker(connections: &[Connection]) -> Result<StartingChild> {
    let executable = std::env::current_exe().context("could not locate current executable")?;
    let encoded = serde_json::to_string(connections)?;
    let mut command = Command::new(executable);
    command
        .args(["_serve", &encoded])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Equivalent to upstream start_new_session=True. No shell or automatic sudo invocation.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(StartingChild {
        child: command.spawn().context("could not start broker")?,
        ready: false,
    })
}
fn start_and_attach(connections: &[Connection], body: Value) -> Result<Value> {
    let mut child = spawn_broker(connections)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child
            .child
            .try_wait()
            .context("could not inspect broker child")?
        {
            bail!("broker failed to start ({status}); check system logs for xtcc");
        }
        if Instant::now() >= deadline {
            bail!("broker readiness timed out");
        }
        match client::request(Operation::Attach, body.clone()) {
            Ok(reply) => {
                let reply = require_healthy(reply)?;
                child.ready = true;
                return Ok(reply);
            }
            Err(error) if client::exit_code(&error) == client::EX_UNAVAILABLE => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_fork::rusty_fork_test;

    #[test]
    fn health_requires_complete_fresh_counts() {
        let good = json!({"ok":true,"helper_pid":1,"connected":2,"expected":2,"acknowledged":2});
        assert!(healthy(&good));
        for change in [
            json!({"ok":false}),
            json!({"connected":0}),
            json!({"expected":3}),
            json!({"acknowledged":1}),
            json!({"helper_pid":0}),
        ] {
            let mut value = good.clone();
            for (key, value_change) in change.as_object().unwrap() {
                value[key] = value_change.clone();
            }
            assert!(!healthy(&value));
        }
    }
    #[test]
    fn readiness_parser_rejects_ambiguous_or_invalid_reports() {
        let good = "LLDB output\nINJECT_RESULT {\"pid\":12,\"receive\":34}\n";
        assert_eq!(parse_injection_report(12, true, good).unwrap().receive, 34);
        assert!(parse_injection_report(12, false, good).is_err());
        assert!(parse_injection_report(12, true, &(good.to_string() + good)).is_err());
        assert!(parse_injection_report(
            12,
            true,
            &(good.to_string() + "INJECT_ERROR detach: failed\n")
        )
        .is_err());
        for bad in [
            "",
            "INJECT_RESULT {}",
            "INJECT_RESULT {\"pid\":13,\"receive\":34}",
            "INJECT_RESULT {\"pid\":12,\"receive\":0}",
            "INJECT_RESULT {\"pid\":12,\"receive\":4294967295}",
            "INJECT_RESULT {\"pid\":12,\"receive\":1.5}",
        ] {
            assert!(parse_injection_report(12, true, bad).is_err());
        }
    }
    #[test]
    fn lldb_program_quotes_paths_without_shell_interpolation() {
        let path = Path::new("/tmp/quote'\"\n\\name.dylib");
        let program = lldb_program(12, path).unwrap();
        assert!(program.starts_with("pid=12\nlibrary=\"/tmp/quote'\\\"\\n\\\\name.dylib\"\n"));
        assert!(program.contains("cyan_tccd_inject_ready"));
        assert!(program.contains("process.Detach()"));
        assert!(lldb_program(0, path).is_err());
    }
    rusty_fork_test! {
        #[test]
        fn embedded_libraries_are_valid_macho_and_materialize_idempotently() {
            use std::os::unix::fs::MetadataExt;

            // SAFETY: This test runs in a separate process; umask changes only that process's mask.
            unsafe { libc::umask(0o077) };
            const MACHO_64_MAGIC: &[u8] = &[0xCF, 0xFA, 0xED, 0xFE];
            let temporary = tempfile::tempdir().unwrap();
            let directory = temporary.path().join("runtime");
            for library in [NativeLibrary::TccInject, NativeLibrary::NetworkExtension] {
                assert!(library.bytes().starts_with(MACHO_64_MAGIC));
                let first = library.materialize_in(&directory).unwrap();
                let metadata = fs::metadata(&first).unwrap();
                assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
                assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o755);

                fs::set_permissions(&first, fs::Permissions::from_mode(0o700)).unwrap();
                let second = library.materialize_in(&directory).unwrap();
                assert_eq!(first, second);
                assert_eq!(fs::metadata(&second).unwrap().ino(), metadata.ino());
                assert_eq!(fs::metadata(&second).unwrap().permissions().mode() & 0o777, 0o755);
                assert_eq!(fs::read(&second).unwrap(), library.bytes());

                fs::write(&first, b"corrupted").unwrap();
                let repaired = library.materialize_in(&directory).unwrap();
                assert_eq!(fs::read(&repaired).unwrap(), library.bytes());
                assert_eq!(fs::metadata(&repaired).unwrap().permissions().mode() & 0o777, 0o755);
            }
        }
    }
}
