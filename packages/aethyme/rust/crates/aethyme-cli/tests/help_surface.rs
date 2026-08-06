use std::process::Command;

#[test]
fn top_level_help_names_the_normal_broker_lifecycle() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("--help")
        .output()
        .expect("run aethyme --help");
    assert!(output.status.success());

    let help = String::from_utf8(output.stderr).expect("UTF-8 help");
    for command in [
        "broker start --task <text>",
        "broker submit --session <id>",
        "broker status",
        "broker finish --session <id>",
        "broker leases [claim|release]",
    ] {
        assert!(help.contains(command), "help omitted {command:?}\n{help}");
    }
}
