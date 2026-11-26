//! The artifact max age outside the ledger's classes, on temporary roots.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use super::*;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const OLD: &str = "aaaaaaaaaaaaaaaa";
const NEW: &str = "bbbbbbbbbbbbbbbb";

fn write(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn set_times(path: &Path, when: SystemTime) {
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(when).set_accessed(when))
        .unwrap();
}

/// Set the modification and access times of `path` and everything in it.
fn age_tree(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let mut pending = vec![path.to_path_buf()];
    let mut all = Vec::new();
    while let Some(next) = pending.pop() {
        if fs::symlink_metadata(&next).unwrap().is_dir() {
            pending.extend(
                fs::read_dir(&next)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            );
        }
        all.push(next);
    }
    for entry in all.iter().rev() {
        set_times(entry, when);
    }
}

/// A unit of `package` with `hash` in the profile: fingerprint, rlib and a
/// dep-info naming a local source.
fn unit(profile: &Path, package: &str, hash: &str) {
    let fingerprint = profile
        .join(".fingerprint")
        .join(format!("{package}-{hash}"));
    write(&fingerprint.join(format!("lib-{package}")), b"1");
    write(&fingerprint.join(format!("lib-{package}.json")), b"{}");
    write(
        &profile
            .join("deps")
            .join(format!("lib{package}-{hash}.rlib")),
        &[7_u8; 4096],
    );
    write(
        &profile.join("deps").join(format!("{package}-{hash}.d")),
        format!("/t/deps/lib{package}-{hash}.rlib: crates/{package}/src/lib.rs\n").as_bytes(),
    );
}

/// A Cargo target holding one built unit.
fn cargo_target(target: &Path) -> PathBuf {
    write(
        &target.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n# cargo\n",
    );
    write(&target.join(".rustc_info.json"), b"{}");
    let profile = target.join("debug");
    write(&profile.join(".cargo-lock"), b"");
    unit(&profile, "work", NEW);
    profile
}

fn budgets() -> Budgets {
    Budgets::default()
}

fn run_on(roots: &[Root]) -> ArtifactAgeReport {
    run(
        roots,
        &budgets(),
        Instant::now() + Duration::from_secs(60),
        Duration::ZERO,
        None,
    )
    .unwrap()
}

#[test]
fn a_cargo_target_goes_once_unused_for_the_max_age_and_only_when_proven() {
    let now = 10 * DAY_MS;
    let facts = |last_use_ms: u64| BuildDirFacts {
        tagged: true,
        untracked: Some(true),
        build_running: false,
        last_use_ms,
    };
    assert_eq!(
        judge_build_dir(facts(now - DAY_MS), now, DAY),
        BuildDirVerdict::Remove
    );
    assert_eq!(
        judge_build_dir(facts(now - DAY_MS + 1), now, DAY),
        BuildDirVerdict::Active
    );
    // A time ahead of the clock is recent.
    assert_eq!(
        judge_build_dir(facts(now + 5), now, DAY),
        BuildDirVerdict::Active
    );
    assert_eq!(
        judge_build_dir(facts(0), now, DAY),
        BuildDirVerdict::Unproven("last_use_unknown")
    );
    // A running build keeps even an old one.
    assert_eq!(
        judge_build_dir(
            BuildDirFacts {
                build_running: true,
                ..facts(1)
            },
            now,
            DAY
        ),
        BuildDirVerdict::BuildRunning
    );
    // Never one that is tracked, or whose tracked state is unknown, or one
    // without Cargo's tag.
    for untracked in [Some(false), None] {
        assert_eq!(
            judge_build_dir(
                BuildDirFacts {
                    untracked,
                    ..facts(1)
                },
                now,
                DAY
            ),
            BuildDirVerdict::Unproven("tracked_or_not_ignored")
        );
    }
    assert_eq!(
        judge_build_dir(
            BuildDirFacts {
                tagged: false,
                ..facts(1)
            },
            now,
            DAY
        ),
        BuildDirVerdict::Unproven("no_cachedir_tag")
    );
}

