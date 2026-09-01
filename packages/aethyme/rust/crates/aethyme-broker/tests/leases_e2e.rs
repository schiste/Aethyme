//! End-to-end lease tests (issues #11, #12, #14): two real worktrees,
//! diff-derived leases, overlap warnings, ignore rules, and
//! new-overlap-only event emission.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    AdoptMode, AdoptOptions, Broker, BrokerOpError, LeaseKind, LeaseOverlapRelation,
};

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.py"), "a\n").unwrap();
    std::fs::write(root.join("src/other.py"), "b\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn managed_worktree_names(root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn planned_start_claims_before_any_diff_and_refuses_a_second_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let first = broker
        .start_worktree_with_planned_paths("first rewrite", &["generated/".into()])
        .unwrap();
    assert_eq!(first.planned_explicit_leases.len(), 1);
    assert_eq!(first.planned_explicit_leases[0].path, "generated/");
    assert_eq!(first.planned_explicit_leases[0].kind, LeaseKind::Explicit);
    let worktrees_before = managed_worktree_names(&first.worktree_placement.root);

    let error = broker
        .start_worktree_with_planned_paths("second rewrite", &["generated/policy.md".into()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("planned lease"), "{error}");
    assert!(error.contains("generated/"), "{error}");
    assert!(error.contains("(active)"), "{error}");
    assert!(error.contains(&first.session.worktree_path), "{error}");
    assert!(
        error.contains("aethyme broker adopt")
            && error.contains("--reuse --path generated/policy.md"),
        "{error}"
    );
    assert!(!error.contains("--reuse --session"), "{error}");
    assert_eq!(broker.store().live_sessions().unwrap().len(), 1);
    assert_eq!(
        managed_worktree_names(&first.worktree_placement.root),
        worktrees_before,
        "a refused plan must not leave a worktree behind"
    );
}

#[test]
fn planned_reuse_is_all_or_nothing_and_does_not_retask_on_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let owner_worktree = add_worktree(tmp.path(), "plan-owner");
    let reuse_worktree = add_worktree(tmp.path(), "plan-reuse");
    let owner = broker.adopt(&owner_worktree, Some("owner")).unwrap();
    let reused = broker
        .adopt(&reuse_worktree, Some("original task"))
        .unwrap();
    broker.claim_lease(owner.id, "src/", None).unwrap();

    let error = broker
        .adopt_with_options(
            &reuse_worktree,
            Some("must not persist"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: false,
                planned_paths: vec!["docs/new.md".into(), "src/auth.py".into()],
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("src/auth.py"), "{error}");
    assert_eq!(
        broker.store().session(reused.id).unwrap().task.as_deref(),
        Some("original task")
    );
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .all(|lease| { lease.session_id != reused.id || lease.kind != LeaseKind::Explicit })
    );
}

#[test]
fn planned_reuse_is_deduplicated_sorted_and_expired_conflicts_do_not_block() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let owner_worktree = add_worktree(tmp.path(), "expiring-owner");
    let reuse_worktree = add_worktree(tmp.path(), "successful-reuse");
    let owner = broker.adopt(&owner_worktree, Some("owner")).unwrap();
    let reused = broker.adopt(&reuse_worktree, Some("first task")).unwrap();
    broker.claim_lease(owner.id, "expired/", Some(1)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));

    let report = broker
        .adopt_with_options(
            &reuse_worktree,
            Some("planned follow-up"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: false,
                planned_paths: vec![
                    "zeta.txt".into(),
                    "expired/file.txt".into(),
                    "zeta.txt".into(),
                ],
            },
        )
        .unwrap();
    assert_eq!(report.session.id, reused.id);
    assert_eq!(report.session.task.as_deref(), Some("planned follow-up"));
    assert_eq!(
        report
            .planned_explicit_leases
            .iter()
            .map(|lease| lease.path.as_str())
            .collect::<Vec<_>>(),
        vec!["expired/file.txt", "zeta.txt"]
    );
}

