use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, FinishOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let output = git_output(repo, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(repo: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(CLI);
    clear_git_environment(&mut command);
    command.args(args).current_dir(repo).output().unwrap()
}

fn clear_git_environment(command: &mut Command) {
    let git_keys = std::env::vars_os()
        .map(|(key, _)| key)
        .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect::<Vec<_>>();
    for key in git_keys {
        command.env_remove(key);
    }
}

#[test]
fn bulk_cleanup_is_dry_run_by_default_and_apply_revalidates() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(tmp.path());
    // `--keep-worktree` retains the checkout only at finish; afterwards the
    // closed-worktree grace decides. Once this test lowers that grace to zero,
    // the landed checkout is eligible for unattended removal, and the bounded
    // inline sweep of any writable broker open (`gc plan`, or the refused
    // unconfirmed apply) may remove it before the assertions that the plan
    // and refused applies left it alone. This test is about explicit bulk
    // cleanup, so turn unattended checkout removal off explicitly.
    aethyme_testkit::disable_unattended_checkout_removal(tmp.path());

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("cleanup CLI fixture", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(
        worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(worktree.join("target/debug/cache.bin"), vec![3_u8; 2048]).unwrap();
    drop(broker);

    let plan = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--json"],
    );
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["applied"], false);
    assert_eq!(plan["plan"]["retained_worktree_count"], 1);
    assert_eq!(plan["plan"]["eligible_worktree_count"], 1);
    assert_eq!(plan["plan"]["schema_version"], 1);
    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();
    assert_eq!(digest.len(), 64);
    assert!(plan["removed_session_ids"].as_array().unwrap().is_empty());
    assert!(worktree.exists());

    // GC must use the same representation proof as cleanup. The only thing
    // GC adds is a short grace period after a state-only close, reported as
    // its own blocker rather than as the `closed_worktrees_days` age gate
    // that once made the two disagree for a week.
    let gc = run(tmp.path(), &["gc", "plan", "--json"]);
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    let gc: serde_json::Value = serde_json::from_slice(&gc.stdout).unwrap();
    assert!(gc["worktrees"].as_array().unwrap().is_empty(), "{gc}");
    assert!(gc["blockers"].as_array().unwrap().iter().any(|blocker| {
        blocker["kind"] == "closed_worktree_grace" && blocker["id"].as_i64() == Some(session.id)
    }));
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 0\n",
    )
    .unwrap();
    let gc = run(tmp.path(), &["gc", "plan", "--json"]);
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    let gc: serde_json::Value = serde_json::from_slice(&gc.stdout).unwrap();
    assert!(
        gc["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["session_id"].as_i64() == Some(session.id)),
        "GC omitted cleanup's eligible worktree: {gc}"
    );
    assert!(!gc["blockers"].as_array().unwrap().iter().any(|blocker| {
        blocker["kind"] == "retention_age" && blocker["id"].as_i64() == Some(session.id)
    }));

    let unconfirmed = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--apply", "--json"],
    );
    assert!(!unconfirmed.status.success());
    assert!(worktree.exists());

    // The retention configuration changed since the first plan. Its digest
    // must no longer authorize cleanup, even if the checkout is still eligible.
    let stale = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(!stale.status.success());
    assert!(worktree.exists());
    let stale: serde_json::Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert!(
        stale["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer matches")
    );

    let refreshed = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--json"],
    );
    assert!(refreshed.status.success());
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    let digest = refreshed["plan"]["digest"].as_str().unwrap();

    let apply = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            digest,
            "--json",
        ],
    );
    assert!(
        apply.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    let apply: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
    assert_eq!(apply["applied"], true);
    assert_eq!(
        apply["removed_session_ids"],
        serde_json::json!([session.id])
    );
    assert!(apply["failures"].as_array().unwrap().is_empty());
    assert!(!worktree.exists());
    assert!(
        !git_output(
            tmp.path(),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", session.branch),
            ],
        )
        .status
        .success()
    );
}

