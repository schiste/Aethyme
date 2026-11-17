use super::*;

use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
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

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_string()).collect()
}

fn store(dir: &tempfile::TempDir) -> crate::BrokerStore {
    crate::BrokerStore::open(&dir.path().join("broker.db")).unwrap()
}

/// A repository on `main` tracking `origin/main` at HEAD, whose origin URL
/// does not exist, so every fetch fails.
fn offline_repository() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n/broker.db*\n").unwrap();
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    let missing = root.join("no-such-remote.git");
    git(
        root,
        &["config", "remote.origin.url", missing.to_str().unwrap()],
    );
    git(
        root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(root, &["config", "branch.main.remote", "origin"]);
    git(root, &["config", "branch.main.merge", "refs/heads/main"]);
    tmp
}

#[test]
fn config_defaults_and_overrides() {
    assert_eq!(parse_config(None), (SyncConfig::default(), None));
    let table: toml::Value = toml::from_str(
        "auto_sync = false\nsync_interval_minutes = 0\nbehind_threshold_commits = 5\nbehind_threshold_hours = 2\n",
    )
    .unwrap();
    let (config, problem) = parse_config(Some(&table));
    assert_eq!(problem, None);
    assert_eq!(
        config,
        SyncConfig {
            auto_sync: false,
            sync_interval_minutes: 0,
            behind_threshold_commits: 5,
            behind_threshold_hours: 2,
            allow_filters: false,
        }
    );
    assert_eq!(
        config.interval_ms(),
        None,
        "auto_sync off disables the timer"
    );
    let zero = SyncConfig {
        sync_interval_minutes: 0,
        ..SyncConfig::default()
    };
    assert_eq!(zero.interval_ms(), None, "0 minutes disables the timer");
    assert_eq!(SyncConfig::default().interval_ms(), Some(15 * 60_000));

    let wrong: toml::Value =
        toml::from_str("auto_sync = \"yes\"\nsync_interval_minutes = -3\n").unwrap();
    let (config, problem) = parse_config(Some(&wrong));
    assert_eq!(
        config,
        SyncConfig::default(),
        "a wrong key keeps its default"
    );
    let problem = problem.unwrap();
    assert!(problem.contains("auto_sync"), "{problem}");
    assert!(problem.contains("sync_interval_minutes"), "{problem}");
}

#[test]
fn only_the_brokers_own_files_are_broker_owned() {
    for owned in [
        ".aethyme/broker.db",
        ".aethyme/run/gc-lock-waiters/12",
        ".aethyme/logs/x.log",
        ".aethyme/gc.lock",
        ".aethyme/gc-journal-archives/gc-journal-abandoned-1.json",
        ".aethyme/.gc.lock.21193.1794768075467388000",
        ".aethyme/.operator-v0.8.51-maintenance-hold-1689-07d7c92fa5404c30827a/manifest.json",
        ".aethyme/worktrees/session/file",
        ".aethyme/reviews/pr-1/security/file",
    ] {
        assert!(is_broker_owned_path(owned), "{owned}");
    }
    for user in [
        ".aethyme/config.toml",
        ".aethyme/overrides/agents.json",
        ".aethyme/broker.toml",
        ".aethyme/runtime.env",
        ".aethyme/.gc.lock.abc.1",
        ".aethyme/.gc.lock.12",
        ".aethyme/.operator-notes",
        "src/.gc.lock.1.2",
        "README.md",
    ] {
        assert!(!is_broker_owned_path(user), "{user}");
    }
}

