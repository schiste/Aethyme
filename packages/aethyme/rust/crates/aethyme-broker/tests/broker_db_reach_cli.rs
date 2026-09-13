//! What a broker command is allowed to reach on the filesystem (#163).
//!
//! Broker state is per-repository and `main_root()` resolves it from the git
//! common directory, so from a worktree it lands in the *main* checkout. That
//! is right for the product. It is wrong for a test binary, whose working
//! directory is its crate directory inside a real checkout: the post-command
//! metric hook resolved the developer's live database through that path and
//! migrated it, so `cargo test --workspace` on a branch that added a migration
//! bricked every installed binary on the machine before the branch merged.
//!
//! Two guarantees, one per test: a metric never brings broker state into
//! existence, and a harness can pin the database somewhere it owns.

use std::path::Path;
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    repo
}

/// A command that fails before it ever wants a database still records a metric
/// on the way out. That metric must not be what creates the database.
///
/// The assertion is deliberately about the *file*, not about the metric: the
/// harm in #163 was a side effect on storage nobody asked to touch, and a test
/// that checked "no row was written" would have passed on the broken code,
/// which wrote its row into the developer's migrated database quite happily.
#[test]
fn a_metric_does_not_create_the_database_it_wants_to_write_to() {
    let repo = fixture();
    let database = repo.path().join(aethyme_broker::BROKER_DB_RELPATH);

    let out = Command::new(CLI)
        .args(["cleanup", "--not-a-flag"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "the command itself must have failed");

    assert!(
        !database.exists(),
        "a failing command's metric created {}",
        database.display()
    );
}

/// The override exists so a harness can name the file it is willing to lose.
#[test]
fn the_database_override_moves_state_out_of_the_repository() {
    let repo = fixture();
    let elsewhere = tempfile::tempdir().unwrap();
    let pinned = elsewhere.path().join("pinned.db");

    let out = Command::new(CLI)
        .args(["status", "--json"])
        .current_dir(repo.path())
        .env(aethyme_broker::BROKER_DB_ENV, &pinned)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(pinned.is_file(), "the pinned database was never created");
    assert!(
        !repo.path().join(aethyme_broker::BROKER_DB_RELPATH).exists(),
        "state landed in the repository despite the override"
    );
}