#[test]
fn bulk_cleanup_rejects_force_and_session_mixups() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    for args in [
        vec!["finish", "cleanup", "--all-cleaned", "--force"],
        vec!["finish", "cleanup", "12", "--all-cleaned"],
        vec!["finish", "cleanup", "12", "--apply"],
    ] {
        let output = run(tmp.path(), &args);
        assert!(!output.status.success(), "unexpected success for {args:?}");
    }
}

/// `--dry-run`, `--confirm` and `--detail` belong to `--all-cleaned`. Given to
/// a single-session cleanup they were dropped, so `cleanup <id> --dry-run`
/// cleaned for real; they are now a usage error that changes nothing.
#[test]
fn single_session_cleanup_refuses_sweep_only_flags() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    for (args, flag) in [
        (vec!["finish", "cleanup", "12", "--dry-run"], "--dry-run"),
        (
            vec!["finish", "cleanup", "12", "--confirm", "abc"],
            "--confirm",
        ),
        (vec!["finish", "cleanup", "12", "--detail"], "--detail"),
    ] {
        let output = run(tmp.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("cleanup <session-id> does not take {flag}")),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn bulk_cleanup_confirmation_binds_the_exact_reviewed_branch_tip() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(tmp.path());

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("cleanup confirmation", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    drop(broker);

    let plan = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--json"],
    );
    assert!(plan.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();

    std::fs::write(worktree.join("followup.txt"), "must survive\n").unwrap();
    git(&worktree, &["add", "followup.txt"]);
    git(&worktree, &["commit", "-qm", "pending followup"]);

    let apply = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(!apply.status.success());
    let refusal = json_error::message(&apply);
    assert!(
        refusal.contains("no longer matches current state")
            && refusal.contains("aethyme broker finish cleanup --all-cleaned"),
        "{refusal}"
    );
    assert!(
        !refusal.contains("expected"),
        "a freshly computed digest must not be offered as a value to confirm: {refusal}"
    );
    assert!(worktree.exists());
    assert!(worktree.join("followup.txt").exists());
}

/// A worktree whose removal died partway is deregistered but still on disk:
/// `git worktree remove` unlinks the administrative entry first, then walks
/// the tree, and an `ENOTEMPTY` on the way out leaves the files behind with no
/// gitdir to reach them (#165). Nothing about that changes what the session
/// did, so cleanup must still judge it -- and finish the removal.
#[test]
fn an_interrupted_worktree_removal_is_judged_and_completed_rather_than_refused() {
    const CHILD: &str = "AETHYME_TEST_EXTERNAL_ORPHAN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cleanup_cli::an_interrupted_worktree_removal_is_judged_and_completed_rather_than_refused", "--nocapture"])
            .env(CHILD, "1")
            .env("AETHYME_WORKTREE_ROOT", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(tmp.path());

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("interrupted removal", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    assert!(
        !worktree.starts_with(tmp.path()),
        "fixture requires an external worktree"
    );
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    let worktree_text = worktree.to_string_lossy().into_owned();
    git(
        tmp.path(),
        &[
            "worktree",
            "lock",
            "--reason",
            "preserve recovery fixture",
            &worktree_text,
        ],
    );
    let finish = broker
        .finish_with_options(
            session.id,
            FinishOptions {
                keep_worktree: false,
            },
        )
        .unwrap();
    assert!(finish.closed);
    assert!(
        finish.cleanup.failure.is_some(),
        "the fixture lock must leave a real persisted finish-cleanup intent"
    );
    assert!(!finish.cleanup.completed);

    // Reproduce a partial removal after the durable finish intent: tracked
    // payload remains, while both Git's admin entry and the checkout pointer
    // are gone. Recovery must be bound to the intent, not just the branch.
    std::fs::remove_file(worktree.join("README.md")).unwrap();
    let gitdir = std::fs::read_to_string(worktree.join(".git")).unwrap();
    let gitdir = std::path::PathBuf::from(gitdir.trim().strip_prefix("gitdir:").unwrap().trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        worktree.join(gitdir)
    };
    std::fs::remove_dir_all(&gitdir).unwrap();
    std::fs::remove_file(worktree.join(".git")).unwrap();
    assert!(!worktree.join("README.md").exists());
    assert!(worktree.join("done.txt").exists());
    drop(broker);

    let plan = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--json"],
    );
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let item = &plan["plan"]["worktrees"][0];
    assert_eq!(
        item["disposition"], "eligible",
        "orphaned worktree judged {}: {}",
        item["disposition"], item["reason"]
    );

    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();
    let applied = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(
        applied["failures"].as_array().unwrap().is_empty(),
        "{:?}",
        applied["failures"]
    );
    assert!(!worktree.exists(), "stranded worktree was not removed");
}

