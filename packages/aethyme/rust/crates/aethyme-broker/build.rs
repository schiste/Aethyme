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

    emit_source_identity_reruns();
}

/// Re-run this script whenever the checkout's commit can have moved.
///
/// `HEAD` alone is not enough. On a branch it holds the fixed text
/// `ref: refs/heads/<name>` and does not change when that branch advances, so a
/// pull, fast-forward or local commit leaves stale provenance baked into the
/// binary while the code itself is rebuilt (#274). The commit lives in the ref
/// file, or in `packed-refs` once packed.
///
/// The two live in different places in a linked worktree: `HEAD` is
/// per-worktree, under `--absolute-git-dir`, while refs are shared, under
/// `--git-common-dir`. Watching the ref under the worktree's own git dir would
/// name a path that never exists.
///
/// Naming a path that does not exist is fine: cargo re-runs the script if it
/// later appears, which is what should happen when a loose ref is packed away
/// or unpacked.
fn emit_source_identity_reruns() {
    if let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) {
        // Branch switches, and every commit while detached.
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
    let Some(common_dir) = git_output(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return;
    };
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        // The branch advancing underneath a stationary HEAD.
        println!("cargo:rerun-if-changed={common_dir}/{reference}");
    }
    // The same commit arriving via a packed ref, where no loose file is written.
    println!("cargo:rerun-if-changed={common_dir}/packed-refs");
}
