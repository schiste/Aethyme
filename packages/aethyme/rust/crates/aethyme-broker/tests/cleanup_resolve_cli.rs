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

#[test]
fn recovery_batch_retires_closed_targets_without_removing_a_live_target() {
    let tmp = repo();
    let (first, first_path) = dirty_closed_session(tmp.path());
    let (second, second_path) = dirty_closed_session(tmp.path());
    let live = isolation::open(tmp.path())
        .start_worktree("live batch target", None)
        .unwrap();
    let live_id = live.id.to_string();
    let live_path = PathBuf::from(&live.worktree_path);
    let args = [
        "finish",
        "cleanup",
        "resolve",
        &first,
        &second,
        &live_id,
        "--batch",
        "--archive",
        "--json",
    ];
    let plan = json(&run(tmp.path(), &args));
    assert_eq!(plan["targets"].as_array().unwrap().len(), 3);
    let protected = plan["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["session_id"] == live.id)
        .unwrap();
    assert!(protected["confirmation"].is_null());
    assert!(
        protected["blocker"]
            .as_str()
            .unwrap()
            .contains("close it first")
    );
    assert!(first_path.exists() && second_path.exists() && live_path.exists());
    let digest = plan["digest"].as_str().unwrap();
    let mut apply_args = args.to_vec();
    apply_args.extend(["--confirm", digest]);
    let applied = run(tmp.path(), &apply_args);
    assert!(
        !applied.status.success(),
        "partial completion must have a nonzero status"
    );
    let outcome: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(outcome["removed_count"], 2);
    assert_eq!(
        outcome["remaining_session_ids"],
        serde_json::json!([live.id])
    );
    assert!(!first_path.exists() && !second_path.exists());
    assert!(live_path.exists());
    for result in outcome["results"].as_array().unwrap() {
        if result["session_id"] != live.id {
            assert!(result["outcome"]["worktree_removed"].as_bool().unwrap());
            let archive = Path::new(result["outcome"]["archive"].as_str().unwrap());
            assert!(archive.join("manifest.json").is_file());
            assert_eq!(
                std::fs::read(archive.join("untracked/data.bin")).unwrap(),
                [0, 255, 1, 7]
            );
        }
    }
}

#[test]
fn recovery_batch_rejects_changed_content_before_removing_any_target() {
    let tmp = repo();
    let (first, first_path) = dirty_closed_session(tmp.path());
    let (second, second_path) = dirty_closed_session(tmp.path());
    let args = [
        "finish",
        "cleanup",
        "resolve",
        &first,
        &second,
        "--batch",
        "--archive",
        "--json",
    ];
    let plan = json(&run(tmp.path(), &args));
    std::fs::write(second_path.join("data.bin"), b"changed after review").unwrap();
    let mut apply_args = args.to_vec();
    apply_args.extend(["--confirm", plan["digest"].as_str().unwrap()]);
    let rejected = run(tmp.path(), &apply_args);
    assert!(!rejected.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(diagnostic.contains("confirmation changed"), "{diagnostic}");
    assert!(first_path.exists() && second_path.exists());
    assert_eq!(
        std::fs::read(second_path.join("data.bin")).unwrap(),
        b"changed after review"
    );
}

#[test]
fn recovery_batch_can_compact_a_delivered_checkout_inside_grace() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "compact delivered fixture");
    let args = [
        "finish",
        "cleanup",
        "resolve",
        &id,
        "--batch",
        "--archive",
        "--json",
    ];
    let plan = json(&run(tmp.path(), &args));
    assert!(plan["targets"][0]["blocker"].is_null());
    assert!(plan["apply_command"].as_str().unwrap().contains("--batch"));
    assert!(worktree.exists());
    let mut apply_args = args.to_vec();
    apply_args.extend(["--confirm", plan["digest"].as_str().unwrap()]);
    let outcome = json(&run(tmp.path(), &apply_args));
    assert_eq!(outcome["removed_count"], 1);
    assert!(!worktree.exists());
    let archive = Path::new(
        outcome["results"][0]["outcome"]["archive"]
            .as_str()
            .unwrap(),
    );
    assert!(archive.join("manifest.json").is_file());
}