#[test]
fn overlapping_edits_warn_once_and_lockfiles_never_warn() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    let session_a = broker.adopt(&wt_a, Some("task a")).unwrap();
    let session_b = broker.adopt(&wt_b, Some("task b")).unwrap();

    // Both touch only a lockfile → ignore rules keep this quiet (#14).
    std::fs::write(wt_a.join("pnpm-lock.yaml"), "x\n").unwrap();
    std::fs::write(wt_b.join("pnpm-lock.yaml"), "y\n").unwrap();
    assert!(broker.refresh_leases().unwrap().is_empty());

    // Disjoint edits → leases exist, no overlap.
    std::fs::write(wt_a.join("src/auth.py"), "edited by a\n").unwrap();
    std::fs::write(wt_b.join("src/other.py"), "edited by b\n").unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.is_empty());
    assert_eq!(broker.store().active_leases().unwrap().len(), 2);

    // B edits the same file as A → overlap detected, event emitted.
    std::fs::write(wt_b.join("src/auth.py"), "edited by b too\n").unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].session_a, session_a.id.min(session_b.id));
    assert_eq!(overlaps[0].path, "src/auth.py");

    // Refresh again: overlap still reported, but the event fired ONCE.
    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let overlap_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "lease.overlap")
        .collect();
    assert_eq!(
        overlap_events.len(),
        1,
        "known overlaps are not re-announced"
    );
    assert!(
        overlap_events[0]
            .payload_json
            .as_deref()
            .unwrap()
            .contains("src/auth.py")
    );

    // A reverts its edit → overlap clears on the next refresh.
    std::fs::write(wt_a.join("src/auth.py"), "a\n").unwrap();
    sh(&wt_a, &["checkout", "--", "src/auth.py"]);
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.is_empty());
}

#[test]
fn explicit_directory_claim_overlaps_other_sessions_files() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    let session_a = broker.adopt(&wt_a, None).unwrap();
    let _session_b = broker.adopt(&wt_b, None).unwrap();

    // A claims the whole src/ directory explicitly; B edits a file in it.
    broker
        .store()
        .claim_lease(session_a.id, "src/", None)
        .unwrap();
    std::fs::write(wt_b.join("src/other.py"), "edit\n").unwrap();

    let overlaps = broker.refresh_leases().unwrap();
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].path, "src/other.py");

    // Config-driven extra ignore rule silences a path (#14).
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[leases]\nignore = [\"src/\"]\n",
    )
    .unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    assert!(
        overlaps.is_empty(),
        "config-ignored prefix keeps implicit leases (and thus overlaps) out"
    );
}

#[test]
fn explicit_claim_blocks_paths_owned_by_another_live_session() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "a");
    let wt_b = add_worktree(tmp.path(), "b");
    let session_a = broker.adopt(&wt_a, None).unwrap();
    let _session_b = broker.adopt(&wt_b, None).unwrap();

    std::fs::write(wt_b.join("src/auth.py"), "owned elsewhere\n").unwrap();
    let err = broker.claim_lease(session_a.id, "src/", None).unwrap_err();

    assert!(matches!(
        err,
        BrokerOpError::LeaseClaimConflict {
            blocker_count: 1,
            ..
        }
    ));
    assert!(err.to_string().contains("overlaps 1 active lease"));
}

