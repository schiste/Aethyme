//! `finish cleanup resolve <id> --archive`: preserve a closed worktree cleanup
//! refuses, verify the archive, then remove the worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, FinishOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

/// A closed session whose worktree holds one unpromoted commit, staged and
/// unstaged edits, untracked files (text, binary, a symlink) and ignored build
/// output.
fn dirty_closed_session(repo: &Path) -> (String, PathBuf) {
    let mut broker = Broker::open(repo).unwrap();
    let session = broker.start_worktree("resolve fixture", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("feature.txt"), "v1\n").unwrap();
    git(&worktree, &["add", "feature.txt"]);
    git(&worktree, &["commit", "-qm", "feature v1"]);
    std::fs::write(worktree.join("README.md"), "fixture\nstaged\n").unwrap();
    git(&worktree, &["add", "README.md"]);
    std::fs::write(worktree.join("README.md"), "fixture\nstaged\nunstaged\n").unwrap();
    std::fs::write(worktree.join("feature.txt"), "v1\nunstaged\n").unwrap();
    std::fs::create_dir_all(worktree.join("notes")).unwrap();
    std::fs::write(worktree.join("notes/todo.txt"), "todo\n").unwrap();
    std::fs::write(worktree.join("data.bin"), [0_u8, 255, 1, 7]).unwrap();
    std::os::unix::fs::symlink("notes/todo.txt", worktree.join("link")).unwrap();
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(worktree.join("target/debug/cache.bin"), vec![9_u8; 4096]).unwrap();
    broker.close(session.id).unwrap();
    (session.id.to_string(), worktree)
}

fn plan(repo: &Path, id: &str) -> serde_json::Value {
    json(&run(
        repo,
        &["finish", "cleanup", "resolve", id, "--archive", "--json"],
    ))
}

fn apply(repo: &Path, id: &str, digest: &str) -> Output {
    run(
        repo,
        &[
            "finish",
            "cleanup",
            "resolve",
            id,
            "--archive",
            "--confirm",
            digest,
            "--json",
        ],
    )
}

