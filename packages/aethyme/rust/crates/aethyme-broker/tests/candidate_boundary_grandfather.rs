//! The candidate boundary (#663) where submit would grandfather gate trust.
//!
//! A repository with gate history and no trust record: submit records trust
//! for what it already ran and proceeds. The adapter counts that as trusted
//! and records nothing. Its own binary: it removes the suite's trust escape
//! part-way, which is only sound with no other test in the process.

use std::path::Path;
use std::process::Command;

use aethyme_broker::Broker;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn a_policy_submit_would_grandfather_builds_a_candidate_and_records_nothing() {
    let host_state = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment concurrently.
    unsafe {
        std::env::set_var("AETHYME_HOST_STATE_DIR", host_state.path());
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Aethyme Test"]);
    git(
        root,
        &["config", "user.email", "aethyme-test@example.invalid"],
    );
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/gates.toml\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"marker\"\ncommand = \"true\"\ntriggers = [\"**\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let mut broker = Broker::open(root).unwrap();

    // Gate history, built under the suite's escape, with no trust record.
    let first = broker.start_worktree("first", None).unwrap();
    let first_path = Path::new(&first.worktree_path);
    std::fs::write(first_path.join("first.txt"), "first\n").unwrap();
    git(first_path, &["add", "-A"]);
    git(first_path, &["commit", "-qm", "first"]);
    let outcome = broker.submit(first.id).unwrap();
    assert!(outcome.promoted, "{outcome:?}");
    assert!(!outcome.gate_outcomes.is_empty(), "the gate ran");
    assert!(
        files(host_state.path())
            .iter()
            .all(|path| !path.to_string_lossy().contains("trust"))
    );

    // SAFETY: as above.
    unsafe {
        std::env::remove_var("AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS");
    }
    let session = broker.start_worktree("a", None).unwrap();
    let worktree = Path::new(&session.worktree_path);
    std::fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-qm", "a"]);

    let events = broker.store().events_after(0, i64::MAX).unwrap().len();
    let outcome = broker.legacy_candidate(session.id).unwrap();
    let candidate = outcome.candidate().expect("a candidate").clone();
    assert_eq!(
        broker.store().events_after(0, i64::MAX).unwrap().len(),
        events
    );
    let trust_files = |dir: &Path| {
        files(dir)
            .into_iter()
            .filter(|path| path.to_string_lossy().contains("trust"))
            .collect::<Vec<_>>()
    };
    assert!(trust_files(host_state.path()).is_empty());

    // Submit grandfathers, then gates and promotes the same tree.
    let submitted = broker.submit(session.id).unwrap();
    assert!(submitted.promoted, "{submitted:?}");
    assert_eq!(
        submitted.entry.merged_tree.as_deref(),
        Some(candidate.tree.as_str())
    );
    assert!(
        !trust_files(host_state.path()).is_empty(),
        "submit grandfathered"
    );
}