#[test]
fn overlap_names_shared_tracked_paths_and_untracked_collisions() {
    let changed = strings(&["src/app.rs", "docs/new.md", "assets/logo.png", "lib"]);
    // A tracked change elsewhere, a tracked change on an updated path, a
    // staged rename whose source the update also changes.
    let tracked = strings(&["README.md", "src/app.rs", "assets/logo.png"]);
    let untracked = strings(&[
        "notes.txt",     // unrelated
        "docs/new.md",   // the update creates this exact path
        "DOCS/NEW.MD",   // same path on a case-insensitive filesystem
        "lib/extra.rs",  // the update writes a file where this directory is
        "src",           // a file where the update needs a directory
        "docs/other.md", // a sibling of an updated path: no collision
    ]);
    assert_eq!(
        overlapping_paths(&changed, &tracked, &untracked),
        strings(&[
            "DOCS/NEW.MD",
            "assets/logo.png",
            "docs/new.md",
            "lib/extra.rs",
            "src",
            "src/app.rs",
        ])
    );
    assert!(overlapping_paths(&changed, &strings(&["README.md"]), &strings(&["x"])).is_empty());
}

fn record(
    behind: u64,
    last_sync_at: Option<i64>,
    reason: Option<SyncReason>,
) -> MainCheckoutStatus {
    MainCheckoutStatus {
        behind: Some(behind),
        ahead: Some(0),
        last_sync_at,
        reason,
        upstream_ref: Some("origin/main".into()),
        ..MainCheckoutStatus::default()
    }
}

#[test]
fn advice_fires_past_the_commit_or_age_threshold() {
    let config = SyncConfig::default();
    let hour = 3_600_000;
    let now = 1_000 * hour;
    let overlap = Some(SyncReason::LocalChangesOverlap);
    assert_eq!(
        behind_verdict(&record(50, Some(now), overlap), config, now),
        None
    );
    assert_eq!(
        behind_verdict(&record(51, Some(now), overlap), config, now),
        Some("local_changes_overlap")
    );
    assert_eq!(
        behind_verdict(&record(3, Some(now - 23 * hour), overlap), config, now),
        None
    );
    assert_eq!(
        behind_verdict(&record(3, Some(now - 25 * hour), overlap), config, now),
        Some("local_changes_overlap")
    );
    assert_eq!(
        behind_verdict(&record(0, Some(now - 100 * hour), None), config, now),
        None,
        "a current checkout is never behind"
    );
    let tight = SyncConfig {
        behind_threshold_commits: 2,
        ..config
    };
    assert_eq!(
        behind_verdict(
            &record(3, Some(now), Some(SyncReason::LocalCommits)),
            tight,
            now
        ),
        Some("local_commits")
    );
    let mut failing = record(0, Some(now), None);
    failing.fetch_failures = 3;
    failing.last_fetch_at = Some(now - 30 * hour);
    assert_eq!(behind_verdict(&failing, config, now), Some("fetch_failed"));
    failing.last_fetch_at = Some(now - hour);
    assert_eq!(behind_verdict(&failing, config, now), None);
    failing.last_fetch_at = None;
    failing.fetch_failing_since = Some(now - hour);
    assert_eq!(
        behind_verdict(&failing, config, now),
        None,
        "one failed fetch is not an alarm"
    );
    failing.fetch_failing_since = Some(now - 25 * hour);
    assert_eq!(behind_verdict(&failing, config, now), Some("fetch_failed"));
}

#[test]
fn a_held_attempt_records_reason_paths_and_keeps_the_last_sync_time() {
    let paths = (0..13).map(|n| format!("file-{n:02}")).collect::<Vec<_>>();
    let attempt = Attempt {
        outcome: SyncOutcome::Held(Hold {
            reason: SyncReason::LocalChangesOverlap,
            message: "13 locally changed path(s)".into(),
            command: "git status".into(),
            paths,
        }),
        observed: Observed {
            behind: Some(7),
            ..Observed::default()
        },
        overlap_check: None,
        fast_forward: None,
    };
    let before = MainCheckoutStatus {
        last_sync_at: Some(5),
        ..MainCheckoutStatus::default()
    };
    let state = apply_attempt(before, &attempt, "broker.worker", 100);
    assert_eq!(state.last_result.as_deref(), Some("held"));
    assert_eq!(state.reason, Some(SyncReason::LocalChangesOverlap));
    assert_eq!(state.blocking_paths.len(), BLOCKING_PATH_LIMIT);
    assert_eq!(state.blocking_paths_omitted, 3);
    assert_eq!(state.last_sync_at, Some(5));
    assert_eq!(state.last_attempt_at, Some(100));
    assert_eq!(state.behind_since, Some(100));
    let again = apply_attempt(state, &attempt, "broker.worker", 200);
    assert_eq!(again.behind_since, Some(100), "first seen behind is kept");

    let advanced = Attempt {
        outcome: SyncOutcome::Advanced {
            from: "a".into(),
            to: "b".into(),
            commits: 7,
            remaining: None,
        },
        observed: Observed {
            behind: Some(0),
            ..Observed::default()
        },
        overlap_check: None,
        fast_forward: None,
    };
    let synced = apply_attempt(again, &advanced, "broker.status", 300);
    assert_eq!(synced.last_result.as_deref(), Some("advanced"));
    assert_eq!(synced.last_sync_at, Some(300));
    assert_eq!(synced.reason, None);
    assert!(synced.blocking_paths.is_empty());
    assert_eq!(synced.behind_since, None);
}

