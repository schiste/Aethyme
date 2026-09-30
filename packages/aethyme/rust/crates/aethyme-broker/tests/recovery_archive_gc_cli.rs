//! Recovery archives written by `finish cleanup resolve --archive` expire only
//! through a reviewed `gc plan` / `gc apply`, and only when the broker can
//! vouch for them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const DAY_MS: i64 = 86_400_000;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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

fn archive_root(repo: &Path) -> PathBuf {
    repo.join(".aethyme/recovery-archives")
}

/// Close a session holding one unlanded commit and, when `dirty`, an
/// uncommitted edit; archive it through the reviewed resolve path.
fn archived_session(repo: &Path, name: &str, dirty: bool) -> (i64, String, PathBuf) {
    let session = {
        let mut broker = Broker::open(repo).unwrap();
        let session = broker.start_worktree(name, None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join(format!("{name}.txt")), "work\n").unwrap();
        git(&worktree, &["add", "-A"]);
        git(&worktree, &["commit", "-qm", name]);
        if dirty {
            std::fs::write(worktree.join("README.md"), "fixture\nedit\n").unwrap();
        }
        broker.close(session.id).unwrap();
        session
    };
    let id = session.id.to_string();
    let reviewed = json(&run(
        repo,
        &["finish", "cleanup", "resolve", &id, "--archive", "--json"],
    ));
    let digest = reviewed["digest"].as_str().unwrap().to_owned();
    let outcome = json(&run(
        repo,
        &[
            "finish",
            "cleanup",
            "resolve",
            &id,
            "--archive",
            "--confirm",
            &digest,
            "--json",
        ],
    ));
    let head = reviewed["head"].as_str().unwrap().to_owned();
    (
        session.id,
        head,
        PathBuf::from(outcome["archive"].as_str().unwrap()),
    )
}

fn edit_manifest(archive: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = archive.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
}

fn age(archive: &Path, days: i64) {
    edit_manifest(archive, |manifest| {
        let created = manifest["created_at_ms"].as_i64().unwrap();
        manifest["created_at_ms"] = serde_json::json!(created - days * DAY_MS);
    });
}

fn plan(repo: &Path) -> serde_json::Value {
    json(&run(repo, &["gc", "plan", "--json"]))
}