#[test]
fn a_leftover_root_goes_only_unregistered_empty_aethyme_and_old() {
    let now = 10 * DAY_MS;
    let scan = |only_markers: bool, evidence: bool, newest_ms: u64| {
        Some(MarkerScan {
            only_markers,
            evidence,
            newest_ms,
        })
    };
    let facts = LeftoverFacts {
        registered_live: false,
        quick_test: false,
        marker_repository_exists: Some(false),
        scan: scan(true, true, now - DAY_MS),
    };
    assert_eq!(judge_leftover_root(facts, now, DAY), Ok(()));
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                scan: scan(true, true, now - DAY_MS + 1),
                ..facts
            },
            now,
            DAY
        ),
        Err("recent")
    );
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                registered_live: true,
                ..facts
            },
            now,
            DAY
        ),
        Err("registered")
    );
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                marker_repository_exists: Some(true),
                ..facts
            },
            now,
            DAY
        ),
        Err("repository_exists")
    );
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                scan: scan(false, true, 1),
                ..facts
            },
            now,
            DAY
        ),
        Err("holds_content")
    );
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                marker_repository_exists: None,
                scan: scan(true, false, 1),
                ..facts
            },
            now,
            DAY
        ),
        Err("not_aethyme")
    );
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                scan: None,
                ..facts
            },
            now,
            DAY
        ),
        Err("not_scanned")
    );
    // A quick-test pool needs no marker or evidence.
    assert_eq!(
        judge_leftover_root(
            LeftoverFacts {
                quick_test: true,
                marker_repository_exists: None,
                scan: scan(true, false, 1),
                ..facts
            },
            now,
            DAY
        ),
        Ok(())
    );
}

#[test]
fn only_aethyme_markers_and_empty_directories_count_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let pool = dir.path().join("pool");
    fs::create_dir_all(pool.join(".aethyme/run/merge-sim/one")).unwrap();
    write(&pool.join(".DS_Store"), b"finder");
    write(
        &pool.join(".cargo/config.toml"),
        format!("{BROKER_CARGO_CONFIG_PREFIX}, for the session worktrees beside it.\n").as_bytes(),
    );
    write(&pool.join(".aethyme-reclaim-plan-0123.json"), b"{}");
    let scan = scan_markers(&pool).unwrap();
    assert!(scan.only_markers && scan.evidence, "{scan:?}");

    // A cargo configuration someone else wrote is content.
    write(&pool.join(".cargo/config.toml"), b"[build]\n");
    assert!(!scan_markers(&pool).unwrap().only_markers);
    write(
        &pool.join(".cargo/config.toml"),
        BROKER_CARGO_CONFIG_PREFIX.as_bytes(),
    );
    // Any other file, however deep, is content.
    write(&pool.join(".aethyme/run/merge-sim/one/result.json"), b"{}");
    assert!(!scan_markers(&pool).unwrap().only_markers);
    fs::remove_file(pool.join(".aethyme/run/merge-sim/one/result.json")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/", pool.join("link")).unwrap();
        assert!(!scan_markers(&pool).unwrap().only_markers);
    }
}

#[test]
fn a_held_cargo_lock_means_a_build_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let profile = cargo_target(&dir.path().join("target"));
    assert!(lock_profiles(std::slice::from_ref(&profile)).is_some());
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.join(".cargo-lock"))
        .unwrap();
    held.try_lock().unwrap();
    assert!(lock_profiles(std::slice::from_ref(&profile)).is_none());
    drop(held);
    assert!(lock_profiles(std::slice::from_ref(&profile)).is_some());
}

/// A worktree root with the broker's scratch container.
fn scratch_container(root: &Path) -> PathBuf {
    let container = root.join(crate::session_scratch::SCRATCH_CONTAINER);
    write(
        &container.join(crate::session_scratch::SCRATCH_MARKER),
        b"aethyme session scratch v1\n",
    );
    container
}

fn worktree_root(path: &Path, repositories: Vec<PathBuf>) -> Root {
    Root {
        path: path.to_path_buf(),
        kind: RootKind::WorktreeRoot,
        repositories,
    }
}

