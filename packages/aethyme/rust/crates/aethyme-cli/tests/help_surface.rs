use std::process::Command;

#[test]
fn top_level_help_names_the_normal_broker_lifecycle() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("--help")
        .output()
        .expect("run aethyme --help");
    assert!(output.status.success());

    // Explicitly requested help is the command's output: stdout, exit 0.
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    for command in [
        "broker start --task <text>",
        "broker submit --session <id>",
        "broker status",
        "broker finish --session <id>",
        "broker unblock",
        "broker gc plan|apply",
        "broker advanced <verb>",
    ] {
        assert!(help.contains(command), "help omitted {command:?}\n{help}");
    }
}

#[test]
fn version_banner_exposes_build_commit_and_date() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("--version")
        .output()
        .expect("run aethyme --version");
    assert!(output.status.success());

    let version = String::from_utf8(output.stdout).expect("UTF-8 version");
    let version = version.trim();
    let commit = version
        .split_once("build_commit=")
        .and_then(|(_, value)| value.split(')').next())
        .expect("version must expose build_commit");
    assert_eq!(
        commit.len(),
        40,
        "build commit must be a full SHA: {version}"
    );
    assert!(
        commit.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "build commit must be hexadecimal: {version}"
    );
    assert!(
        version.contains("build_date=") && version.ends_with('Z'),
        "version must expose an ISO-8601 UTC build date: {version}"
    );
}