fn proposed(plan: &serde_json::Value) -> Vec<serde_json::Value> {
    plan["recovery_archives"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn unowned(plan: &serde_json::Value) -> Vec<String> {
    plan["recovery_archive_inventory"]["unowned"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[test]
fn an_expired_unlanded_archive_is_proposed_and_says_it_holds_unlanded_work() {
    let tmp = repo();
    let (id, _, archive) = archived_session(tmp.path(), "expired", true);
    age(&archive, 31);

    let reviewed = plan(tmp.path());
    let candidates = proposed(&reviewed);
    assert_eq!(candidates.len(), 1, "{reviewed}");
    assert_eq!(candidates[0]["session_id"], id);
    assert_eq!(candidates[0]["landed"], false);
    let reason = candidates[0]["reason"].as_str().unwrap();
    assert!(reason.contains("contains unlanded work"), "{reason}");

    let text = run(tmp.path(), &["gc", "plan"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("recovery archive:"), "{text}");
    assert!(text.contains("contains unlanded work"), "{text}");
}

#[test]
fn a_young_unlanded_archive_is_listed_but_not_proposed() {
    let tmp = repo();
    archived_session(tmp.path(), "young", true);

    let reviewed = plan(tmp.path());
    assert!(proposed(&reviewed).is_empty(), "{reviewed}");
    assert_eq!(reviewed["recovery_archive_inventory"]["count"], 1);
}

#[test]
fn zero_days_keeps_every_archive() {
    let tmp = repo();
    let (_, _, archive) = archived_session(tmp.path(), "forever", true);
    age(&archive, 3650);
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nrecovery_archive_days = 0\n",
    )
    .unwrap();

    assert!(proposed(&plan(tmp.path())).is_empty());
}

#[test]
fn an_archive_whose_commits_landed_is_proposed_before_it_expires() {
    let tmp = repo();
    let (_, head, _) = archived_session(tmp.path(), "landed", false);
    // The archived commit reaches the primary checkout after archiving.
    git(tmp.path(), &["merge", "-q", "--ff-only", &head]);

    let candidates = proposed(&plan(tmp.path()));
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["landed"], true);
    assert!(
        candidates[0]["reason"]
            .as_str()
            .unwrap()
            .contains("nothing in it is unique")
    );
}

/// Landed commits do not make uncommitted changes safe to drop: they were
/// never on any branch.
#[test]
fn landed_commits_with_uncommitted_changes_wait_for_the_retention_period() {
    let tmp = repo();
    let (_, head, _) = archived_session(tmp.path(), "partly", true);
    git(tmp.path(), &["merge", "-q", "--ff-only", &head]);

    assert!(proposed(&plan(tmp.path())).is_empty());
}

#[test]
fn a_hand_made_archive_is_reported_and_never_proposed() {
    let tmp = repo();
    let manual = archive_root(tmp.path()).join("20260921T071003Z");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("repository-history.bundle"), b"bundle").unwrap();

    let reviewed = plan(tmp.path());
    assert!(proposed(&reviewed).is_empty());
    let unowned = unowned(&reviewed);
    assert_eq!(unowned.len(), 1, "{reviewed}");
    assert!(unowned[0].contains("no broker manifest"), "{}", unowned[0]);
}

#[test]
fn an_archive_naming_another_repository_or_session_is_refused() {
    let tmp = repo();
    let (_, _, foreign) = archived_session(tmp.path(), "foreign", true);
    let (_, _, unknown) = archived_session(tmp.path(), "unknown", true);
    age(&foreign, 400);
    age(&unknown, 400);
    edit_manifest(&foreign, |manifest| {
        manifest["repository"] = serde_json::json!("/elsewhere/another-repo");
    });
    edit_manifest(&unknown, |manifest| {
        manifest["session_id"] = serde_json::json!(999_999);
    });

    let reviewed = plan(tmp.path());
    assert!(proposed(&reviewed).is_empty(), "{reviewed}");
    let unowned = unowned(&reviewed).join("\n");
    assert!(unowned.contains("another repository"), "{unowned}");
    assert!(unowned.contains("not recorded here"), "{unowned}");
}

#[test]
fn applying_the_reviewed_plan_removes_exactly_the_proposed_archive() {
    let tmp = repo();
    let (id, _, expired) = archived_session(tmp.path(), "old", true);
    let (_, _, young) = archived_session(tmp.path(), "new", true);
    age(&expired, 45);

    let digest = plan(tmp.path())["digest"].as_str().unwrap().to_owned();
    let report = json(&run(
        tmp.path(),
        &["gc", "apply", "--confirm", &digest, "--json"],
    ));
    let removed = report["recovery_archives_removed"].as_array().unwrap();
    assert_eq!(removed.len(), 1, "{report}");
    assert_eq!(
        canonical(Path::new(removed[0].as_str().unwrap()).parent().unwrap()),
        canonical(&archive_root(tmp.path()))
    );
    assert!(!expired.exists(), "the reviewed archive is removed");
    assert!(young.exists(), "the young archive is kept");

    let events = Command::new("sqlite3")
        .arg(tmp.path().join(".aethyme/broker.db"))
        .arg(format!(
            "select count(*) from events where kind = 'broker.gc.recovery-archive-removed' and session_id = {id}"
        ))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&events.stdout).trim(), "1");
}

#[test]
fn a_digest_reviewed_before_another_archive_expired_is_refused() {
    let tmp = repo();
    let (_, _, first) = archived_session(tmp.path(), "first", true);
    let (_, _, second) = archived_session(tmp.path(), "second", true);
    age(&first, 45);
    let digest = plan(tmp.path())["digest"].as_str().unwrap().to_owned();

    // The set changes after review: the second archive now expires too.
    age(&second, 45);
    let output = run(tmp.path(), &["gc", "apply", "--confirm", &digest, "--json"]);
    assert!(!output.status.success(), "a stale digest must be refused");
    assert!(first.exists() && second.exists(), "nothing was removed");
}