/// Scratch targets: an old one goes whole; one in use keeps its newest
/// generation and loses the superseded one unused for the max age; one a
/// build holds stays, however old.
#[test]
fn session_scratch_targets_follow_the_max_age() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pool");
    let container = scratch_container(&root);
    let stale = container.join("s1-abc/target");
    cargo_target(&stale);
    age_tree(&stale, 2 * DAY);

    let active = container.join("s2-abc/build/target");
    let active_profile = cargo_target(&active);
    unit(&active_profile, "work", OLD);
    age_tree(
        &active_profile.join(format!(".fingerprint/work-{OLD}")),
        2 * DAY,
    );

    let held = container.join("s3-abc/target");
    let held_profile = cargo_target(&held);
    age_tree(&held, 2 * DAY);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(held_profile.join(".cargo-lock"))
        .unwrap();
    lock.try_lock().unwrap();

    let report = run_on(&[worktree_root(&root, Vec::new())]);

    assert!(!stale.exists(), "unused for the max age");
    assert!(
        container.join("s1-abc").exists(),
        "the scratch directory stays"
    );
    assert!(
        active_profile
            .join(format!(".fingerprint/work-{NEW}"))
            .exists()
    );
    assert!(
        !active_profile
            .join(format!(".fingerprint/work-{OLD}"))
            .exists()
    );
    assert!(held.exists(), "a build holds it");
    let outcome = &report.classes[SESSION_SCRATCH_BUILD];
    assert_eq!(outcome.found, 3);
    assert_eq!(outcome.removed_for_age, 1);
    assert_eq!(outcome.pruned_for_pile_up, 1);
    assert_eq!(outcome.kept_in_use, 2);
    assert_eq!(report.stop, "done");
    // Nothing staged is left behind.
    for parent in [container.join("s1-abc"), container.join("s2-abc/build")] {
        assert!(
            fs::read_dir(&parent).unwrap().flatten().all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".aethyme-storage-evicting-")),
            "{parent:?}"
        );
    }
    drop(lock);

    // Off: nothing is judged.
    let off = Budgets {
        artifact_max_age_hours: 0,
        ..Budgets::default()
    };
    assert!(run(&[], &off, Instant::now(), Duration::ZERO, None).is_none());
}

/// Without the broker's marker the container is not the broker's.
#[test]
fn a_scratch_container_without_the_marker_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pool");
    let target = root
        .join(crate::session_scratch::SCRATCH_CONTAINER)
        .join("s1-abc/target");
    cargo_target(&target);
    age_tree(&target, 2 * DAY);
    let report = run_on(&[worktree_root(&root, Vec::new())]);
    assert!(target.exists());
    assert!(!report.classes.contains_key(SESSION_SCRATCH_BUILD));
}

fn git(repository: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Main-checkout targets: an ignored, untracked one unused for the max age
/// goes; a tracked one never does.
#[test]
fn main_checkout_targets_go_only_untracked_ignored_and_unused() {
    let dir = tempfile::tempdir().unwrap();
    let ignored = dir.path().join("ignored");
    fs::create_dir_all(&ignored).unwrap();
    git(&ignored, &["init", "-q"]);
    write(&ignored.join(".gitignore"), b"target/\n");
    let nested = ignored.join("packages/rust/target");
    cargo_target(&nested);
    age_tree(&nested, 2 * DAY);

    let tracked = dir.path().join("tracked");
    fs::create_dir_all(&tracked).unwrap();
    git(&tracked, &["init", "-q"]);
    let tracked_target = tracked.join("target");
    cargo_target(&tracked_target);
    git(&tracked, &["add", "-f", "target/CACHEDIR.TAG"]);
    git(&tracked, &["commit", "-qm", "track a target"]);
    age_tree(&tracked_target, 2 * DAY);

    let pool = dir.path().join("pool");
    fs::create_dir_all(&pool).unwrap();
    let report = run_on(&[worktree_root(&pool, vec![ignored.clone(), tracked.clone()])]);

    assert!(!nested.exists(), "ignored, untracked and unused");
    assert!(tracked_target.exists(), "a tracked target is never removed");
    let outcome = &report.classes[MAIN_CHECKOUT_TARGET];
    assert_eq!(outcome.found, 2);
    assert_eq!(outcome.removed_for_age, 1);
    assert_eq!(outcome.kept_unproven, 1);
    assert!(outcome.removed_for_age_bytes > 0);
}

/// Leftover worktree roots: only unregistered, Aethyme's, empty and old.
#[test]
fn leftover_worktree_roots_go_when_empty_unregistered_and_old() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("storage");
    let container = storage.join("worktrees");
    let quick = container.join("aethyme-broker-quick-test-49725-17924098-4f64122b85fd1c82");
    fs::create_dir_all(quick.join(".aethyme/run/merge-sim/one")).unwrap();
    let gone = container.join("gone-0123456789abcdef");
    write(
        &gone.join(crate::broker::WORKTREE_ROOT_MARKER),
        serde_json::json!({"repository_root": dir.path().join("no-such-repository")})
            .to_string()
            .as_bytes(),
    );
    fs::create_dir_all(gone.join(crate::session_scratch::SCRATCH_CONTAINER)).unwrap();
    let repository = dir.path().join("repository");
    fs::create_dir_all(repository.join(".git")).unwrap();
    let exists = container.join("exists-0123456789abcdef");
    write(
        &exists.join(crate::broker::WORKTREE_ROOT_MARKER),
        serde_json::json!({"repository_root": repository})
            .to_string()
            .as_bytes(),
    );
    let content = container.join("content-0123456789abcdef");
    write(&content.join(".aethyme/notes.txt"), b"someone's");
    let foreign = container.join("foreign");
    fs::create_dir_all(foreign.join("empty")).unwrap();
    let registered = container.join("registered-0123456789abcdef");
    fs::create_dir_all(registered.join(".aethyme")).unwrap();
    for path in [&quick, &gone, &exists, &content, &foreign, &registered] {
        age_tree(path, 2 * DAY);
    }
    let recent = container.join("recent-0123456789abcdef");
    fs::create_dir_all(recent.join(".aethyme")).unwrap();

    let report = run_on(&[
        Root::new(&storage, RootKind::Storage),
        worktree_root(&registered, vec![repository.clone()]),
    ]);

    assert!(!quick.exists(), "an old empty quick-test pool");
    assert!(!gone.exists(), "its repository is gone");
    for kept in [&exists, &content, &foreign, &registered, &recent] {
        assert!(kept.exists(), "{kept:?} must stay");
    }
    let outcome = &report.classes[LEFTOVER_WORKTREE_ROOT];
    assert_eq!(outcome.removed_for_age, 2);
    assert_eq!(outcome.kept_in_use, 1, "the recent one");
}

