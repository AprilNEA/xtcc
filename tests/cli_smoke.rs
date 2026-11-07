//! Exercise only help, version, and invalid commands; never invoke control operations.
use std::process::Command;

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
