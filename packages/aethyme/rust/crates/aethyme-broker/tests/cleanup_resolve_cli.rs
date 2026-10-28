//! `finish cleanup resolve <id> --archive`: preserve a closed worktree cleanup
//! refuses, verify the archive, then remove the worktree.

#[path = "common/isolation.rs"]
mod isolation;

use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::FinishOptions;

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
    isolation::command(CLI, repo, args).output().unwrap()
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

fn repo() -> isolation::FixtureRepo {
    let tmp = isolation::FixtureRepo::new();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        tmp.path().join(".gitignore"),
        "/.aethyme/\n/target/\n/private.txt\n/private-link\n/opaque-cache/\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

/// A closed session whose worktree holds one unpromoted commit, staged and
/// unstaged edits, untracked files (text, binary, a symlink) and ignored build
/// output.
fn dirty_closed_session(repo: &Path) -> (String, PathBuf) {
    let mut broker = isolation::open(repo);
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

fn closed_session(repo: &Path, task: &str) -> (String, PathBuf) {
    let mut broker = isolation::open(repo);
    let session = broker.start_worktree(task, None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
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

fn refused_plan(repo: &Path, id: &str) -> String {
    let refused = run(
        repo,
        &["finish", "cleanup", "resolve", id, "--archive", "--json"],
    );
    assert!(!refused.status.success());
    json_error::message(&refused)
}

fn archive_root(repo: &Path, plan: &serde_json::Value) -> PathBuf {
    let root = PathBuf::from(plan["archive_root"].as_str().unwrap());
    let owned = repo
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join("host-state/recovery-archives");
    assert_eq!(root.parent(), Some(owned.as_path()));
    root
}

fn archive_entries(root: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(root) {
        Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("cannot inspect archive root {}: {error}", root.display()),
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

    let human = run(
        tmp.path(),
        &["finish", "cleanup", "resolve", &id, "--archive"],
    );
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_text = String::from_utf8(human.stdout).unwrap();
    assert!(
        human_text.contains(
            "committed work: landing not evaluated; recovery preserves target-absent commits"
        ),
        "{human_text}"
    );
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
        archive_entries(&archive_root(tmp.path(), &reviewed))
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
fn target_absent_commits_are_carried_by_a_verified_bundle() {
    let tmp = repo();
    let mut broker = isolation::open(tmp.path());
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
    assert_eq!(reviewed["disposition"], "unproven_provenance");
    assert_eq!(reviewed["landing"]["state"], "not_evaluated_for_recovery");
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
    assert_eq!(manifest["landing"]["state"], "not_evaluated_for_recovery");

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
fn untracked_newline_path_is_archived_without_truncating_the_inventory() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "newline path fixture");
    let relative = "untracked-line\nbreak.txt";
    std::fs::write(worktree.join(relative), b"preserve newline path\n").unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["untracked_files"].as_array().unwrap().len(), 1);
    assert_eq!(reviewed["untracked_files"][0]["path"], relative);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::read(archive.join("untracked").join(relative)).unwrap(),
        b"preserve newline path\n"
    );
}

#[test]
fn non_utf8_untracked_path_refuses_archive_resolution_before_cleanup() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "non-utf8 path fixture");
    let name = std::ffi::OsString::from_vec(b"untracked-\xff.txt".to_vec());
    let path = worktree.join(name);
    std::fs::write(&path, b"must remain").unwrap();

    let error = refused_plan(tmp.path(), &id);
    assert!(
        error.contains("non-UTF-8 NUL-separated porcelain status"),
        "{error}"
    );
    assert!(
        path.is_file(),
        "the unrepresentable file remains in the worktree"
    );
    let archive_root = tmp
        .path()
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join("host-state/recovery-archives");
    assert!(
        archive_entries(&archive_root).is_empty(),
        "no archive was created"
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
    let stderr = json_error::message(&refused);
    assert!(stderr.contains("no longer matches"), "{stderr}");
    assert!(worktree.join("notes/todo.txt").is_file());
    assert!(
        archive_entries(&archive_root(tmp.path(), &reviewed)).is_empty(),
        "nothing was archived"
    );
}

#[cfg(unix)]
#[test]
fn a_permission_mode_change_after_review_invalidates_the_plan() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "mode-change fixture");
    let executable = worktree.join("tool.sh");
    std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o751)).unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["untracked_files"][0]["mode"], 0o751);
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();

    let refused = apply(tmp.path(), &id, reviewed["digest"].as_str().unwrap());
    assert!(!refused.status.success());
    assert!(json_error::message(&refused).contains("digest"));
    assert!(worktree.is_dir());
    assert!(executable.is_file());
    assert!(archive_entries(&archive_root(tmp.path(), &reviewed)).is_empty());
}

#[cfg(unix)]
#[test]
fn an_untracked_executable_mode_survives_archive_and_documented_restore() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "executable restore fixture");
    let executable = worktree.join("tool.sh");
    std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o751)).unwrap();
    let reviewed = plan(tmp.path(), &id);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::metadata(archive.join("untracked/tool.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );

    let restored = tempfile::tempdir().unwrap();
    git(
        restored.path(),
        &["clone", "-q", &tmp.path().to_string_lossy(), "."],
    );
    let copied = Command::new("cp")
        .args(["-pR"])
        .arg(archive.join("untracked/."))
        .arg(".")
        .current_dir(restored.path())
        .status()
        .unwrap();
    assert!(copied.success());
    assert_eq!(
        std::fs::metadata(restored.path().join("tool.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
}

#[test]
fn protected_ignored_file_alone_is_archived_before_removal() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "ignored-only fixture");
    let protected = worktree.join("private.txt");
    std::fs::write(&protected, "keep this file\n").unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["ignored_files"].as_array().unwrap().len(), 1);
    assert_eq!(reviewed["ignored_files"][0]["path"], "private.txt");
    assert_eq!(reviewed["ignored_bytes"], b"keep this file\n".len() as u64);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);
    assert!(!worktree.exists());
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::read(archive.join("ignored/private.txt")).unwrap(),
        b"keep this file\n"
    );
}

