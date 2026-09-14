//! Build script: capture the source identity and build time so
//! `aethyme --version` can identify the exact binary an operator is running.
//! Best-effort by design — building without git or `date` still produces a
//! useful version banner with `unknown` metadata.

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

fn build_date() -> String {
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match out {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
        _ => "unknown".to_string(),
    }
}

fn main() {
    let describe = git_output(&["describe", "--tags", "--always", "--dirty"]).unwrap_or_default();
    let commit = git_output(&["rev-parse", "HEAD"]).unwrap_or_default();
    let build_date = build_date();
    println!("cargo:rustc-env=AETHYME_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=AETHYME_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=AETHYME_BUILD_DATE={build_date}");

    // Re-run when HEAD moves so the source identity tracks the checkout.
    // `--git-dir` resolves correctly from worktrees (where `.git` is a file).
    if let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}
