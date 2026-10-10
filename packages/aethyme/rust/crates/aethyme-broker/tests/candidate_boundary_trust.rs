//! The candidate boundary (#663) under a gate policy change nobody trusted.
//!
//! Its own test binary with one test: it removes the test-suite trust escape
//! that `.cargo/config.toml` exports and points host state at a temporary
//! directory, which is only sound while no other test shares the process.

use std::path::Path;
use std::process::Command;

use aethyme_broker::composition::{CompositionOutcome, Refusal};
use aethyme_broker::{Broker, BrokerOpError};

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

#[test]
fn an_untrusted_policy_on_the_baseline_is_refused_as_submit_refuses_it() {
    let host_state = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment concurrently.
    unsafe {
        std::env::remove_var("AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS");
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
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        root.join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/gates.toml\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let mut broker = Broker::open(root).unwrap();
    let session = broker.start_worktree("a", None).unwrap();
    let worktree = Path::new(&session.worktree_path);
    std::fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-qm", "a"]);

    // The baseline gains a policy that runs commands.
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"marker\"\ncommand = \"true\"\ntriggers = [\"**\"]\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "add a gate"]);

    let refs = git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let events = broker.store().events_after(0, i64::MAX).unwrap().len();
    let outcome = broker.legacy_candidate(session.id).unwrap();
    let CompositionOutcome::Refused { reason, detail } = &outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    assert_eq!(*reason, Refusal::UntrustedPolicy);
    assert!(detail.contains("trust"), "{detail}");
    assert!(outcome.candidate().is_none());
    assert_eq!(
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        refs
    );
    assert_eq!(
        broker.store().events_after(0, i64::MAX).unwrap().len(),
        events
    );
    // Assessing trust records none.
    let mut recorded = Vec::new();
    for entry in walk(host_state.path()) {
        if entry.to_string_lossy().contains("trust") {
            recorded.push(entry);
        }
    }
    assert!(recorded.is_empty(), "{recorded:?}");

    match broker.submit(session.id) {
        Err(BrokerOpError::GatePolicyUntrusted { .. }) => {}
        other => panic!("submit should refuse the untrusted policy: {other:?}"),
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}