#[test]
fn lease_plan_reports_all_overlap_shapes_and_never_mutates_state() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "planner");
    let wt_b = add_worktree(tmp.path(), "owner");
    let session_a = broker.adopt(&wt_a, None).unwrap();
    let session_b = broker.adopt(&wt_b, None).unwrap();
    broker
        .store()
        .claim_lease(session_a.id, "src/auth.py", None)
        .unwrap();
    broker
        .store()
        .set_implicit_leases(session_b.id, &["src/auth.py".into()])
        .unwrap();
    broker
        .store()
        .claim_lease(session_b.id, "src/", Some(60_000))
        .unwrap();
    broker
        .store()
        .claim_lease(session_b.id, "docs/api/", None)
        .unwrap();
    broker
        .store()
        .claim_lease(session_b.id, "expired.py", Some(-1))
        .unwrap();

    let leases_before = serde_json::to_value(broker.store().active_leases().unwrap()).unwrap();
    let events_before =
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap();
    let report = broker
        .plan_leases(
            &[
                "src/new.py".into(),
                "src/auth.py".into(),
                "src/".into(),
                "docs/".into(),
                "clear.txt".into(),
                "expired.py".into(),
            ],
            Some(session_a.id),
        )
        .unwrap();

    assert_eq!(
        report
            .paths
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "clear.txt",
            "docs/",
            "expired.py",
            "src/",
            "src/auth.py",
            "src/new.py"
        ]
    );
    assert!(report.would_conflict);
    let auth = report
        .paths
        .iter()
        .find(|path| path.path == "src/auth.py")
        .unwrap();
    assert_eq!(auth.owned.len(), 1);
    assert_eq!(auth.owned[0].session_id, session_a.id);
    assert_eq!(auth.owned[0].relation, LeaseOverlapRelation::Exact);
    assert_eq!(auth.conflicts.len(), 2);
    assert!(
        auth.conflicts
            .iter()
            .any(|lease| lease.relation == LeaseOverlapRelation::Exact)
    );
    assert!(
        auth.conflicts
            .iter()
            .any(|lease| lease.relation == LeaseOverlapRelation::Directory)
    );

    let directory = report
        .paths
        .iter()
        .find(|path| path.path == "src/")
        .unwrap();
    assert!(
        directory
            .owned
            .iter()
            .any(|lease| lease.path == "src/auth.py")
    );
    assert!(
        directory
            .conflicts
            .iter()
            .any(|lease| lease.path == "src/auth.py")
    );
    let nested_directories = report
        .paths
        .iter()
        .find(|path| path.path == "docs/")
        .unwrap();
    assert_eq!(nested_directories.conflicts.len(), 1);
    assert_eq!(
        nested_directories.conflicts[0].relation,
        LeaseOverlapRelation::Directory
    );
    for clear in ["clear.txt", "expired.py"] {
        let path = report.paths.iter().find(|path| path.path == clear).unwrap();
        assert!(!path.would_conflict);
        assert!(path.owned.is_empty());
        assert!(path.conflicts.is_empty());
    }

    let anonymous = broker.plan_leases(&["src/auth.py".into()], None).unwrap();
    assert!(anonymous.paths[0].owned.is_empty());
    assert_eq!(anonymous.paths[0].conflicts.len(), 3);
    assert!(anonymous.paths[0].would_conflict);

    assert_eq!(
        serde_json::to_value(broker.store().active_leases().unwrap()).unwrap(),
        leases_before,
        "planning must not claim or refresh leases"
    );
    assert_eq!(
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap(),
        events_before,
        "planning must not append events"
    );
}

#[test]
fn lease_plan_and_claim_share_strict_repo_relative_path_validation() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), None).unwrap();

    for path in [
        "",
        "/absolute",
        "../outside",
        "src/../file",
        "./src/file",
        "src//file",
    ] {
        let plan_error = broker.plan_leases(&[path.into()], None).unwrap_err();
        let claim_error = broker.claim_lease(session.id, path, None).unwrap_err();
        assert!(
            matches!(plan_error, BrokerOpError::InvalidLeasePath { .. }),
            "plan accepted {path:?}: {plan_error}"
        );
        assert_eq!(plan_error.to_string(), claim_error.to_string());
    }
}