fn archive_entries(repo: &Path) -> Vec<PathBuf> {
    let root = repo.join(".aethyme/recovery-archives");
    match std::fs::read_dir(&root) {
        Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn a_dirty_worktree_is_archived_restored_and_then_removed() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    // The ignored build output must not reach the archive, whatever the
    // close-time sweep did with it.
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(worktree.join("target/debug/cache.bin"), vec![9_u8; 4096]).unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["disposition"], "dirty");
    assert_eq!(reviewed["unlanded_commits"].as_array().unwrap().len(), 1);
    let untracked: Vec<&str> = reviewed["untracked_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(untracked, ["data.bin", "link", "notes/todo.txt"]);
    let digest = reviewed["digest"].as_str().unwrap();

    let outcome = json(&apply(tmp.path(), &id, digest));
    assert_eq!(outcome["worktree_removed"], true, "{outcome}");
    assert!(
        !worktree.exists(),
        "the worktree is removed after archiving"
    );
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
    assert_eq!(
        archive_entries(tmp.path())
            .iter()
            .map(|entry| canonical(entry))
            .collect::<Vec<_>>(),
        vec![canonical(&archive)],
        "one finished archive, and no partial one"
    );
    assert!(!archive.join("untracked/target").exists());
    for part in ["manifest.json", "README.md", "commits.bundle"] {
        assert!(archive.join(part).is_file(), "{part} missing");
    }

    // Restore into a fresh clone, following the archive's README.
    let restored = tempfile::tempdir().unwrap();
    git(
        restored.path(),
        &["clone", "-q", &tmp.path().to_string_lossy(), "."],
    );
    let bundle = archive.join("commits.bundle");
    git(
        restored.path(),
        &["fetch", "-q", &bundle.to_string_lossy(), "HEAD"],
    );
    git(restored.path(), &["switch", "-q", "--detach", "FETCH_HEAD"]);
    assert_eq!(
        git(restored.path(), &["rev-parse", "HEAD"]),
        reviewed["head"].as_str().unwrap()
    );
    git(
        restored.path(),
        &[
            "apply",
            "--index",
            &archive.join("staged.patch").to_string_lossy(),
        ],
    );
    git(
        restored.path(),
        &["apply", &archive.join("unstaged.patch").to_string_lossy()],
    );
    let copied = Command::new("cp")
        .arg("-R")
        .arg(archive.join("untracked/."))
        .arg(".")
        .current_dir(restored.path())
        .status()
        .unwrap();
    assert!(copied.success());

    let read = |path: &str| std::fs::read(restored.path().join(path)).unwrap();
    assert_eq!(read("README.md"), b"fixture\nstaged\nunstaged\n");
    assert_eq!(read("feature.txt"), b"v1\nunstaged\n");
    assert_eq!(read("notes/todo.txt"), b"todo\n");
    assert_eq!(read("data.bin"), [0_u8, 255, 1, 7]);
    assert_eq!(
        std::fs::read_link(restored.path().join("link")).unwrap(),
        PathBuf::from("notes/todo.txt")
    );
    assert_eq!(
        git(restored.path(), &["diff", "--cached", "--name-only"]),
        "README.md",
        "the staged/unstaged split survives the round trip"
    );
}

#[test]
fn pending_commits_are_carried_by_a_verified_bundle() {
    let tmp = repo();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("pending fixture", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("one.txt"), "1\n").unwrap();
    git(&worktree, &["add", "one.txt"]);
    git(&worktree, &["commit", "-qm", "one"]);
    std::fs::write(worktree.join("two.txt"), "2\n").unwrap();
    git(&worktree, &["add", "two.txt"]);
    git(&worktree, &["commit", "-qm", "two"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    broker.close(session.id).unwrap();
    drop(broker);
    let id = session.id.to_string();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["disposition"], "pending_commits");
    assert_eq!(reviewed["unlanded_commits"].as_array().unwrap().len(), 2);
    assert_eq!(reviewed["staged_patch_bytes"], 0);
    assert_eq!(reviewed["unstaged_patch_bytes"], 0);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);

    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    let bundle = archive
        .join("commits.bundle")
        .to_string_lossy()
        .into_owned();
    git(tmp.path(), &["bundle", "verify", "-q", &bundle]);
    assert!(
        git(tmp.path(), &["bundle", "list-heads", &bundle]).starts_with(&head),
        "the bundle's tip is the session head"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(archive.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["head"], head);
    assert_eq!(manifest["bundle"], "commits.bundle");
    assert_eq!(manifest["landing"]["state"], "not_landed");

    let events = run(
        tmp.path(),
        &[
            "advanced",
            "events",
            "--kind",
            "broker.cleanup.archived",
            "--json",
        ],
    );
    let stderr = String::from_utf8_lossy(&events.stderr).into_owned();
    let events = String::from_utf8_lossy(&events.stdout);
    assert!(
        events.contains(&archive.to_string_lossy().into_owned()),
        "the archive path is recorded in an event: {events}{stderr}"
    );
}

#[test]
fn a_stale_digest_is_refused_and_nothing_moves() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    let reviewed = plan(tmp.path(), &id);
    std::fs::write(worktree.join("notes/todo.txt"), "changed after review\n").unwrap();

    let refused = apply(tmp.path(), &id, reviewed["digest"].as_str().unwrap());
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("no longer matches"), "{stderr}");
    assert!(worktree.join("notes/todo.txt").is_file());
    assert!(
        archive_entries(tmp.path()).is_empty(),
        "nothing was archived"
    );
}

/// Any failure between the confirmed plan and removal keeps the worktree.
#[test]
fn a_failed_archive_leaves_the_worktree_in_place() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    let reviewed = plan(tmp.path(), &id);
    // A file where the archive root's directory must go.
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/recovery-archives"),
        "not a directory",
    )
    .unwrap();

    let refused = apply(tmp.path(), &id, reviewed["digest"].as_str().unwrap());
    assert!(!refused.status.success());
    assert!(worktree.join("notes/todo.txt").is_file());
    assert!(worktree.join("feature.txt").is_file());
}

#[test]
fn live_eligible_and_unmarked_requests_are_refused() {
    let tmp = repo();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let live = broker.start_worktree("live fixture", None).unwrap();

    let landed = broker.start_worktree("landed fixture", None).unwrap();
    let landed_path = PathBuf::from(&landed.worktree_path);
    std::fs::write(landed_path.join("done.txt"), "done\n").unwrap();
    git(&landed_path, &["add", "done.txt"]);
    git(&landed_path, &["commit", "-qm", "done"]);
    assert!(broker.submit(landed.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                landed.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    drop(broker);

    let refused = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "resolve",
            &live.id.to_string(),
            "--archive",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("close it first"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let refused = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "resolve",
            &landed.id.to_string(),
            "--archive",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("can already remove it"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let refused = run(
        tmp.path(),
        &["finish", "cleanup", "resolve", &landed.id.to_string()],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("requires --archive"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
