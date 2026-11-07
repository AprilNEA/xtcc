//! Exercise parsing, diagnostics, and isolated builds; never invoke control operations.
use std::fs;
use std::process::Command;
use xtcc::bootstrap::NativeLibrary;

fn output(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xtcc"))
        .args(arguments)
        .env("NO_COLOR", "1")
        .output()
        .expect("the built xtcc executable should start")
}

#[test]
fn help_lists_public_commands() {
    let result = output(&["--help"]);
    assert!(result.status.success());
    let text = String::from_utf8_lossy(&result.stdout);
    for command in ["start", "stop", "status", "logs", "doctor", "ne"] {
        assert!(
            text.contains(command),
            "help is missing command {command}: {text}"
        );
    }
    assert!(
        !text.contains("_serve"),
        "internal commands must not appear in public help"
    );
}

#[test]
fn version_uses_new_project_name() {
    let result = output(&["--version"]);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).starts_with("xtcc "));
}

#[test]
fn unknown_command_fails_without_runtime_actions() {
    let result = output(&["not-a-real-command"]);
    assert!(!result.status.success());
    assert!(!result.stderr.is_empty());
}

#[test]
fn nested_help_is_available_without_root() {
    for command in ["start", "logs", "doctor", "ne"] {
        let result = output(&[command, "--help"]);
        assert!(result.status.success(), "{command} --help failed");
    }
}

#[test]
fn public_help_and_errors_use_english_text() {
    let mut cases = vec![
        vec!["--help"],
        vec!["--version"],
        vec!["not-a-real-command"],
    ];
    for command in [
        "build", "start", "enable", "stop", "disable", "status", "logs", "doctor", "ne",
    ] {
        cases.push(vec![command, "--help"]);
    }
    for command in [
        "list",
        "approve",
        "filter",
        "dns-proxy",
        "vpn",
        "connect",
        "disconnect",
        "status",
        "enable",
        "disable",
        "remove",
    ] {
        cases.push(vec!["ne", command, "--help"]);
    }
    for args in cases {
        let result = output(&args);
        assert!(result.stdout.is_ascii(), "non-English stdout for {args:?}");
        assert!(result.stderr.is_ascii(), "non-English stderr for {args:?}");
        assert!(!result.stdout.is_empty() || !result.stderr.is_empty());
    }
    let help = output(&["--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("Usage:"));
    assert!(text.contains("Commands:"));
    assert!(text.contains("Options:"));
}

#[test]
fn network_extension_arguments_fail_before_runtime_operations() {
    for arguments in [
        vec!["ne", "list", "--timeout", "0", "--json"],
        vec!["ne", "list", "--timeout", "301", "--json"],
        vec!["ne", "approve", "--json"],
        vec!["ne", "filter", "--json"],
        vec!["ne", "dns-proxy", "--json"],
        vec!["ne", "vpn", "com.example.provider", "--json"],
        vec![
            "ne",
            "vpn",
            "com.example.provider",
            "--server-address",
            "",
            "--json",
        ],
        vec![
            "ne",
            "dns-proxy",
            "com.example.provider",
            "--server-address",
            "example.com",
            "--json",
        ],
        vec!["ne", "connect", "--json"],
        vec!["ne", "disconnect", "--json"],
        vec!["ne", "status", "--json"],
        vec!["ne", "enable", "--json"],
        vec!["ne", "disable", "--json"],
        vec!["ne", "remove", "--json"],
        vec!["ne", "list", "--all", "--json"],
    ] {
        let result = output(&arguments);
        assert_eq!(result.status.code(), Some(64), "{arguments:?}");
        let record: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(record["ok"], false);
        let error = record["error"].as_str().unwrap();
        assert!(!error.contains("root_required"), "{arguments:?}: {error}");
        assert!(!error.contains("NE client"), "{arguments:?}: {error}");
    }
}

#[test]
fn doctor_reports_embedded_libraries_without_external_dylibs() {
    let home = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_xtcc"))
        .args(["doctor", "--json"])
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
    let record: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(record["ok"], true);
    for name in ["xcrun", "lldb"] {
        assert!(record["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name && tool["found"] == true));
    }
    for key in ["library", "ne_library"] {
        assert_eq!(record["files"][key]["embedded"], true, "{key}");
        assert!(
            record["files"][key]["bytes"].as_u64().unwrap_or(0) > 0,
            "{key}"
        );
    }
}

#[test]
fn forced_build_regenerates_embedded_native_libraries() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("target");
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtcc"));
    command
        .args(["build", "--json"])
        .env("CARGO", env!("CARGO"))
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_NET_OFFLINE", "true");
    let initial = command.output().unwrap();
    assert!(initial.status.success(), "{initial:?}");

    let libraries = [
        ("library", NativeLibrary::TccInject),
        ("ne_library", NativeLibrary::NetworkExtension),
    ];
    let native_output = fs::read_dir(target.join("release/build"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("out"))
        .find(|directory| {
            libraries
                .iter()
                .all(|(_, library)| directory.join(library.file_name()).is_file())
        })
        .expect("the build must produce both native libraries");
    for (_, library) in libraries {
        fs::write(
            native_output.join(library.file_name()),
            b"outdated native payload",
        )
        .unwrap();
    }

    let rebuilt = command.arg("--force").output().unwrap();
    assert!(rebuilt.status.success(), "{rebuilt:?}");
    let doctor = Command::new(target.join("release/xtcc"))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{doctor:?}");
    let record: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    for (key, library) in libraries {
        let bytes = fs::read(native_output.join(library.file_name())).unwrap();
        assert!(
            bytes.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]),
            "{key} was not rebuilt"
        );
        assert_eq!(record["files"][key]["bytes"], bytes.len());
    }
}