#[test]
fn guarded_exec_requires_explicit_lease_for_new_dirty_paths() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt = add_worktree(tmp.path(), "guarded");
    let session = broker.adopt(&wt, None).unwrap();
    broker.claim_lease(session.id, "src/auth.py", None).unwrap();

    let allowed = broker
        .guarded_exec(
            session.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf owned > src/auth.py".into(),
            ],
        )
        .unwrap();
    assert!(allowed.ok, "{allowed:?}");
    assert_eq!(allowed.touched_paths, vec!["src/auth.py"]);
    assert_eq!(allowed.newly_dirty_paths, vec!["src/auth.py"]);
    assert!(allowed.modified_preexisting_dirty_paths.is_empty());

    let modified = broker
        .guarded_exec(
            session.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf revised > src/auth.py".into(),
            ],
        )
        .unwrap();
    assert!(modified.ok, "{modified:?}");
    assert_eq!(modified.touched_paths, vec!["src/auth.py"]);
    assert!(modified.newly_dirty_paths.is_empty());
    assert_eq!(
        modified.modified_preexisting_dirty_paths,
        vec!["src/auth.py"]
    );
    let json = serde_json::to_value(&modified).unwrap();
    assert_eq!(json["newly_dirty_paths"], serde_json::json!([]));
    assert_eq!(
        json["modified_preexisting_dirty_paths"],
        serde_json::json!(["src/auth.py"])
    );

    broker
        .store()
        .release_lease(session.id, "src/auth.py")
        .unwrap();
    let modified_without_lease = broker
        .guarded_exec(
            session.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf unowned > src/auth.py".into(),
            ],
        )
        .unwrap();
    assert!(!modified_without_lease.ok, "{modified_without_lease:?}");
    assert_eq!(
        modified_without_lease.modified_preexisting_dirty_paths,
        vec!["src/auth.py"]
    );
    assert_eq!(
        modified_without_lease.outside_lease_paths,
        vec!["src/auth.py"]
    );

    let blocked = broker
        .guarded_exec(
            session.id,
            &[
                "sh".into(),
                "-c".into(),
                "printf outside > src/other.py".into(),
            ],
        )
        .unwrap();
    assert!(!blocked.ok, "{blocked:?}");
    assert_eq!(blocked.outside_lease_paths, vec!["src/other.py"]);
}

#[test]
fn submit_blocks_paths_explicitly_owned_by_another_session() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt_a = add_worktree(tmp.path(), "owner");
    let wt_b = add_worktree(tmp.path(), "intruder");
    let session_a = broker.adopt(&wt_a, None).unwrap();
    let session_b = broker.adopt(&wt_b, None).unwrap();
    broker
        .claim_lease(session_a.id, "src/auth.py", None)
        .unwrap();

    std::fs::write(wt_b.join("src/auth.py"), "intruder\n").unwrap();
    sh(&wt_b, &["add", "-A"]);
    sh(&wt_b, &["commit", "-qm", "intruder"]);

    let err = broker.submit(session_b.id).unwrap_err();
    let BrokerOpError::OwnershipViolation { report, .. } = err else {
        panic!("expected ownership violation");
    };
    assert_eq!(report.conflicting_leases.len(), 1);
    assert_eq!(report.conflicting_leases[0].session_id, session_a.id);
    assert_eq!(report.conflicting_leases[0].path, "src/auth.py");
}

#[test]
fn adoption_time_foreign_file_blocks_submit_until_explicitly_claimed() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let wt = add_worktree(tmp.path(), "foreign");
    std::fs::write(wt.join("foreign.txt"), "preexisting\n").unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&wt, None).unwrap();
    assert_eq!(
        broker.store().session_foreign_files(session.id).unwrap(),
        vec!["foreign.txt"]
    );

    sh(&wt, &["add", "foreign.txt"]);
    sh(&wt, &["commit", "-qm", "commit foreign"]);
    let err = broker.submit(session.id).unwrap_err();
    let BrokerOpError::OwnershipViolation { report, .. } = err else {
        panic!("expected ownership violation");
    };
    assert_eq!(report.foreign_paths, vec!["foreign.txt"]);

    broker.claim_lease(session.id, "foreign.txt", None).unwrap();
    assert!(broker.submit(session.id).unwrap().promoted);
}

// ── #41: leases derive from merge-base, self-healing after rebases ─────

