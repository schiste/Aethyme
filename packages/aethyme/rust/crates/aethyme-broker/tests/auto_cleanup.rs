//! #588: deterministic automatic removal of disposable session checkouts.
//!
//! Every fixture is a clone of a bare `origin`, with a publisher clone
//! standing in for the forge, so "contained in the remote default branch" is
//! a real remote-tracking ref that local branches cannot satisfy.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{AutoCleanupReport, Broker};

fn git_out(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", message]);
    git_out(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _tmp: tempfile::TempDir,
    publisher: PathBuf,
    repo: PathBuf,
}

/// A bare origin, a publisher clone and the repository under test.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let publisher = tmp.path().join("publisher");
    let repo = tmp.path().join("repo");
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "publisher"],
    );
    std::fs::write(
        publisher.join(".gitignore"),
        "/.aethyme/\ntarget/\nnode_modules/\n.env\n",
    )
    .unwrap();
    std::fs::write(publisher.join("shared.txt"), "one\n").unwrap();
    git(&publisher, &["add", "-A"]);
    git(&publisher, &["commit", "-qm", "init"]);
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "repo"],
    );
    Fixture {
        _tmp: tmp,
        publisher,
        repo,
    }
}

/// A closed session whose one commit reached origin/main by a merge commit.
fn merged_closed_session(fx: &Fixture, broker: &mut Broker, close: bool) -> (i64, PathBuf, String) {
    let session = broker.start_worktree("merged work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    // Content unique to the session, so a second one still has a change.
    let content = format!("work {}\n", session.id);
    let head = commit(&worktree, "feature.txt", &content, "feature");
    // The forge merges the branch with a merge commit.
    git(
        &worktree,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{}", session.branch),
        ],
    );
    git(&fx.publisher, &["fetch", "-q", "origin"]);
    git(
        &fx.publisher,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "merge",
            &format!("origin/{}", session.branch),
        ],
    );
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);
    if close {
        broker.close(session.id).unwrap();
    }
    (session.id, worktree, head)
}

fn run(broker: &mut Broker) -> AutoCleanupReport {
    broker
        .auto_cleanup_now(None)
        .unwrap()
        .expect("no other GC holds the lock")
}

fn kept_reason<'a>(report: &'a AutoCleanupReport, worktree: &Path) -> &'a str {
    let canonical = std::fs::canonicalize(worktree)
        .unwrap_or_else(|_| worktree.to_path_buf())
        .to_string_lossy()
        .into_owned();
    report
        .kept
        .iter()
        .find(|kept| kept.worktree == canonical)
        .map(|kept| kept.reason.as_str())
        .unwrap_or_else(|| panic!("{} not kept: {report:#?}", worktree.display()))
}

#[test]
fn a_merged_closed_checkout_is_removed_by_ancestry_and_journaled() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (session_id, worktree, head) = merged_closed_session(&fx, &mut broker, true);
    // Regenerable output and Finder metadata do not keep it.
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(worktree.join("target/debug/app"), "bin").unwrap();
    // Cargo writes this into every target directory it owns.
    std::fs::write(
        worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(worktree.join(".DS_Store"), "finder").unwrap();

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    let removed = &report.removed[0];
    assert_eq!(removed.proof, "ancestry");
    assert_eq!(removed.head, head);
    assert_eq!(removed.contained_in, "refs/remotes/origin/main");
    assert!(!worktree.exists());
    let session = broker.store().session(session_id).unwrap();
    assert_eq!(
        session.cleanup_state,
        aethyme_broker::SessionCleanupState::Cleaned
    );
    assert!(
        git_out(&fx.repo, &["branch", "--list", &session.branch]).is_empty(),
        "the proved session branch is deleted"
    );
    let events = broker
        .store()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "broker.cleanup.auto_removed")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    // `status` and `gc plan` both show it.
    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.auto_cleanup.as_ref().unwrap().removed.len(), 1);
}