#[test]
fn the_timer_slot_is_single_flight_and_switchable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp);
    let root = tmp.path();
    let minute = 60_000;
    assert!(claim_timer_slot(&store, root, 1_000));
    assert!(!claim_timer_slot(&store, root, 1_000), "claimed once");
    assert!(!claim_timer_slot(&store, root, 1_000 + 14 * minute));
    assert!(take_timer_request(&store), "the claim left one request");
    assert!(!take_timer_request(&store), "served once");
    assert!(claim_timer_slot(&store, root, 1_000 + 15 * minute));

    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[main_checkout]\nsync_interval_minutes = 0\n",
    )
    .unwrap();
    assert!(!claim_timer_slot(&store, root, 1_000 + 60 * minute));
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[main_checkout]\nauto_sync = false\n",
    )
    .unwrap();
    assert!(!claim_timer_slot(&store, root, 1_000 + 60 * minute));
}

#[test]
fn a_failed_fetch_is_recorded_and_backs_off() {
    let tmp = offline_repository();
    let root = tmp.path();
    let store = store(&tmp);
    let now = 10_000_000;

    assert!(claim_timer_slot(&store, root, now));
    let first = run_timer_lane(&store, root, now);
    assert!(
        first.ran && first.fetch_attempted && first.fetch_failed,
        "{first:?}"
    );
    assert_eq!(first.outcome, Some(SyncOutcome::Current), "{first:?}");
    let state = load_state(&store).unwrap();
    assert_eq!(state.fetch_failures, 1);
    assert!(state.fetch_error.is_some());
    assert_eq!(state.fetch_failing_since, Some(now));
    let next = state.next_fetch_at.unwrap();
    assert!(next >= now + 15 * 60_000, "{next}");
    assert_eq!(state.last_result.as_deref(), Some("current"));

    // Requested again before the backoff ends: no fetch, still an attempt.
    store.meta_set(REQUEST_META_KEY, "1").unwrap();
    let second = run_timer_lane(&store, root, now + 60_000);
    assert!(second.ran && !second.fetch_attempted, "{second:?}");
    assert_eq!(load_state(&store).unwrap().fetch_failures, 1);

    // After the backoff, it fetches again and the failures add up.
    store.meta_set(REQUEST_META_KEY, "1").unwrap();
    let third = run_timer_lane(&store, root, next);
    assert!(third.fetch_attempted && third.fetch_failed, "{third:?}");
    let state = load_state(&store).unwrap();
    assert_eq!(state.fetch_failures, 2);
    assert!(
        state.next_fetch_at.unwrap() - next > next - now,
        "backoff grows"
    );

    // Without a request the lane does nothing.
    let idle = run_timer_lane(&store, root, next + 1);
    assert!(!idle.ran);
}