/// A squash merge rewrites the SHA, so no ancestry check can see the work
/// land. The recorded representation is the only evidence that survives, and
/// cleanup has to consult it or the session is uncleanable forever (#164).
#[test]
fn a_squash_merged_session_is_eligible_on_its_recorded_representation() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let checkout = tmp.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&checkout, &["init", "-q", "-b", "main"]);
    std::fs::write(checkout.join("README.md"), "fixture\n").unwrap();
    std::fs::write(checkout.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-qm", "init"]);
    git(
        &checkout,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&checkout, &["push", "-q", "-u", "origin", "main"]);
    // The representation scan reads the default branch from `origin/HEAD`,
    // which a real clone gets for free.
    git(&checkout, &["remote", "set-head", "origin", "--auto"]);
    let tmp_root = tmp;
    let tmp = Holder(checkout);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("squash merged", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("feature.txt"), "feature\n").unwrap();
    git(&worktree, &["add", "feature.txt"]);
    git(&worktree, &["commit", "-qm", "feature"]);
    let session_head = String::from_utf8(git_output(&worktree, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();

    // The squash merge: same content on main, a commit the session never made.
    std::fs::write(tmp.path().join("feature.txt"), "feature\n").unwrap();
    git(tmp.path(), &["add", "feature.txt"]);
    git(tmp.path(), &["commit", "-qm", "feature (#1)"]);

    let scan = broker.scan_session_representation(session.id).unwrap();
    assert!(
        scan.search.represented(),
        "squashed content was not located on the default branch"
    );
    broker
        .record_session_representation(session.id, &scan.digest)
        .unwrap();
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    drop(broker);

    let plan = run(
        tmp.path(),
        &["finish", "cleanup", "--all-cleaned", "--json"],
    );
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let item = &plan["plan"]["worktrees"][0];
    assert_eq!(
        item["disposition"], "eligible",
        "squash-merged session judged {}: {}",
        item["disposition"], item["reason"]
    );
    let provenance = &item["provenance"];
    assert_eq!(provenance["session_head"], session_head);
    assert!(
        provenance["represented_by_commit"].is_string(),
        "eligibility did not name the commit that carried the work: {provenance}"
    );
    drop(tmp_root);
}

/// Lets the squash fixture keep the `tmp.path()` spelling of its siblings
/// while working inside a checkout that has an `origin` beside it.
struct Holder(std::path::PathBuf);

impl Holder {
    fn path(&self) -> &Path {
        &self.0
    }
}

#[path = "common/json_error.rs"]
mod json_error;

#[test]
fn scoped_cleanup_cli_accepts_a_fixed_cutoff_and_rejects_incomplete_scope() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "README.md"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    drop(Broker::open(tmp.path()).unwrap());
    let output = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--closed-before-ms",
            "123456",
            "--after-session",
            "3",
            "--limit",
            "5",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["plan"]["closed_before_ms"], 123456);
    assert_eq!(json["plan"]["after_session"], 3);
    assert_eq!(json["plan"]["batch_limit"], 5);
    assert!(json["plan"].get("batch_last_session").is_none());
    let incomplete = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--closed-before-ms",
            "123456",
            "--json",
        ],
    );
    assert!(!incomplete.status.success());
}

const SWEEP_META_PREFIX: &str = "gc.artifact_sweep.v2.";
/// What an unspaced inline sweep leaves behind: the finished-lap stamp, the
/// paused-lap stamp, or (on every pass, finished or not) its last report.
const SWEEP_WITNESS_SUFFIXES: [&str; 3] = [".last_run_ms", ".last_partial_ms", ".last_report"];