#[test]
fn a_squash_landing_is_removed_through_the_deep_proof() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let session = broker.start_worktree("squashed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    git(&fx.publisher, &["pull", "-q", "--ff-only"]);
    std::fs::copy(
        worktree.join("feature.txt"),
        fx.publisher.join("feature.txt"),
    )
    .unwrap();
    git(&fx.publisher, &["add", "feature.txt"]);
    git(&fx.publisher, &["commit", "-qm", "squashed"]);
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert_eq!(report.removed[0].proof, "content");
    assert!(!worktree.exists());
}

#[test]
fn a_deep_proof_the_budget_cannot_afford_is_deferred_not_removed() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let session = broker.start_worktree("squashed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    git(&fx.publisher, &["pull", "-q", "--ff-only"]);
    std::fs::copy(
        worktree.join("feature.txt"),
        fx.publisher.join("feature.txt"),
    )
    .unwrap();
    git(&fx.publisher, &["add", "feature.txt"]);
    git(&fx.publisher, &["commit", "-qm", "squashed"]);
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);

    let report = broker
        .auto_cleanup_now(Some(std::time::Instant::now()))
        .unwrap()
        .unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    assert_eq!(report.deferred, 1);
    assert!(worktree.exists());
}

#[test]
fn an_open_session_keeps_its_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, false);
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(worktree.exists());
}

#[test]
fn a_tracked_change_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("feature.txt"), "edited\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("uncommitted or untracked"));
    assert!(worktree.exists());
}

#[test]
fn an_untracked_file_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("notes.md"), "mine\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("notes.md"));
    assert!(worktree.exists());
}

#[test]
fn a_valuable_ignored_file_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join(".env"), "SECRET=1\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("outside the regenerable set"));
    assert!(worktree.exists());
}

#[test]
fn a_stash_made_on_the_branch_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("feature.txt"), "stashed\n").unwrap();
    git(&worktree, &["stash", "-q"]);
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("stash"));
    assert!(worktree.exists());
}

#[test]
fn a_rebase_in_progress_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let git_dir = git_out(&worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::create_dir_all(Path::new(&git_dir).join("rebase-merge")).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("rebase"));
    assert!(worktree.exists());
}

#[test]
fn a_commit_not_on_the_remote_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let session = broker.start_worktree("unpushed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("not contained"));
    assert!(worktree.exists());
}

#[test]
fn a_commit_only_in_a_local_branch_is_not_contained() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let session = broker.start_worktree("local only", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    let head = commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    // Local main and the integration branch hold it; the remote does not.
    git(&fx.repo, &["branch", "-f", "aethyme/integration", &head]);
    git(&fx.repo, &["merge", "-q", "--ff-only", &head]);
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("not contained"));
    assert!(worktree.exists());
}

#[test]
fn no_fetched_remote_default_keeps_everything() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    git(
        &fx.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("no fetched remote default branch"));
    assert!(worktree.exists());
}

#[test]
fn a_running_gate_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let run_dir = fx.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(&run_dir).unwrap();
    let pid = std::process::id();
    std::fs::write(
        run_dir.join("cargo-test.pid"),
        format!("{pid} deadbeef {pid}\n"),
    )
    .unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("gate"));
    assert!(worktree.exists());
}

