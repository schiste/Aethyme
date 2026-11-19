//! Compact status keeps repeated warnings visible without repeating each
//! session's evidence, while the established JSON contracts remain intact.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::OverlapSeverity;

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn clear_git_selectors(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    clear_git_selectors(&mut command);
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "compact-status-test")
        .env("GIT_AUTHOR_EMAIL", "compact-status-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "compact-status-test")
        .env("GIT_COMMITTER_EMAIL", "compact-status-test@example.invalid");
    let output = command.output().expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join("src/shared.txt"), "base\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn add_worktree(repo: &Path, worktree_root: &Path, name: &str) -> PathBuf {
    let path = worktree_root.join(name);
    assert!(
        !path.starts_with(repo),
        "fixture worktrees stay outside the repository"
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn run_status(repo: &Path, args: &[&str]) -> Output {
    let checkout = repo
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(repo);
    let mut command = common::broker_cli_for_fixture(CLI, args, checkout);
    clear_git_selectors(&mut command);
    command
        .current_dir(repo)
        .output()
        .expect("run broker status")
}

fn json_status(repo: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_status(repo, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

#[test]
fn compact_status_groups_repeated_warnings_and_marks_omitted_details() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();

    let worktree_root = fixture.worktree_root();
    for name in ["agent-a", "agent-b", "agent-c"] {
        let worktree = add_worktree(fixture.path(), &worktree_root, name);
        broker.adopt(&worktree, Some(name)).unwrap();
        std::fs::write(worktree.join("src/shared.txt"), format!("{name}\n")).unwrap();
    }
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.len() >= 3, "expected all three pairs to overlap");
    let high = broker
        .overlap_pairs_snapshot(&overlaps)
        .into_iter()
        .filter(|pair| pair.severity == OverlapSeverity::High)
        .count();
    assert_eq!(high, 3, "each pair has a same-line conflict");
    drop(broker);

    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["schema"], "aethyme.broker-status-compact/v1");
    assert_eq!(compact["data_freshness"]["leases_refreshed"], false);
    assert_eq!(
        compact["summary"]["dirty_sessions"],
        serde_json::Value::Null
    );
    assert_eq!(
        compact["summary"]["promoted_conflict_count"],
        serde_json::Value::Null
    );
    assert_eq!(compact["summary"]["integration_relation"], "not_checked");
    assert_eq!(
        compact["summary"]["integration_ahead_main_commits"],
        serde_json::Value::Null
    );
    let unknown_summary_fields = compact["summary"]["unknown_fields"]
        .as_array()
        .expect("unknown summary fields are listed");
    for field in [
        "dirty_sessions",
        "promoted_conflict_count",
        "integration_ahead_main_commits",
        "integration_head",
        "baseline_ref",
        "baseline_head",
        "main_head",
        "integration_ahead_local_main_commits",
    ] {
        assert!(
            unknown_summary_fields.iter().any(|listed| listed == field),
            "null summary field {field} is declared unknown"
        );
    }
    assert_eq!(
        compact["data_freshness"]["deferred_checks_are_unknown"],
        true
    );
    assert!(
        compact["data_freshness"]["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check == "git_refs")
    );

    let categories = compact["advice"]["categories"].as_array().unwrap();
    let overlap = categories
        .iter()
        .find(|category| category["id"] == "lease.overlap-conflict")
        .expect("overlap warning category retained");
    assert_eq!(overlap["severity"], "warning");
    assert_eq!(overlap["count"], 3);
    assert_eq!(overlap["duplicate_count"], 2);
    assert_eq!(overlap["summary_variants_omitted"], true);
    assert_eq!(overlap["details_omitted"], true);
    assert_eq!(overlap["commands"], serde_json::json!([]));
    assert!(overlap["commands_omitted_count"].as_u64().unwrap() > 0);
    assert!(
        compact["omitted_details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "advice.evidence")
    );
    assert_eq!(compact["waiters"]["details_omitted"], false);
    assert_eq!(compact["waiters"]["count"], 0);
    assert_eq!(compact["waiters"]["omitted_count"], 0);
    assert_eq!(compact["waiters"]["records"], serde_json::json!([]));
    assert_eq!(compact["coordinated_operations"]["count"], 0);
    assert_eq!(compact["lease_release_requests"]["count"], 0);

    let text = run_status(fixture.path(), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Broker status (compact)"));
    assert!(text.contains("recorded lease-overlap observations"));
    assert!(text.contains("Repository information:"));
    assert!(!text.contains("aethyme broker advanced leases --json"));
    assert!(text.contains("Deferred checks (unknown, not clean)"));
    assert!(text.contains("aethyme broker status --full --json"));
    assert!(text.contains("Dirty sessions and promoted conflicts: not checked."));
    assert!(!text.contains("0 dirty"));
    if let Some(command) = compact["summary"]["commands"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
    {
        assert!(text.contains("Next:"));
        assert!(text.contains(command));
    }
}

#[test]
fn compact_json_is_additive_and_legacy_json_shapes_are_unchanged() {
    let fixture = tempfile::tempdir().unwrap();
    init_repo(fixture.path());

    let full = json_status(fixture.path(), &["status", "--full", "--json"]);
    assert_eq!(full["form"], "full");
    for key in [
        "agents",
        "blockers",
        "cleanup_retention",
        "deferred_checks",
        "leases_refreshed",
        "outstanding_advisories",
        "summary",
    ] {
        assert!(full.get(key).is_some(), "legacy full JSON still has {key}");
    }

    let summary = json_status(fixture.path(), &["status", "--summary", "--json"]);
    for key in [
        "advice",
        "deferred_checks",
        "leases_refreshed",
        "phase_timings_ms",
        "summary",
    ] {
        assert!(
            summary.get(key).is_some(),
            "legacy summary JSON still has {key}"
        );
    }
    assert!(summary.get("schema").is_none());

    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["schema"], "aethyme.broker-status-compact/v1");
    assert_eq!(compact["form"], "compact");
    assert!(compact["hint"].as_str().unwrap().contains("--full"));
    assert!(compact.get("agents").is_none());
    assert!(compact.get("blockers").is_none());
    assert!(compact.get("session").is_none());

    // Compact is the default JSON form; `--compact` changes nothing.
    let default = json_status(fixture.path(), &["status", "--json"]);
    assert_eq!(default["schema"], "aethyme.broker-status-compact/v1");
    assert_eq!(default["form"], "compact");
    assert!(default.get("agents").is_none());
    let keys = |value: &serde_json::Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&default), keys(&compact));

    for args in [
        ["status", "--compact", "--refresh"].as_slice(),
        ["status", "--compact", "--summary"].as_slice(),
    ] {
        let output = run_status(fixture.path(), args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--compact"));
    }
    for args in [
        ["status", "--full", "--compact", "--json"].as_slice(),
        ["status", "--full", "--summary", "--json"].as_slice(),
        ["status", "--full", "--session", "1", "--json"].as_slice(),
    ] {
        let output = run_status(fixture.path(), args);
        assert!(!output.status.success(), "{args:?}");
        // A `--json` refusal is a JSON error document on stdout.
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(message.contains("--full"), "{args:?}: {message}");
    }
}

#[test]
fn unbound_compact_status_summarizes_waiters_without_resources() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let broker = common::open_fixture_repository_broker(&fixture).unwrap();
    drop(broker);
    let waits = fixture.path().join(".aethyme/run/waits");
    std::fs::create_dir_all(&waits).unwrap();
    for i in 0..12 {
        let record = aethyme_broker::WaitRecord {
            pid: i64::from(std::process::id()),
            session_id: Some(100 + i),
            kind: "lease".into(),
            resource: format!("resource-{i}"),
            holder: format!("session {}", 200 + i),
            started_at_ms: i,
        };
        std::fs::write(
            waits.join(format!("fixture-{i}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["waiters"]["count"], 12);
    assert_eq!(compact["waiters"]["omitted_count"], 12);
    assert_eq!(compact["waiters"]["details_omitted"], true);
    assert_eq!(compact["waiters"]["records"], serde_json::json!([]));
    let text = run_status(fixture.path(), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("12 waiter(s)."), "{text}");
    assert!(!text.contains("resource-0"), "{text}");
}

#[test]
fn summary_defers_gate_history_queries_to_full_inspection() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let broker = common::open_fixture_repository_broker(&fixture).unwrap();
    // Make gate history unavailable independently of session/coordination
    // records. A full recovery inspection must report that fault; summary
    // explicitly defers this source and must not query it at all.
    rusqlite::Connection::open(aethyme_broker::broker_db_path(fixture.path()).unwrap())
        .unwrap()
        .execute_batch("ALTER TABLE gate_results RENAME TO unavailable_gate_history")
        .unwrap();
    let blockers = broker.blockers();
    assert!(
        blockers
            .unavailable
            .iter()
            .any(|source| source.source == "broker.db gate_results"),
        "full inspection must report unavailable gate history: {blockers:?}"
    );
    // Inspect through the already-open broker: a new CLI process correctly
    // refuses a database missing a required table during schema validation,
    // before it reaches the snapshot collector this regression exercises.
    let summary = serde_json::to_value(broker.status_brief_snapshot(0).unwrap()).unwrap();
    let advice = summary["advice"].as_array().unwrap();
    assert!(
        !advice.iter().any(|item| {
            item["id"] == "blockers.sources-unavailable"
                && item["evidence"].as_array().is_some_and(|evidence| {
                    evidence.iter().any(|entry| {
                        entry
                            .as_str()
                            .is_some_and(|text| text.starts_with("broker.db gate_results:"))
                    })
                })
        }),
        "routine status must not query deferred gate history: {summary}"
    );
}

#[test]
fn default_status_identifies_the_worktree_session_and_reports_only_its_conflicts() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let root = fixture.worktree_root();
    let first = add_worktree(fixture.path(), &root, "focused-first");
    let second = add_worktree(fixture.path(), &root, "focused-second");
    let session = broker.adopt(&first, Some("focused-first")).unwrap();
    broker.adopt(&second, Some("focused-second")).unwrap();
    std::fs::write(first.join("src/shared.txt"), "first\n").unwrap();
    std::fs::write(second.join("src/shared.txt"), "second\n").unwrap();
    broker.refresh_leases().unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let global = json_status(fixture.path(), &["status", "--compact", "--json"]);
    let focused = json_status(
        fixture.path(),
        &["status", "--compact", "--session", &session_id, "--json"],
    );
    assert_eq!(focused["schema"], "aethyme.broker-session-view/v2");
    assert_eq!(focused["peer_state"]["lease_count"], 1);
    assert!(focused["global_state"].get("records").is_none());
    assert!(focused.get("session_contexts").is_none());
    assert_eq!(global["summary"]["live_sessions"], 2);
    let text = run_status(&first.join("src"), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("src/shared.txt"), "{text}");
    assert!(text.contains("Next:"), "{text}");
    assert_eq!(text.matches("Next:").count(), 1, "{text}");
    let unknown = run_status(
        fixture.path(),
        &["status", "--compact", "--json", "--session", "999999999"],
    );
    assert!(!unknown.status.success());
    let implicit = json_status(
        fixture.path(),
        &["status", "--json", "--session", &session_id],
    );
    let bound = json_status(&first.join("src"), &["status", "--json"]);
    assert_eq!(bound, focused);
    assert_eq!(implicit, focused);
    // The session block: local facts only, the PR unknown when unrecorded.
    let block = &implicit["session"];
    assert_eq!(block["id"], session.id, "{block}");
    assert_eq!(block["branch"], "focused-first");
    assert_eq!(block["status"], "active");
    assert!(
        block["worktree"]
            .as_str()
            .unwrap()
            .ends_with("focused-first")
    );
    let head = block["head_oid"].as_str().expect("head oid");
    assert_eq!(head.len(), 40, "{block}");
    assert_eq!(block["remote_oid"], serde_json::Value::Null);
    assert!(block["unpushed_commits"].as_u64().unwrap() >= 1, "{block}");
    assert_eq!(block["pr"], serde_json::Value::Null);
    assert_eq!(block["pr_state"], "unknown");
    assert!(block["preparation"]["state"].is_string(), "{block}");
    assert!(block["advice"].is_array() && block["blockers"].is_array());
    let next = block["next_action"]["command"].as_str().unwrap();
    assert!(
        !next.contains("broker push") && next.contains("status --short"),
        "{next}"
    );
    assert!(block["next_action"]["reason"].is_string());
    assert!(global.get("session").is_none());
    let text = run_status(fixture.path(), &["status", "--session", &session_id]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!("Aethyme session {session_id} (active)")),
        "{text}"
    );
    assert!(text.contains("Next:"), "{text}");
}

#[test]
fn compact_revision_returns_safe_deltas_and_resets_on_scope_change() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let initial = json_status(fixture.path(), &["status", "--compact", "--json"]);
    let revision = initial["revision"].as_str().unwrap();
    let next = json_status(
        fixture.path(),
        &["status", "--since-revision", revision, "--json"],
    );
    assert_eq!(next["schema"], "aethyme.broker-status-delta/v1");
    assert_eq!(next["data_freshness"]["deferred_checks_are_unknown"], true);
    let reset = json_status(
        fixture.path(),
        &["status", "--since-revision", "invalid", "--json"],
    );
    assert_eq!(reset["revision_reset"], true);
    assert_eq!(reset["schema"], "aethyme.broker-status-compact/v1");
}

#[test]
fn focused_revision_only_returns_session_when_its_facts_change() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let worktree = add_worktree(fixture.path(), &fixture.worktree_root(), "revision-session");
    let session = broker.adopt(&worktree, Some("revision-session")).unwrap();
    drop(broker);
    let id = session.id.to_string();
    let first = json_status(fixture.path(), &["status", "--session", &id, "--json"]);
    let revision = first["revision"].as_str().unwrap();
    let unchanged = json_status(
        fixture.path(),
        &[
            "status",
            "--session",
            &id,
            "--since-revision",
            revision,
            "--json",
        ],
    );
    assert!(unchanged.get("session").is_none(), "{unchanged}");
    assert!(
        unchanged["changed_sections"].get("session").is_none(),
        "{unchanged}"
    );
    std::fs::write(worktree.join("new.txt"), "new content").unwrap();
    git(&worktree, &["add", "new.txt"]);
    git(&worktree, &["commit", "-qm", "new head"]);
    let changed = json_status(
        fixture.path(),
        &[
            "status",
            "--session",
            &id,
            "--since-revision",
            revision,
            "--json",
        ],
    );
    assert_ne!(
        changed["changed_sections"]["session"]["head_oid"], first["session"]["head_oid"],
        "{changed}"
    );
}

#[test]
fn session_view_ignores_unrelated_sessions_and_resets_other_revision_scopes() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let worktree = add_worktree(fixture.path(), &fixture.worktree_root(), "view-first");
    let session = broker.adopt(&worktree, Some("view-first")).unwrap();
    drop(broker);
    let id = session.id.to_string();
    let first = json_status(
        fixture.path(),
        &["status", "--session", &id, "--session-view", "--json"],
    );
    assert_eq!(first["schema"], "aethyme.broker-session-view/v2");
    assert_eq!(
        first["data_freshness"]["global_warnings"],
        "recorded_partial"
    );
    assert_eq!(first["peer_state"]["pre_edit"]["action"], "continue");
    assert!(first["peer_state"]["pre_edit"]["peer"].is_null());
    let revision = first["revision"].as_str().unwrap();
    let other = add_worktree(fixture.path(), &fixture.worktree_root(), "view-other");
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let peer = broker.adopt(&other, Some("unrelated")).unwrap();
    drop(broker);
    let same = json_status(
        fixture.path(),
        &[
            "status",
            "--session",
            &id,
            "--session-view",
            "--since-revision",
            revision,
            "--json",
        ],
    );
    assert!(
        same["changed_sections"].as_object().unwrap().is_empty(),
        "{same}"
    );
    let unbound = json_status(
        fixture.path(),
        &["status", "--since-revision", revision, "--json"],
    );
    assert_eq!(unbound["revision_reset"], true);
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    broker
        .claim_lease(session.id, "src/shared.txt", None)
        .unwrap();
    broker.claim_lease(peer.id, "src/shared.txt", None).unwrap();
    drop(broker);
    let peer_change = json_status(
        fixture.path(),
        &[
            "status",
            "--session",
            &id,
            "--session-view",
            "--since-revision",
            revision,
            "--json",
        ],
    );
    assert_eq!(
        peer_change["changed_sections"]["peer_state"]["lease_count"],
        1
    );
    let decision = &peer_change["changed_sections"]["peer_state"]["pre_edit"];
    assert_eq!(decision["action"], "continue");
    assert_eq!(decision["peer"]["session_id"], peer.id);
    assert_eq!(decision["peer"]["task"], "unrelated");

    let waiting = json_status(&other, &["status", "--json"]);
    let decision = &waiting["peer_state"]["pre_edit"];
    assert_eq!(decision["action"], "wait");
    assert_eq!(decision["peer"]["session_id"], session.id);
    assert!(decision["command"].is_null());

    std::fs::write(worktree.join("new-view.txt"), "changed").unwrap();
    git(&worktree, &["add", "new-view.txt"]);
    git(&worktree, &["commit", "-qm", "change session"]);
    let changed = json_status(
        fixture.path(),
        &[
            "status",
            "--session",
            &id,
            "--session-view",
            "--since-revision",
            revision,
            "--json",
        ],
    );
    assert_ne!(
        changed["changed_sections"]["session"]["head_oid"], first["session"]["head_oid"],
        "{changed}"
    );
    assert!(
        !run_status(fixture.path(), &["status", "--session-view", "--json"])
            .status
            .success()
    );
    assert!(
        !run_status(
            fixture.path(),
            &["status", "--session", &id, "--session-view"]
        )
        .status
        .success()
    );
}

