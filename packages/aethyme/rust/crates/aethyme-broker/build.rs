//! Build script: capture the source checkout used to build the broker
//! library so `broker doctor` and `certify` can detect stale local CLI
//! installs against the Aethyme source repository.

use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn main() {
    let describe = git_output(&["describe", "--tags", "--always", "--dirty"]).unwrap_or_default();
    let commit = git_output(&["rev-parse", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=AETHYME_BROKER_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=AETHYME_BROKER_GIT_COMMIT={commit}");

    if let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}