#[test]
fn recovery_batch_rejects_duplicate_ids_before_checkout_inspection() {
    let tmp = repo();
    let (id, worktree) = closed_session(tmp.path(), "duplicate scope fixture");
    let rejected = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "resolve",
            &id,
            &id,
            "--archive",
            "--json",
        ],
    );
    assert!(!rejected.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(diagnostic.contains("duplicate session ID"), "{diagnostic}");
    assert!(worktree.exists());
}

#[test]
fn retirement_of_an_adopted_checkout_requires_opt_in_and_keeps_its_branch() {
    let tmp = repo();
    let worktree = tmp.path().parent().unwrap().join("operator-adopted");
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "operator-owned",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = isolation::open(tmp.path());
    let session = broker
        .adopt(&worktree, Some("explicit adopted recovery"))
        .unwrap();
    std::fs::write(worktree.join("recovery-note.txt"), b"preserve this note\n").unwrap();
    broker.close(session.id).unwrap();
    drop(broker);
    let id = session.id.to_string();
    let args = [
        "finish",
        "cleanup",
        "resolve",
        &id,
        "--retire",
        "--archive",
        "--json",
    ];
    assert!(!run(tmp.path(), &args).status.success());
    assert!(worktree.exists());
    let mut opted_in = args.to_vec();
    opted_in.push("--allow-adopted");
    let reviewed = json(&run(tmp.path(), &opted_in));
    let digest = reviewed["digest"].as_str().unwrap();
    let mut unapproved_apply = args.to_vec();
    unapproved_apply.extend(["--confirm", digest]);
    assert!(!run(tmp.path(), &unapproved_apply).status.success());
    assert!(worktree.exists());
    opted_in.extend(["--confirm", digest]);
    let outcome = json(&run(tmp.path(), &opted_in));
    assert_eq!(outcome["worktree_removed"], true);
    assert!(!worktree.exists());
    assert_eq!(
        git(tmp.path(), &["rev-parse", "refs/heads/operator-owned"]),
        git(tmp.path(), &["rev-parse", "refs/heads/main"])
    );
    let archive = Path::new(outcome["archive"].as_str().unwrap());
    assert_eq!(
        std::fs::read(archive.join("untracked/recovery-note.txt")).unwrap(),
        b"preserve this note\n"
    );
}