#[test]
fn the_timer_lane_does_nothing_when_disabled() {
    let tmp = offline_repository();
    let root = tmp.path();
    let store = store(&tmp);
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[main_checkout]\nauto_sync = false\n",
    )
    .unwrap();
    store.meta_set(REQUEST_META_KEY, "1").unwrap();
    let report = run_timer_lane(&store, root, 1);
    assert!(!report.ran && !report.fetch_attempted, "{report:?}");
    assert!(load_state(&store).is_none());
}

fn set_mtime(path: &Path, when: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn staging_name(pid: u32, created: SystemTime) -> String {
    let nanos = created
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(".gc.lock.{pid}.{nanos}")
}

#[test]
fn staged_gc_lock_files_are_swept_only_when_provably_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join(".aethyme");
    std::fs::create_dir_all(&state).unwrap();
    let now = SystemTime::now();
    // Just past the one-hour floor, and after PID 1 started.
    let old = now - Duration::from_secs(61 * 60);
    let dead = dead_pid();
    // PID 1 is alive and is not this test, so it only "created" the file.
    let live = 1;

    let stale = state.join(staging_name(dead, old));
    let young = state.join(staging_name(dead, now));
    let alive = state.join(staging_name(live, old));
    for path in [&stale, &young, &alive] {
        std::fs::write(path, "1\n").unwrap();
    }
    set_mtime(&stale, old);
    set_mtime(&alive, old);
    // Not litter: left alone whatever its age.
    std::fs::write(state.join("config.toml"), "").unwrap();
    std::fs::write(state.join("runtime.env"), "").unwrap();

    let sweep = sweep_broker_litter(tmp.path(), now);
    assert_eq!(sweep.swept, 1, "{sweep:?}");
    assert!(!stale.exists(), "dead PID, old enough, lock not held");
    assert!(young.exists() && alive.exists());
    assert_eq!(sweep.kept_by_reason.get("too_young"), Some(&1), "{sweep:?}");
    assert_eq!(sweep.kept_by_reason.get("pid_alive"), Some(&1), "{sweep:?}");
    assert!(state.join("config.toml").exists() && state.join("runtime.env").exists());

    // A staged file of the live process that holds `gc.lock`: kept.
    std::fs::remove_file(&alive).unwrap();
    std::fs::remove_file(&young).unwrap();
    let own = std::process::id();
    let held = state.join(staging_name(own, old));
    std::fs::write(&held, "1\n").unwrap();
    set_mtime(&held, old);
    std::fs::write(state.join("gc.lock"), format!("{own}\n")).unwrap();
    let sweep = sweep_broker_litter(tmp.path(), now);
    assert_eq!(sweep.swept, 0, "{sweep:?}");
    assert_eq!(sweep.kept_by_reason.get("lock_held"), Some(&1), "{sweep:?}");
    assert!(held.exists());
}