#[test]
fn a_process_in_the_checkout_keeps_it() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let mut child = Command::new("sleep")
        .arg("30")
        .current_dir(&worktree)
        .spawn()
        .unwrap();
    let report = run(&mut broker);
    let _ = child.kill();
    let _ = child.wait();
    let reason = kept_reason(&report, &worktree);
    assert!(
        reason.starts_with("a process has") && reason.ends_with("open"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn closing_releases_leases_so_none_can_hold_a_closed_checkout() {
    // `close` deletes the session's leases in the same transaction, so a
    // closed checkout never has a holding lease of its own. The lease proof
    // stays as a guard; this pins the invariant it relies on.
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, false);
    broker.claim_lease(session_id, "feature.txt", None).unwrap();
    broker.close(session_id).unwrap();
    assert!(
        broker
            .store()
            .session_leases(session_id)
            .unwrap()
            .is_empty()
    );
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[test]
fn a_live_session_that_adopted_the_closed_checkout_keeps_it() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let adopted = broker.adopt(&worktree, Some("picked up again")).unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree).contains(&format!("session {} is still open", adopted.id)),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn a_change_between_selection_and_removal_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    std::fs::write(worktree.join("late.txt"), "written after selection\n").unwrap();
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("changed before removal"));
    assert!(worktree.exists());
}

#[test]
fn a_head_moved_after_selection_is_proved_again_not_reused() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    // A commit no remote has: the selection's proof names the old HEAD and
    // must not vouch for this one.
    commit(&worktree, "late.txt", "committed after selection\n", "late");
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("changed before removal"));
    assert!(worktree.exists());
}

#[test]
fn removal_past_the_selection_deadline_defers_all_but_the_first() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, first, _) = merged_closed_session(&fx, &mut broker, true);
    let (_, second, _) = merged_closed_session(&fx, &mut broker, true);
    // Selection by ancestry is quick; the deadline then passes before
    // removal, as it does when a broker open spent its budget selecting.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let plan = broker
        .auto_cleanup_plan_now(Some(deadline))
        .unwrap()
        .unwrap();
    assert_eq!(plan.selected_worktrees().len(), 2, "selection outran 2 s");
    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert_eq!(report.deferred, 1, "{report:#?}");
    assert_ne!(first.exists(), second.exists());
    // Unbounded, the next pass removes the one left.
    assert_eq!(run(&mut broker).removed.len(), 1);
    assert!(!first.exists() && !second.exists());
}

#[test]
fn auto_remove_false_disables_it_and_keep_pins_and_regenerable_globs_apply() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(fx.repo.join(".aethyme")).unwrap();
    let config = fx.repo.join(".aethyme/config.toml");

    std::fs::write(&config, "[cleanup]\nauto_remove = false\n").unwrap();
    let report = run(&mut broker);
    assert!(!report.enabled);
    assert!(report.removed.is_empty());
    assert!(worktree.exists());

    let name = worktree.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(&config, format!("[cleanup]\nkeep = [\"{name}\"]\n")).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("pinned"));
    assert!(worktree.exists());

    // An ignored cache directory outside the built-in set keeps it until it
    // is declared regenerable.
    git(
        &fx.repo,
        &[
            "config",
            "core.excludesFile",
            fx.repo.join("excludes").to_str().unwrap(),
        ],
    );
    std::fs::write(fx.repo.join("excludes"), ".gradle-cache/\n").unwrap();
    std::fs::create_dir_all(worktree.join(".gradle-cache")).unwrap();
    std::fs::write(worktree.join(".gradle-cache/x"), "x").unwrap();
    std::fs::write(&config, "[cleanup]\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains(".gradle-cache"));
    std::fs::write(
        &config,
        "[cleanup]\nregenerable = [\".gradle-cache/**\", \".gradle-cache\"]\n",
    )
    .unwrap();
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[test]
fn the_primary_checkout_and_non_broker_worktrees_are_never_removed() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    // A worktree the broker did not create, adopted and closed.
    let foreign = fx.repo.parent().unwrap().join("foreign");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            foreign.to_str().unwrap(),
            "-b",
            "foreign",
        ],
    );
    let adopted = broker.adopt(&foreign, Some("foreign")).unwrap();
    broker.close(adopted.id).unwrap();
    // The primary checkout itself, adopted and closed.
    let primary = broker.adopt(&fx.repo, Some("primary")).unwrap();
    broker.close(primary.id).unwrap();

    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(foreign.exists());
    assert!(fx.repo.exists());
    assert!(kept_reason(&report, &foreign).contains("not a broker-owned worktree"));
}

