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
