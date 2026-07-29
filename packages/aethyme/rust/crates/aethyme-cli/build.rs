//! Build script: capture `git describe` so `aethyme --version` can report
//! the exact source state alongside the crate version. Best-effort by
//! design — building outside a git checkout (or without git on PATH)
//! yields an empty string and the binary prints the crate version alone.

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
    println!("cargo:rustc-env=AETHYME_GIT_DESCRIBE={describe}");

    // Re-run when HEAD moves so the describe string tracks the checkout.
    // `--git-dir` resolves correctly from worktrees (where `.git` is a file).
    if let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}