/// Ignore `pattern` in every checkout of the fixture's repository.
fn ignore(fx: &Fixture, pattern: &str) {
    let exclude = fx.repo.join(".git/info/exclude");
    let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
    text.push_str(pattern);
    text.push('\n');
    std::fs::write(exclude, text).unwrap();
}

#[test]
fn ignored_code_under_build_without_a_manifest_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    ignore(&fx, "build/");
    std::fs::create_dir_all(worktree.join("build")).unwrap();
    std::fs::write(worktree.join("build/generate.py"), "print('mine')\n").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("not inside a recognised build-output directory"),
        "{report:#?}"
    );
    assert!(worktree.join("build/generate.py").exists());
}

#[test]
fn a_prefix_named_directory_is_not_build_output() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    ignore(&fx, "target-notes/");
    std::fs::create_dir_all(worktree.join("target-notes")).unwrap();
    std::fs::write(worktree.join("target-notes/plan.md"), "notes\n").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("target-notes"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_leaving_the_worktree_keeps_it() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("precious.txt"), "keep\n").unwrap();
    std::fs::create_dir_all(worktree.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), worktree.join("node_modules/linked")).unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("outside the worktree"),
        "{report:#?}"
    );
    assert!(elsewhere.path().join("precious.txt").exists());
}

#[test]
fn a_regenerable_glob_that_could_match_a_secret_is_rejected() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(fx.repo.join(".aethyme")).unwrap();
    std::fs::write(
        fx.repo.join(".aethyme/config.toml"),
        "[cleanup]\nregenerable = [\".env*\"]\n",
    )
    .unwrap();
    let report = run(&mut broker);
    assert!(!report.enabled, "{report:#?}");
    assert!(
        report
            .config_error
            .as_deref()
            .unwrap_or_default()
            .contains(".env")
    );
    assert!(report.removed.is_empty());
    assert!(worktree.exists());
}

#[test]
fn an_unreadable_checkout_status_keeps_it() {
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    // A corrupt index makes `git status` fail: not knowing is not clean.
    let git_dir = git_out(&worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::write(Path::new(&git_dir).join("index"), "not an index").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("could not be checked"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn a_live_session_recorded_through_a_symlinked_root_keeps_the_checkout() {
    // The host-state root reached through a symlink (`/tmp` against
    // `/private/tmp`) records a live session's worktree under a different
    // spelling than the closed one. It is still the same checkout.
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let alias_root = fx.repo.parent().unwrap().join("host-state-alias");
    std::os::unix::fs::symlink(worktree.parent().unwrap(), &alias_root).unwrap();
    let alias = alias_root.join(worktree.file_name().unwrap());
    let live = broker
        .store()
        .register_session(&aethyme_broker::NewSession {
            worktree_path: alias.to_string_lossy().into_owned(),
            branch: "live-through-alias".into(),
            origin: aethyme_broker::SessionOrigin::Adopted,
            task: Some("working through the alias".into()),
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree).contains(&format!("session {} is still open", live.id)),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn an_unclaimed_worktree_is_never_removed() {
    // A worktree no session record names -- a stray `git worktree add`, or a
    // live session's own checkout the broker failed to match -- has no owner
    // the proofs can be about, so it is never a candidate.
    let fx = fixture();
    let mut broker = Broker::open(&fx.repo).unwrap();
    let (_, owned, _) = merged_closed_session(&fx, &mut broker, true);
    let stray = owned.parent().unwrap().join("unclaimed-stray");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            stray.to_str().unwrap(),
            "origin/main",
        ],
    );
    let report = run(&mut broker);
    assert!(stray.exists(), "{report:#?}");
    let stray_text = std::fs::canonicalize(&stray)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        !report
            .removed
            .iter()
            .any(|removed| removed.worktree == stray_text),
        "{report:#?}"
    );
}