fn write_hold(state: &Path, name: &str, phase: &str, repository: &Path) -> std::path::PathBuf {
    let hold = state.join(name);
    std::fs::create_dir_all(&hold).unwrap();
    std::fs::write(
        hold.join("manifest.json"),
        serde_json::json!({
            "changed": 1,
            "phase": phase,
            "plan_sha256": "00",
            "repository": repository,
            "schema_version": 1,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(hold.join("item-01.json"), "{}").unwrap();
    hold
}

#[test]
fn completed_maintenance_holds_are_swept_after_a_day_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state = root.join(".aethyme");
    std::fs::create_dir_all(&state).unwrap();
    let now = SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);

    let complete = write_hold(
        &state,
        ".operator-v1.2.3-maintenance-hold-7-aa",
        "complete",
        root,
    );
    let running = write_hold(
        &state,
        ".operator-v1.2.4-maintenance-hold-8-bb",
        "applying",
        root,
    );
    let foreign = write_hold(
        &state,
        ".operator-v1.2.5-maintenance-hold-9-cc",
        "complete",
        &root.join("elsewhere"),
    );
    let extra = write_hold(
        &state,
        ".operator-v1.2.6-maintenance-hold-10-dd",
        "complete",
        root,
    );
    std::fs::write(extra.join("notes.txt"), "keep").unwrap();

    let sweep = sweep_broker_litter(root, now);
    assert_eq!(sweep.swept, 1, "{sweep:?}");
    assert!(!complete.exists());
    assert!(running.exists() && foreign.exists() && extra.exists());
    assert_eq!(
        sweep.kept_by_reason.get("hold_incomplete"),
        Some(&1),
        "{sweep:?}"
    );
    assert_eq!(
        sweep.kept_by_reason.get("foreign_repository"),
        Some(&1),
        "{sweep:?}"
    );
    assert_eq!(
        sweep.kept_by_reason.get("unknown_content"),
        Some(&1),
        "{sweep:?}"
    );

    let recent = write_hold(
        &state,
        ".operator-v1.2.7-maintenance-hold-11-ee",
        "complete",
        root,
    );
    let sweep = sweep_broker_litter(root, SystemTime::now());
    assert!(recent.exists());
    assert_eq!(
        sweep.kept_by_reason.get("hold_recent"),
        Some(&1),
        "{sweep:?}"
    );
}

/// An offline repository whose `base` files are committed at HEAD (`from`),
/// with `origin/main` at `to`: one commit per `upstream` file on top.
fn behind_repository(
    base: &[(&str, &str)],
    upstream: &[(&str, &str)],
) -> (tempfile::TempDir, String, String) {
    let tmp = offline_repository();
    let root = tmp.path();
    if !base.is_empty() {
        for (path, body) in base {
            std::fs::write(root.join(path), body).unwrap();
            git(root, &["add", "-f", path]);
        }
        git(root, &["commit", "-qm", "base"]);
    }
    let from = git(root, &["rev-parse", "HEAD"]);
    for (path, body) in upstream {
        std::fs::write(root.join(path), body).unwrap();
        git(root, &["add", "-f", path]);
        git(root, &["commit", "-qm", &format!("upstream {path}")]);
    }
    let to = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &to]);
    git(root, &["reset", "-q", "--hard", &from]);
    (tmp, from, to)
}

fn dirty_options() -> AttemptOptions {
    AttemptOptions::foreground(SyncConfig::default(), true)
}

fn head(root: &Path) -> String {
    git(root, &["rev-parse", "HEAD"])
}

fn dead_marker(from: &str, to: &str) -> MoveMarker {
    MoveMarker {
        schema_version: 1,
        branch: "main".into(),
        from: from.into(),
        to: to.into(),
        started_at: 1,
        pid: dead_pid(),
    }
}

#[test]
fn an_interruption_after_the_tree_update_is_finished_by_the_next_attempt() {
    let (tmp, from, to) = behind_repository(&[], &[("up.txt", "up\n")]);
    let root = tmp.path();
    std::fs::write(root.join("README.md"), "local edit\n").unwrap();
    let checkout = GitRepo::discover(root).unwrap();

    let step = move_step(&checkout, root, "main", &from, &to, &mut || false);
    assert_eq!(step, StepResult::Interrupted);
    assert_eq!(head(root), from, "the ref moves last");
    assert_eq!(std::fs::read(root.join("up.txt")).unwrap(), b"up\n");
    assert!(
        marker_path(root).exists(),
        "the marker survives the interruption"
    );
    assert!(interrupted_advice(root).is_some());

    let attempt = attempt(&checkout, root, dirty_options());
    assert_eq!(attempt.outcome, SyncOutcome::Current, "{attempt:?}");
    assert_eq!(head(root), to, "recovery finished the move");
    assert!(!marker_path(root).exists());
    assert!(interrupted_advice(root).is_none());
    assert_eq!(
        std::fs::read(root.join("README.md")).unwrap(),
        b"local edit\n"
    );
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(git(root, &["status", "--porcelain"]), "M README.md");
}