#[test]
fn protected_ignored_file_is_archived_alongside_dirty_work() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    let protected = worktree.join("private.txt");
    std::fs::write(&protected, "keep alongside the patch\n").unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["ignored_files"].as_array().unwrap().len(), 1);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::read(archive.join("ignored/private.txt")).unwrap(),
        b"keep alongside the patch\n"
    );
}

#[cfg(unix)]
#[test]
fn protected_ignored_symlink_is_archived_without_following_its_target() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "ignored symlink fixture");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "outside data\n").unwrap();
    let link = worktree.join("private-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["ignored_files"].as_array().unwrap().len(), 1);
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::read_link(archive.join("ignored/private-link")).unwrap(),
        target
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"outside data\n");
}

#[test]
fn strict_cleanup_policy_refuses_ignored_directory_during_archive_resolution() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "strict ignored fixture");
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[cleanup]\nstrict = true\n",
    )
    .unwrap();
    let ignored = worktree.join("opaque-cache");
    std::fs::create_dir_all(&ignored).unwrap();
    std::fs::write(ignored.join("private.bin"), b"retain under strict policy").unwrap();

    let error = refused_plan(tmp.path(), &id);
    assert!(
        error.contains("strict cleanup requires no ignored leftovers"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(ignored.join("private.bin")).unwrap(),
        b"retain under strict policy"
    );
    assert!(worktree.is_dir());
}

#[cfg(unix)]
#[test]
fn default_opaque_ignored_directory_is_removed_without_following_nested_symlinks() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    let opaque = worktree.join("opaque-cache");
    std::fs::create_dir_all(&opaque).unwrap();
    std::fs::write(opaque.join("nested.bin"), b"not recursively inspected").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "untouched\n").unwrap();
    std::os::unix::fs::symlink(&target, opaque.join("external-link")).unwrap();

    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["disposition"], "dirty");
    let untracked: Vec<&str> = reviewed["untracked_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(untracked, ["data.bin", "link", "notes/todo.txt"]);
    assert!(opaque.join("nested.bin").is_file());

    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true, "{outcome}");
    assert!(!worktree.exists());
    assert_eq!(std::fs::read(target).unwrap(), b"untouched\n");
}

/// Any failure between the confirmed plan and removal keeps the worktree.
#[test]
fn a_failed_archive_leaves_the_worktree_in_place() {
    let tmp = repo();
    let (id, worktree) = dirty_closed_session(tmp.path());
    let reviewed = plan(tmp.path(), &id);
    // A file where the archive root's directory must go.
    let root = archive_root(tmp.path(), &reviewed);
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    std::fs::write(root, "not a directory").unwrap();

    let refused = apply(tmp.path(), &id, reviewed["digest"].as_str().unwrap());
    assert!(!refused.status.success());
    assert!(worktree.join("notes/todo.txt").is_file());
    assert!(worktree.join("feature.txt").is_file());
}

#[test]
fn squash_landed_content_is_recoverable_without_a_landing_claim() {
    let tmp = repo();
    let mut broker = isolation::open(tmp.path());
    let session = broker
        .start_worktree("squash recovery fixture", None)
        .unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "session work"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    git(tmp.path(), &["merge", "--squash", &session.branch]);
    git(tmp.path(), &["commit", "-qm", "squash delivery"]);
    assert_ne!(git(tmp.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(tmp.path(), &["rev-parse", "HEAD^{tree}"]),
        git(&worktree, &["rev-parse", "HEAD^{tree}"])
    );
    broker.close(session.id).unwrap();
    drop(broker);
    let id = session.id.to_string();
    let reviewed = plan(tmp.path(), &id);
    assert_eq!(reviewed["disposition"], "unproven_provenance");
    assert_eq!(reviewed["landing"]["state"], "not_evaluated_for_recovery");
    assert_eq!(reviewed["unlanded_commits"], serde_json::json!([head]));
    let outcome = json(&apply(
        tmp.path(),
        &id,
        reviewed["digest"].as_str().unwrap(),
    ));
    assert_eq!(outcome["worktree_removed"], true);
    assert!(!worktree.exists());
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    let bundle = archive
        .join("commits.bundle")
        .to_string_lossy()
        .into_owned();
    git(tmp.path(), &["bundle", "verify", "-q", &bundle]);
    assert!(git(tmp.path(), &["bundle", "list-heads", &bundle]).starts_with(&head));
}

#[test]
fn live_eligible_and_unmarked_requests_are_refused() {
    let tmp = repo();
    let mut broker = isolation::open(tmp.path());
    let live = broker.start_worktree("live fixture", None).unwrap();

    let landed = broker.start_worktree("landed fixture", None).unwrap();
    let landed_path = PathBuf::from(&landed.worktree_path);
    std::fs::write(landed_path.join("done.txt"), "done\n").unwrap();
    git(&landed_path, &["add", "done.txt"]);
    git(&landed_path, &["commit", "-qm", "done"]);
    // Recovery's cheap eligibility check proves ancestry, not replayed or
    // squash delivery. Keep this refusal fixture ancestry-represented.
    git(tmp.path(), &["merge", "--ff-only", &landed.branch]);
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

#[path = "common/json_error.rs"]
mod json_error;