#[test]
fn session_event_follower_drains_backlog_without_another_write() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let first = add_worktree(fixture.path(), &fixture.worktree_root(), "stream-first");
    let second = add_worktree(fixture.path(), &fixture.worktree_root(), "stream-second");
    let session = broker.adopt(&first, Some("first")).unwrap();
    let other = broker.adopt(&second, Some("second")).unwrap();
    broker
        .store()
        .append_event("test.stream", Some(other.id), None)
        .unwrap();
    for _ in 0..1001 {
        broker
            .store()
            .append_event("test.stream", Some(session.id), None)
            .unwrap();
    }
    drop(broker);

    let id = session.id.to_string();
    let mut child = common::broker_cli_for_fixture(
        CLI,
        &[
            "advanced",
            "events",
            "--session",
            &id,
            "--kind",
            "test.stream",
            "--follow",
            "--json",
        ],
        fixture.path(),
    )
    .current_dir(fixture.path())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut ids = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let value: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
            ids.push(value["session_id"].as_i64().unwrap());
            if ids.len() == 1001 {
                let _ = send.send(ids);
                break;
            }
        }
    });
    let result = recv.recv_timeout(Duration::from_secs(15));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    let ids = result.unwrap_or_else(|error| {
        panic!(
            "follower did not drain backlog: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(ids.len(), 1001);
    assert!(ids.iter().all(|id| *id == session.id));
}

#[test]
fn focused_status_reports_only_live_waits_owned_by_the_selected_session() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let root = fixture.worktree_root();
    let first = add_worktree(fixture.path(), &root, "waiting-first");
    let other = add_worktree(fixture.path(), &root, "waiting-other");
    let session = broker.adopt(&first, None).unwrap();
    let peer = broker.adopt(&other, None).unwrap();
    broker
        .store()
        .set_session_status(peer.id, aethyme_broker::SessionStatus::Stale, None)
        .unwrap();
    drop(broker);
    let waits = fixture.path().join(".aethyme/run/waits");
    std::fs::create_dir_all(&waits).unwrap();
    for (id, pid, resource) in [
        (session.id, std::process::id(), "my-owned-ref"),
        (peer.id, std::process::id(), "unrelated-ref"),
        (session.id, i32::MAX as u32, "dead-waiter"),
    ] {
        let row = aethyme_broker::WaitRecord {
            pid: i64::from(pid),
            session_id: Some(id),
            kind: "coordinated_write_lock".into(),
            resource: resource.into(),
            holder: "fixture holder".into(),
            started_at_ms: 1,
        };
        std::fs::write(
            waits.join(format!("{resource}.json")),
            serde_json::to_vec(&row).unwrap(),
        )
        .unwrap();
    }
    let view = json_status(&first, &["status", "--json"]);
    assert_eq!(view["global_state"]["recorded_stale_sessions"], 1);
    assert_eq!(view["session"]["waiting"]["count"], 1, "{view}");
    assert_eq!(
        view["session"]["waiting"]["records"][0]["resource"],
        "my-owned-ref"
    );
    assert!(!view.to_string().contains("unrelated-ref"));
    assert!(!view.to_string().contains("dead-waiter"));
    assert!(
        view["session"]["next_action"]["reason"]
            .as_str()
            .unwrap()
            .contains("running wait")
    );
}

#[test]
fn session_status_keeps_recovery_details_with_the_session_that_can_act() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let root = fixture.worktree_root();
    let first = add_worktree(fixture.path(), &root, "recovery-first");
    let other = add_worktree(fixture.path(), &root, "recovery-other");
    let session = broker.adopt(&first, Some("focused")).unwrap();
    let peer = broker.adopt(&other, Some("unrelated")).unwrap();
    std::fs::create_dir_all(other.join(".aethyme")).unwrap();
    std::fs::write(
        other.join(".aethyme/broker-action-required.md"),
        "private-conflict.txt",
    )
    .unwrap();
    drop(broker);

    let unbound = json_status(fixture.path(), &["status", "--json"]);
    assert!(unbound.get("session").is_none());
    let status = json_status(&first, &["status", "--json"]);
    assert_eq!(status["session"]["id"], session.id);
    assert_eq!(status["session"]["blockers"], serde_json::json!([]));
    assert!(!status.to_string().contains("recovery-other"));

    let owner = json_status(&other, &["status", "--json"]);
    assert_eq!(
        owner["session"]["blockers"][0]["id"],
        format!("action:{}", peer.id)
    );
    let next = owner["session"]["next_action"]["command"].as_str().unwrap();
    assert!(
        next.starts_with("cat ") && next.ends_with("broker-action-required.md"),
        "{next}"
    );
    assert!(!owner.to_string().contains("private-conflict.txt"));
}

#[test]
fn session_view_does_not_load_unrelated_agent_records() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    let first = add_worktree(fixture.path(), &fixture.worktree_root(), "focused-record");
    let other = add_worktree(fixture.path(), &fixture.worktree_root(), "unrelated-record");
    let session = broker.adopt(&first, Some("focused")).unwrap();
    let peer = broker.adopt(&other, Some("unrelated")).unwrap();
    drop(broker);
    let db = rusqlite::Connection::open(aethyme_broker::broker_db_path(fixture.path()).unwrap())
        .unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    db.execute(
        "UPDATE sessions SET origin = 'unrecognized-origin' WHERE id = ?1",
        [peer.id],
    )
    .unwrap();
    drop(db);
    let id = session.id.to_string();
    let view = json_status(
        fixture.path(),
        &["status", "--session", &id, "--session-view", "--json"],
    );
    assert_eq!(view["session"]["id"], session.id);
    assert_eq!(view["data_freshness"]["session_advice"], "partial");
    assert_eq!(view["data_freshness"]["peer_state"], "recorded_only");
}