#[test]
fn a_marker_from_before_the_tree_update_is_dropped_and_the_move_retried() {
    let (tmp, from, to) = behind_repository(&[], &[("up.txt", "up\n")]);
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    assert!(write_marker_new(root, &dead_marker(&from, &to)).unwrap());
    let checkout = GitRepo::discover(root).unwrap();

    let attempt = attempt(&checkout, root, dirty_options());
    assert!(
        matches!(&attempt.outcome, SyncOutcome::Advanced { to: reached, remaining: None, .. } if *reached == to),
        "{attempt:?}"
    );
    assert_eq!(head(root), to);
    assert!(!marker_path(root).exists());
}

#[test]
fn a_partly_written_move_is_held_with_its_marker_and_advice() {
    let (tmp, from, to) = behind_repository(&[], &[("one.txt", "one\n"), ("two.txt", "two\n")]);
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    assert!(write_marker_new(root, &dead_marker(&from, &to)).unwrap());
    // A tree update cut off after writing one of its two files.
    std::fs::write(root.join("one.txt"), "one\n").unwrap();
    let checkout = GitRepo::discover(root).unwrap();
    assert_eq!(
        classify_tree(&checkout, root, &from, &to).unwrap(),
        TreeState::Partial
    );

    let first = attempt(&checkout, root, dirty_options());
    match &first.outcome {
        SyncOutcome::Held(hold) => assert_eq!(hold.reason, SyncReason::Interrupted, "{hold:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(head(root), from);
    assert!(
        marker_path(root).exists(),
        "an unproven state keeps the marker"
    );
    let advice = interrupted_advice(root).unwrap();
    assert_eq!(advice.id, INTERRUPTED_ADVICE_ID);
    assert!(advice.commands.iter().any(|c| c.contains("rev-parse HEAD")));
    assert!(
        advice
            .commands
            .iter()
            .any(|c| c.contains(&format!("diff --stat {from} {to}")))
    );
    // Still held on the next attempt: never Current, never a new move.
    let again = attempt(&checkout, root, dirty_options());
    assert!(matches!(again.outcome, SyncOutcome::Held(_)), "{again:?}");
}

#[test]
fn a_concurrent_commit_stops_the_move_and_the_tree_goes_back() {
    let (tmp, from, to) = behind_repository(&[], &[("up.txt", "up\n")]);
    let root = tmp.path();
    std::fs::write(root.join("README.md"), "local edit\n").unwrap();
    let checkout = GitRepo::discover(root).unwrap();

    // Between the check and the move.
    let tree = git(root, &["rev-parse", &format!("{from}^{{tree}}")]);
    let early = git(root, &["commit-tree", &tree, "-p", &from, "-m", "early"]);
    git(root, &["update-ref", "refs/heads/main", &early]);
    let step = move_step(&checkout, root, "main", &from, &to, &mut || true);
    assert!(matches!(step, StepResult::Refused(_)), "{step:?}");
    assert_eq!(head(root), early);
    assert!(!root.join("up.txt").exists());
    assert!(!marker_path(root).exists());
    git(root, &["update-ref", "refs/heads/main", &from]);

    // Between the tree update and the ref swap.
    let mut concurrent = String::new();
    let step = move_step(&checkout, root, "main", &from, &to, &mut || {
        concurrent = git(root, &["commit-tree", &tree, "-p", &from, "-m", "late"]);
        git(root, &["update-ref", "refs/heads/main", &concurrent]);
        true
    });
    assert!(matches!(step, StepResult::Refused(_)), "{step:?}");
    assert_eq!(head(root), concurrent, "the other writer's ref is kept");
    assert!(!root.join("up.txt").exists(), "the tree went back");
    assert!(!marker_path(root).exists());
    assert_eq!(
        std::fs::read(root.join("README.md")).unwrap(),
        b"local edit\n"
    );
    assert_eq!(git(root, &["status", "--porcelain"]), "M README.md");
}

#[test]
fn a_large_jump_advances_in_bounded_steps() {
    let (tmp, from, to) =
        behind_repository(&[], &[("a.txt", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")]);
    let root = tmp.path();
    let checkout = GitRepo::discover(root).unwrap();
    let chain = checkout.fast_forward_steps(&from, &to).unwrap();
    assert_eq!(chain.len(), 3);
    let (target, changes) = plan_step(&checkout, &from, &to, 1).unwrap();
    assert_eq!(target, chain[0]);
    assert_eq!(changes, vec![(b'A', "a.txt".to_string())]);
    let (target, _) = plan_step(&checkout, &from, &to, 2).unwrap();
    assert_eq!(target, chain[1]);
    assert_eq!(plan_step(&checkout, &from, &to, 3).unwrap().0, to);

    let options = AttemptOptions {
        step_max_paths: 1,
        max_steps: 2,
        ..dirty_options()
    };
    let first = attempt(&checkout, root, options);
    assert!(
        matches!(&first.outcome, SyncOutcome::Advanced { to: reached, commits: 2, .. } if *reached == chain[1]),
        "{first:?}"
    );
    assert_eq!(first.observed.behind, Some(1));
    let second = attempt(&checkout, root, options);
    assert!(
        matches!(&second.outcome, SyncOutcome::Advanced { to: reached, .. } if *reached == to),
        "{second:?}"
    );
    assert!(!marker_path(root).exists());
}

#[test]
fn filters_on_updated_paths_hold_the_move_unless_allowed() {
    let (tmp, _from, to) = behind_repository(
        &[(".gitattributes", "*.bin filter=demo\n")],
        &[("data.bin", "payload\n")],
    );
    let root = tmp.path();
    git(root, &["config", "filter.demo.smudge", "cat"]);
    git(root, &["config", "filter.demo.clean", "cat"]);
    let checkout = GitRepo::discover(root).unwrap();

    let held = attempt(&checkout, root, dirty_options());
    match &held.outcome {
        SyncOutcome::Held(hold) => assert_eq!(hold.reason, SyncReason::FiltersConfigured),
        other => panic!("{other:?}"),
    }
    assert!(!root.join("data.bin").exists());

    let allowed = AttemptOptions {
        allow_filters: true,
        ..dirty_options()
    };
    let moved = attempt(&checkout, root, allowed);
    assert!(
        matches!(moved.outcome, SyncOutcome::Advanced { .. }),
        "{moved:?}"
    );
    assert_eq!(head(root), to);
}

#[test]
fn an_ignored_file_where_the_update_writes_holds_the_move() {
    let (tmp, from, _to) = behind_repository(
        &[(".gitignore", "/.aethyme/\n/broker.db*\n*.cache\n")],
        &[("data.cache", "upstream\n")],
    );
    let root = tmp.path();
    std::fs::write(root.join("data.cache"), "local ignored\n").unwrap();
    let checkout = GitRepo::discover(root).unwrap();

    let attempt = attempt(&checkout, root, dirty_options());
    match &attempt.outcome {
        SyncOutcome::Held(hold) => {
            assert_eq!(hold.reason, SyncReason::LocalChangesOverlap);
            assert_eq!(hold.paths, vec!["data.cache".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(head(root), from);
    assert_eq!(
        std::fs::read(root.join("data.cache")).unwrap(),
        b"local ignored\n"
    );
}

#[test]
fn status_moves_only_a_clean_checkout() {
    let (tmp, from, to) = behind_repository(&[], &[("up.txt", "up\n")]);
    let root = tmp.path();
    std::fs::write(root.join("notes.txt"), "local\n").unwrap();
    let checkout = GitRepo::discover(root).unwrap();
    let read_only = AttemptOptions::foreground(SyncConfig::default(), false);

    let attempt_dirty = attempt(&checkout, root, read_only);
    match &attempt_dirty.outcome {
        SyncOutcome::Held(hold) => assert_eq!(hold.reason, SyncReason::DeferredToBackground),
        other => panic!("{other:?}"),
    }
    assert_eq!(head(root), from);

    std::fs::remove_file(root.join("notes.txt")).unwrap();
    let attempt_clean = attempt(&checkout, root, read_only);
    assert!(
        matches!(attempt_clean.outcome, SyncOutcome::Advanced { .. }),
        "{attempt_clean:?}"
    );
    assert_eq!(head(root), to);
}