#[test]
fn retirement_preserves_recorded_and_switched_checkout_branch_roots() {
    for recorded_branch_is_delivered in [true, false] {
        let tmp = repo();
        let mut broker = isolation::open(tmp.path());
        let session = broker
            .start_worktree("switched legacy checkout", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        if !recorded_branch_is_delivered {
            std::fs::write(worktree.join("recorded.txt"), b"recorded branch only\n").unwrap();
            git(&worktree, &["add", "recorded.txt"]);
            git(&worktree, &["commit", "-qm", "recorded branch root"]);
        }
        let recorded_ref = format!("refs/heads/{}", session.branch);
        let recorded_tip = git(&worktree, &["rev-parse", &recorded_ref]);
        git(&worktree, &["switch", "-c", "actual-checkout", "main"]);
        std::fs::write(worktree.join("actual.txt"), b"actual checkout only\n").unwrap();
        git(&worktree, &["add", "actual.txt"]);
        git(&worktree, &["commit", "-qm", "actual checkout root"]);
        let actual_tip = git(&worktree, &["rev-parse", "HEAD"]);
        assert_ne!(actual_tip, recorded_tip);
        broker.close(session.id).unwrap();
        drop(broker);
        let id = session.id.to_string();
        let args = [
            "finish",
            "cleanup",
            "resolve",
            &id,
            "--retire",
            "--archive",
            "--json",
        ];
        let reviewed = json(&run(tmp.path(), &args));
        let mut apply_args = args.to_vec();
        apply_args.extend(["--confirm", reviewed["digest"].as_str().unwrap()]);
        let outcome = json(&run(tmp.path(), &apply_args));
        assert_eq!(outcome["worktree_removed"], true);
        assert!(!worktree.exists());
        assert_eq!(git(tmp.path(), &["rev-parse", &recorded_ref]), recorded_tip);
        assert_eq!(
            git(tmp.path(), &["rev-parse", "refs/heads/actual-checkout"]),
            actual_tip
        );
        let archive = Path::new(outcome["archive"].as_str().unwrap());
        let bundle = archive.join("commits.bundle");
        let heads = git(
            tmp.path(),
            &["bundle", "list-heads", bundle.to_str().unwrap()],
        );
        assert!(
            heads.lines().any(|line| line.starts_with(&actual_tip)),
            "{heads}"
        );
        if !recorded_branch_is_delivered {
            assert!(
                heads.lines().any(|line| line.starts_with(&recorded_tip)),
                "{heads}"
            );
        }
    }
}

#[test]
fn recovery_batch_resume_skips_removed_targets_but_keeps_recreated_paths() {
    let tmp = repo();
    let (first, first_path) = closed_session(tmp.path(), "first resume target");
    let (second, second_path) = closed_session(tmp.path(), "second resume target");
    let single = [
        "finish",
        "cleanup",
        "resolve",
        &first,
        "--batch",
        "--archive",
        "--json",
    ];
    let initial = json(&run(tmp.path(), &single));
    let mut first_apply = single.to_vec();
    first_apply.extend(["--confirm", initial["digest"].as_str().unwrap()]);
    assert_eq!(json(&run(tmp.path(), &first_apply))["removed_count"], 1);
    assert!(!first_path.exists() && second_path.exists());

    let original_set = [
        "finish",
        "cleanup",
        "resolve",
        &first,
        &second,
        "--batch",
        "--archive",
        "--json",
    ];
    let human_args: Vec<_> = original_set
        .iter()
        .copied()
        .filter(|arg| *arg != "--json")
        .collect();
    let human = run(tmp.path(), &human_args);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(
        human.contains(&format!("session {first}: already removed; no new removal")),
        "{human}"
    );
    let reviewed = json(&run(tmp.path(), &original_set));
    assert_eq!(reviewed["targets"][0]["already_removed"], true);
    assert!(reviewed["targets"][0]["confirmation"].is_null());
    assert!(reviewed["targets"][0]["preserved_file_bytes"].is_null());
    let mut resumed_apply = original_set.to_vec();
    resumed_apply.extend(["--confirm", reviewed["digest"].as_str().unwrap()]);
    let outcome = json(&run(tmp.path(), &resumed_apply));
    assert_eq!(outcome["removed_count"], 1);
    assert_eq!(outcome["remaining_session_ids"], serde_json::json!([]));
    assert_eq!(outcome["results"][0]["already_removed"], true);
    assert!(outcome["results"][0]["outcome"].is_null());
    assert!(!first_path.exists() && !second_path.exists());

    std::fs::create_dir(&first_path).unwrap();
    std::fs::write(
        first_path.join("replacement-owner.txt"),
        b"keep replacement\n",
    )
    .unwrap();
    let reviewed = json(&run(tmp.path(), &original_set));
    assert_eq!(reviewed["targets"][0]["already_removed"], false);
    assert!(reviewed["targets"][0]["confirmation"].is_null());
    assert!(reviewed["targets"][0]["blocker"].is_string());
    let mut apply = original_set.to_vec();
    apply.extend(["--confirm", reviewed["digest"].as_str().unwrap()]);
    let retained = run(tmp.path(), &apply);
    assert!(!retained.status.success());
    let outcome: serde_json::Value = serde_json::from_slice(&retained.stdout).unwrap();
    assert_eq!(outcome["removed_count"], 0);
    assert_eq!(
        outcome["remaining_session_ids"],
        serde_json::json!([first.parse::<i64>().unwrap()])
    );
    assert_eq!(
        std::fs::read(first_path.join("replacement-owner.txt")).unwrap(),
        b"keep replacement\n"
    );
}