#[test]
fn rebase_onto_integration_does_not_inflate_leases() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Session 1 promotes work on file_one — integration now contains it.
    let wt1 = add_worktree(tmp.path(), "one");
    let s1 = broker.adopt(&wt1, Some("one")).unwrap();
    std::fs::write(wt1.join("file_one.txt"), "one\n").unwrap();
    sh(&wt1, &["add", "-A"]);
    sh(&wt1, &["commit", "-qm", "one"]);
    assert!(broker.submit(s1.id).unwrap().promoted);

    // Session 2 rebases onto the integration branch (as the
    // action-required flow instructs), bringing session 1's promoted
    // commit into its history, then does its own work on file_two.
    let wt2 = add_worktree(tmp.path(), "two");
    let s2 = broker.adopt(&wt2, Some("two")).unwrap();
    sh(&wt2, &["fetch", ".", "aethyme/integration"]);
    sh(&wt2, &["rebase", "FETCH_HEAD"]);
    std::fs::write(wt2.join("file_two.txt"), "two\n").unwrap();
    sh(&wt2, &["add", "-A"]);
    sh(&wt2, &["commit", "-qm", "two"]);

    broker.refresh_leases().unwrap();
    let leases = broker.store().active_leases().unwrap();
    let s2_paths: Vec<&str> = leases
        .iter()
        .filter(|l| l.session_id == s2.id)
        .map(|l| l.path.as_str())
        .collect();
    assert!(
        s2_paths.contains(&"file_two.txt"),
        "own work must be leased: {s2_paths:?}"
    );
    assert!(
        !s2_paths.contains(&"file_one.txt"),
        "promoted work brought in by the rebase must NOT appear as a \
         phantom lease (#41): {s2_paths:?}"
    );
}

#[test]
fn equivalent_tree_publication_starts_from_integration_without_phantom_leases() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let integration_builder = add_worktree(tmp.path(), "equivalent-integration");
    std::fs::create_dir_all(integration_builder.join("gadget")).unwrap();
    std::fs::write(integration_builder.join("gadget/wait.js"), "ready\n").unwrap();
    sh(&integration_builder, &["add", "gadget/wait.js"]);
    sh(
        &integration_builder,
        &["commit", "-qm", "integration identity"],
    );
    sh(
        tmp.path(),
        &[
            "branch",
            "-f",
            "aethyme/integration",
            "agent/equivalent-integration",
        ],
    );

    std::fs::create_dir_all(tmp.path().join("gadget")).unwrap();
    std::fs::write(tmp.path().join("gadget/wait.js"), "ready\n").unwrap();
    sh(tmp.path(), &["add", "gadget/wait.js"]);
    sh(tmp.path(), &["commit", "-qm", "published identity"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let repository = aethyme_broker::GitRepo::discover(tmp.path()).unwrap();
    let main = repository.resolve_ref("main").unwrap();
    let integration = repository.resolve_ref("aethyme/integration").unwrap();
    assert_ne!(main, integration);
    assert_eq!(
        repository.commit_tree_id(&main).unwrap(),
        repository.commit_tree_id(&integration).unwrap()
    );

    let started = broker.start_worktree("disjoint proxy change").unwrap();
    assert_eq!(started.diff_base, Some(integration.clone()));
    let started_checkout =
        aethyme_broker::GitRepo::discover(Path::new(&started.worktree_path)).unwrap();
    assert_eq!(started_checkout.head_commit().unwrap(), integration);
    let worktree = Path::new(&started.worktree_path);
    std::fs::create_dir_all(worktree.join("proxy")).unwrap();
    std::fs::write(worktree.join("proxy/app.py"), "proxy = True\n").unwrap();
    sh(worktree, &["add", "proxy/app.py"]);
    sh(worktree, &["commit", "-qm", "proxy change"]);

    broker.refresh_leases().unwrap();
    let session_paths = broker
        .store()
        .active_leases()
        .unwrap()
        .into_iter()
        .filter(|lease| lease.session_id == started.id)
        .map(|lease| lease.path)
        .collect::<Vec<_>>();
    assert_eq!(session_paths, vec!["proxy/app.py"]);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(broker.status(now_ms).unwrap().promoted_conflicts.is_empty());
}