/// The detached worker may inherit a Git hook's routing variables: they are
/// dropped, so a foreign index never makes a tracked target look untracked.
#[test]
fn inherited_git_routing_never_reads_another_repository() {
    let dir = tempfile::tempdir().unwrap();
    let tracked = dir.path().join("tracked");
    fs::create_dir_all(&tracked).unwrap();
    git(&tracked, &["init", "-q"]);
    cargo_target(&tracked.join("target"));
    git(&tracked, &["add", "-f", "target/CACHEDIR.TAG"]);
    git(&tracked, &["commit", "-qm", "track a target"]);
    let foreign = dir.path().join("foreign");
    fs::create_dir_all(&foreign).unwrap();
    git(&foreign, &["init", "-q"]);
    write(&foreign.join("other.txt"), b"other");
    git(&foreign, &["add", "other.txt"]);
    git(&foreign, &["commit", "-qm", "other"]);
    let index = foreign.join(".git/index");
    let git_dir = foreign.join(".git");
    for inherited in [
        vec![("GIT_INDEX_FILE", index.to_str().unwrap())],
        vec![
            ("GIT_DIR", git_dir.to_str().unwrap()),
            ("GIT_WORK_TREE", foreign.to_str().unwrap()),
        ],
    ] {
        assert_eq!(
            untracked_and_ignored_inheriting(&tracked, "target", &inherited),
            Some(false),
            "{inherited:?}"
        );
    }
}

/// A build between the first listing and the locks is seen: last use is
/// read again under the locks, so the fresh target stays.
#[test]
fn a_build_just_before_the_locks_keeps_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pool");
    let container = scratch_container(&root);
    let target = container.join("s1-abc/target");
    cargo_target(&target);
    age_tree(&target, 2 * DAY);
    BEFORE_LOCK_FOR_TEST.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|target: &Path| {
            set_times(&target.join(".rustc_info.json"), SystemTime::now());
        }));
    });
    let report = run_on(&[worktree_root(&root, Vec::new())]);
    BEFORE_LOCK_FOR_TEST.with(|hook| *hook.borrow_mut() = None);
    assert!(target.exists(), "used just before the locks");
    let outcome = &report.classes[SESSION_SCRATCH_BUILD];
    assert_eq!((outcome.removed_for_age, outcome.kept_in_use), (0, 1));
}

/// Cargo creates `.cargo-lock` before anything else in a profile: such a
/// profile counts, and a build holding it keeps an old target.
#[test]
fn a_profile_holding_only_its_lock_counts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pool");
    let container = scratch_container(&root);
    let target = container.join("s1-abc/target");
    cargo_target(&target);
    write(&target.join("release/.cargo-lock"), b"");
    assert!(profiles(&target).contains(&target.join("release")));
    age_tree(&target, 2 * DAY);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target.join("release/.cargo-lock"))
        .unwrap();
    held.try_lock().unwrap();
    let report = run_on(&[worktree_root(&root, Vec::new())]);
    assert!(target.exists(), "a build holds the new profile");
    assert_eq!(report.classes[SESSION_SCRATCH_BUILD].kept_in_use, 1);
    drop(held);
}
