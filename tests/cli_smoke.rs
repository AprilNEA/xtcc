//! 仅运行帮助、版本与未知命令；不调用实际控制操作。
use std::process::Command;

fn output(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xtcc"))
        .args(arguments)
        .env("NO_COLOR", "1")
        .output()
        .expect("应能启动已构建的 xtcc")
}

#[test]
fn help_lists_public_commands() {
    let result = output(&["--help"]);
    assert!(result.status.success());
    let text = String::from_utf8_lossy(&result.stdout);
    for command in ["start", "stop", "status", "logs", "doctor"] {
        assert!(text.contains(command), "帮助缺少命令 {command}: {text}");
    }
    assert!(!text.contains("_serve"), "内部命令不应出现在公共帮助中");
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
    for command in ["start", "logs", "doctor"] {
        let result = output(&[command, "--help"]);
        assert!(result.status.success(), "{command} --help 失败");
    }
}
