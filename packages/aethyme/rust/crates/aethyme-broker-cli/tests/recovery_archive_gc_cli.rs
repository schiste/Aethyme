//! Recovery archives written by `finish cleanup resolve --archive` expire only
//! through a reviewed `gc plan` / `gc apply`, and only when the broker can
//! vouch for them.

use sha2::Digest;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

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
    let broker = common::open_fixture_repository_path_broker(repo).unwrap();
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(repo);
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    drop(broker);
    command.output().unwrap()
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

fn repo() -> common::FixtureRepository {
    let fixture = common::FixtureRepository::new();
    git(fixture.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(fixture.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(fixture.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(fixture.path(), &["add", "-A"]);
    git(fixture.path(), &["commit", "-qm", "init"]);
    fixture
}

fn archive_root(repo: &Path) -> PathBuf {
    let broker = common::open_fixture_repository_path_broker(repo).unwrap();
    // The fixture explicitly selects host state, so archives use that owned
    // root even when the repository itself is under the system temp directory.
    common::fixture_host_state_dir(repo)
        .join("recovery-archives")
        .join(broker.worktree_root_plan().unwrap().repository_key)
}

/// Close a session holding one unlanded commit and, when `dirty`, an
/// uncommitted edit; archive it through the reviewed resolve path.
fn archived_session(repo: &Path, name: &str, dirty: bool) -> (i64, String, PathBuf) {
    archived_session_with_mode(repo, name, dirty, false)
}

fn archived_session_with_mode(
    repo: &Path,
    name: &str,
    dirty: bool,
    retirement: bool,
) -> (i64, String, PathBuf) {
    let session = {
        let mut broker = common::open_fixture_repository_path_broker(repo).unwrap();
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
    let mut args = vec!["finish", "cleanup", "resolve", &id, "--archive", "--json"];
    if retirement {
        args.push("--retire");
    }
    let reviewed = json(&run(repo, &args));
    let digest = reviewed["digest"].as_str().unwrap().to_owned();
    args.extend(["--confirm", &digest]);
    let outcome = json(&run(repo, &args));
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
        let aged = created - days * DAY_MS;
        if manifest["schema_version"].as_u64().unwrap_or_default() >= 5 {
            // Build a consistently aged fixture, not a one-sided metadata edit.
            let record_path = archive.join("retirement.json");
            let mut record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
            assert_eq!(record["archive_created_at_ms"], created);
            record["archive_created_at_ms"] = serde_json::json!(aged);
            let bytes = serde_json::to_vec(&record).unwrap();
            manifest["retirement_record_sha256"] =
                serde_json::json!(format!("{:x}", sha2::Sha256::digest(&bytes)));
            std::fs::write(record_path, bytes).unwrap();
        }
        manifest["created_at_ms"] = serde_json::json!(aged);
    });
}

fn plan(repo: &Path) -> serde_json::Value {
    json(&run(repo, &["gc", "plan", "--full-scan", "--json"]))
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

#[test]
fn default_plan_defers_a_nonempty_archive_inventory_without_a_digest() {
    let tmp = repo();
    archived_session(tmp.path(), "archived", true);

    let reviewed = json(&run(tmp.path(), &["gc", "plan", "--json"]));

    assert!(
        reviewed["digest"].as_str().unwrap().is_empty(),
        "{reviewed}"
    );
    assert!(proposed(&reviewed).is_empty(), "{reviewed}");
    assert!(
        reviewed["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check == "recovery_archives"),
        "{reviewed}"
    );
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

    let text = run(tmp.path(), &["gc", "plan", "--full-scan"]);
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
fn a_landed_archive_is_proposed_after_minimum_retention_before_policy_expiry() {
    let tmp = repo();
    let (_, head, archive) = archived_session(tmp.path(), "landed", false);
    // The archived commit reaches the primary checkout after archiving.
    git(tmp.path(), &["merge", "-q", "--ff-only", &head]);

    let young = plan(tmp.path());
    assert!(proposed(&young).is_empty(), "{young}");
    // Landing does not bypass the 15-day minimum; it permits collection
    // afterward without waiting for the default 30-day policy expiry.
    age(&archive, 16);
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
        &["gc", "apply", "--confirm", &digest, "--full-scan", "--json"],
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
    let output = run(
        tmp.path(),
        &["gc", "apply", "--confirm", &digest, "--full-scan", "--json"],
    );
    assert!(!output.status.success(), "a stale digest must be refused");
    assert!(first.exists() && second.exists(), "nothing was removed");
}

#[test]
fn retirement_archive_floor_survives_landing_and_a_shorter_current_policy() {
    for dirty in [false, true] {
        let tmp = repo();
        let (_, head, archive) = archived_session_with_mode(tmp.path(), "retirement", dirty, true);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(archive.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["schema_version"], 6);
        assert!(manifest["minimum_archive_retention_ms"].as_u64().unwrap() >= 15 * DAY_MS as u64);
        if !dirty {
            git(tmp.path(), &["merge", "-q", "--ff-only", &head]);
        }
        std::fs::write(
            tmp.path().join(".aethyme/broker.toml"),
            "[retention]\nrecovery_archive_days = 1\n",
        )
        .unwrap();
        age(&archive, 2);
        let protected = plan(tmp.path());
        assert!(proposed(&protected).is_empty(), "{protected}");
        assert!(
            unowned(&protected).is_empty(),
            "a valid protected archive remains owned: {protected}"
        );
        assert!(archive.exists());

        age(&archive, 14);
        let expired = plan(tmp.path());
        let candidates = proposed(&expired);
        assert_eq!(candidates.len(), 1, "{expired}");
        assert_eq!(candidates[0]["landed"], !dirty);
        let digest = expired["digest"].as_str().unwrap();
        let applied = json(&run(
            tmp.path(),
            &["gc", "apply", "--confirm", digest, "--full-scan", "--json"],
        ));
        assert_eq!(
            applied["recovery_archives_removed"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{applied}"
        );
        assert!(!archive.exists());
    }
}

fn prerequisite_refs(archive: &Path) -> Vec<(String, String)> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(archive.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 6, "{manifest}");
    manifest["prerequisite_refs"]
        .as_array()
        .unwrap_or_else(|| panic!("v6 archive has no prerequisite_refs: {manifest}"))
        .iter()
        .map(|item| {
            (
                item["ref_name"].as_str().unwrap().to_owned(),
                item["object_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn set_short_archive_retention(repo: &Path) {
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nrecovery_archive_days = 1\n",
    )
    .unwrap();
}

#[test]
fn gc_removes_exact_v6_prerequisite_refs_only_after_archive_removal() {
    let tmp = repo();
    let (_, _, archive) = archived_session_with_mode(tmp.path(), "pinned", false, true);
    let refs = prerequisite_refs(&archive);
    assert!(!refs.is_empty(), "v6 archive must own prerequisite refs");
    for (name, oid) in &refs {
        assert_eq!(git(tmp.path(), &["rev-parse", name]), *oid);
    }
    set_short_archive_retention(tmp.path());
    age(&archive, 16);

    let reviewed = plan(tmp.path());
    let candidates = proposed(&reviewed);
    assert_eq!(
        candidates.len(),
        1,
        "pins must not make their own archive landed: {reviewed}"
    );
    let digest = reviewed["digest"].as_str().unwrap();
    let applied = json(&run(
        tmp.path(),
        &["gc", "apply", "--confirm", digest, "--full-scan", "--json"],
    ));

    assert!(!archive.exists(), "the reviewed archive is fully removed");
    assert_eq!(
        applied["recovery_archives_removed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        applied["failures"].as_array().unwrap().len(),
        0,
        "{applied}"
    );
    for (name, _) in refs {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &name])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "prerequisite ref still exists: {name}"
        );
    }
}

#[test]
fn changed_v6_prerequisite_ref_keeps_archive_and_the_new_ref_target() {
    let tmp = repo();
    let (_, replacement, archive) =
        archived_session_with_mode(tmp.path(), "changed-pin", false, true);
    let refs = prerequisite_refs(&archive);
    assert!(!refs.is_empty());
    set_short_archive_retention(tmp.path());
    age(&archive, 16);

    let reviewed = plan(tmp.path());
    assert_eq!(proposed(&reviewed).len(), 1, "{reviewed}");
    let digest = reviewed["digest"].as_str().unwrap();
    assert_ne!(replacement, refs[0].1);
    git(tmp.path(), &["update-ref", &refs[0].0, &replacement]);

    let apply = run(
        tmp.path(),
        &["gc", "apply", "--confirm", digest, "--full-scan", "--json"],
    );

    assert!(
        !apply.status.success(),
        "a changed prerequisite ref invalidates the reviewed plan"
    );
    assert!(
        archive.exists(),
        "changed prerequisite ownership refuses removal"
    );
    assert_eq!(git(tmp.path(), &["rev-parse", &refs[0].0]), replacement);
    for (name, oid) in refs.iter().skip(1) {
        assert_eq!(git(tmp.path(), &["rev-parse", name]), *oid);
    }
}

#[test]
fn a_retirement_archive_with_an_inconsistent_creation_time_stays_protected() {
    let tmp = repo();
    let (_, head, archive) =
        archived_session_with_mode(tmp.path(), "inconsistent-time", false, true);
    git(tmp.path(), &["merge", "-q", "--ff-only", &head]);
    edit_manifest(&archive, |manifest| {
        let created = manifest["created_at_ms"].as_i64().unwrap();
        manifest["created_at_ms"] = serde_json::json!(created - 365 * DAY_MS);
    });
    let reviewed = plan(tmp.path());
    assert!(proposed(&reviewed).is_empty(), "{reviewed}");
    let rejected = unowned(&reviewed);
    assert_eq!(rejected.len(), 1, "{reviewed}");
    assert!(rejected[0].contains("retirement"), "{rejected:?}");
    assert!(archive.exists());
}

#[test]
fn landed_retirement_head_does_not_hide_an_unlanded_recorded_branch_root() {
    let tmp = repo();
    let mut broker = common::open_fixture_repository_path_broker(tmp.path()).unwrap();
    let session = broker.start_worktree("two archive roots", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(
        worktree.join("unique-recorded-root.txt"),
        b"unlanded recorded work\n",
    )
    .unwrap();
    git(&worktree, &["add", "unique-recorded-root.txt"]);
    git(&worktree, &["commit", "-qm", "unique recorded root"]);
    let recorded_tip = git(&worktree, &["rev-parse", "HEAD"]);
    git(
        &worktree,
        &["switch", "-c", "already-delivered-checkout", "main"],
    );
    assert_ne!(git(&worktree, &["rev-parse", "HEAD"]), recorded_tip);
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]),
        git(tmp.path(), &["rev-parse", "main"])
    );
    broker.close(session.id).unwrap();
    drop(broker);
    let id = session.id.to_string();
    let mut args = vec![
        "finish",
        "cleanup",
        "resolve",
        &id,
        "--retire",
        "--archive",
        "--json",
    ];
    let reviewed = json(&run(tmp.path(), &args));
    let digest = reviewed["digest"].as_str().unwrap();
    args.extend(["--confirm", digest]);
    let outcome = json(&run(tmp.path(), &args));
    assert_eq!(outcome["worktree_removed"], true);
    let archive = PathBuf::from(outcome["archive"].as_str().unwrap());
    age(&archive, 16);
    let protected = plan(tmp.path());
    assert!(
        proposed(&protected).is_empty(),
        "the recorded root remains unique under the default 30-day archive policy: {protected}"
    );
    assert!(unowned(&protected).is_empty(), "{protected}");
    assert!(archive.exists());

    git(tmp.path(), &["merge", "-q", "--ff-only", &recorded_tip]);
    let landed = plan(tmp.path());
    let candidates = proposed(&landed);
    assert_eq!(candidates.len(), 1, "{landed}");
    assert_eq!(candidates[0]["landed"], true);
}
