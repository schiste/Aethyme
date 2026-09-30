use std::path::PathBuf;
use std::process::Command;

/// Run the broker CLI without reaching the developer's live Chau7 instance.
/// Existing lifecycle fixtures get a stable test label unless a case is
/// explicitly testing the short-name contract.
pub fn test_session_args(args: &[&str]) -> Vec<String> {
    let mut args = args.to_vec();
    if matches!(args.first(), Some(&"start" | &"adopt" | &"start-agent"))
        && args.contains(&"--task")
        && !args.contains(&"--short-name")
    {
        args.extend(["--short-name", "test session"]);
    }
    args.into_iter().map(str::to_string).collect()
}

pub fn disabled_bridge_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/no-chau7-bridge")
}

#[allow(dead_code)]
pub fn broker_cli(binary: &str, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command
        .args(test_session_args(args))
        .env("AETHYME_CHAU7_MCP_BRIDGE", disabled_bridge_path());
    command
}