/// Forget earlier sweeps, so the next writable broker open is not spaced out
/// by a recent stamp and runs its inline pass.
fn reset_sweep_witnesses(repo: &Path) {
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    for suffix in SWEEP_WITNESS_SUFFIXES {
        db.execute(
            "DELETE FROM meta WHERE substr(key, 1, length(?1)) = ?1 AND substr(key, -length(?2)) = ?2",
            rusqlite::params![SWEEP_META_PREFIX, suffix],
        )
        .unwrap();
    }
}

/// Sweep stamps and reports present, plus the number of GC events.
fn sweep_witnesses(repo: &Path) -> (Vec<String>, i64) {
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    let mut statement = db
        .prepare("SELECT key, value FROM meta WHERE substr(key, 1, length(?1)) = ?1")
        .unwrap();
    let keys = statement
        .query_map([SWEEP_META_PREFIX], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .filter(|(key, value)| {
            SWEEP_WITNESS_SUFFIXES
                .iter()
                .any(|suffix| key.ends_with(*suffix))
                && value != "0"
        })
        .map(|(key, _)| key)
        .collect();
    let gc_events = db
        .query_row(
            "SELECT COUNT(*) FROM events WHERE substr(kind, 1, 10) = 'broker.gc.'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (keys, gc_events)
}

/// Every writable broker open resumes GC maintenance and runs a bounded
/// inline sweep that may remove an eligible closed checkout. A command refused
/// for a malformed argument must refuse before that open, so it changes
/// nothing. Removal itself depends on how much of the 250 ms budget the
/// runner gets, so the test asserts on what every unspaced inline pass writes
/// however fast it runs: its last report. The closing control shows that a
/// writable open in this very fixture writes it.
#[test]
fn malformed_commands_refuse_before_the_writable_open_and_change_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(tmp.path());

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("refusal fixture", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    drop(broker);
    // Unattended removal stays on (the default): with no grace the landed,
    // closed checkout is eligible for it on the next writable open.
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 0\n",
    )
    .unwrap();

    let digest_shaped = "0".repeat(64);
    let refusals: [(&[&str], &str); 8] = [
        (
            &["finish", "cleanup", "--all-cleaned", "--apply"],
            "must be a full SHA-256 digest",
        ),
        (
            &[
                "finish",
                "cleanup",
                "--all-cleaned",
                "--apply",
                "--confirm",
                "not-a-digest",
            ],
            "must be a full SHA-256 digest",
        ),
        (&["gc", "apply"], "gc apply requires --confirm"),
        (&["gc", "frobnicate"], "unknown gc action"),
        (&["advanced", "leases", "claim"], "claim requires a path"),
        (
            &["advanced", "leases", "frobnicate"],
            "unknown leases action",
        ),
        (
            &["advanced", "representation", "record", "--session", "9999"],
            "representation record requires --confirm",
        ),
        (
            &["start", "--adopt", "--reuse", "--replace-stale"],
            "mutually exclusive",
        ),
    ];
    for (args, refusal) in refusals {
        reset_sweep_witnesses(tmp.path());
        let (_, gc_events_before) = sweep_witnesses(tmp.path());
        let output = run(tmp.path(), args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{args:?} succeeded: {text}");
        assert!(text.contains(refusal), "{args:?}: {text}");
        let (witnesses, gc_events) = sweep_witnesses(tmp.path());
        assert!(
            witnesses.is_empty(),
            "{args:?} was refused after a writable broker open ran its sweep: {witnesses:?}"
        );
        assert_eq!(gc_events, gc_events_before, "{args:?} wrote a GC event");
        assert!(worktree.exists(), "{args:?} removed the eligible checkout");
    }

    // Control: a well-formed digest passes every argument check, so this
    // command opens the broker for writing before its plan refuses the stale
    // digest. That open must write the witness the loop asserts is absent.
    reset_sweep_witnesses(tmp.path());
    let control = run(
        tmp.path(),
        &[
            "finish",
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest_shaped,
        ],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&control.stdout),
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(!control.status.success(), "{text}");
    assert!(text.contains("no longer matches"), "{text}");
    let (witnesses, _) = sweep_witnesses(tmp.path());
    assert!(
        !witnesses.is_empty(),
        "a writable open in this fixture must leave a sweep witness"
    );
}
