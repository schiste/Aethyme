use super::{UsageError, owned_comment_from_view, repository_wide_publication_lines};

/// One entry of `gh pr view --json comments`. `id` is spelled the way `gh`
/// really spells it -- a GraphQL node id -- so a test can never accidentally
/// pass by reading the field the edit endpoint cannot use.
fn gh_comment(url: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "IC_kwDOQ07FcM8AAAABUHRMPQ",
        "url": url,
        "body": body,
    })
}

/// A body carrying the marker the projection recognises as its own.
fn ours(body: &str) -> String {
    format!("{}\n{body}", crate::COMMENT_MARKER)
}

// `UsageError` derives no `Debug` -- it cannot implement `Display` either,
// because the blanket `From<E: Display>` above would then collide with
// core's reflexive `From<T> for T` -- so these match rather than `expect`.

#[test]
fn an_aethyme_comment_is_found_by_the_rest_id_in_its_url() {
    let comments = [
        gh_comment("https://github.com/o/r/pull/9#issuecomment-11", "hello"),
        gh_comment(
            "https://github.com/o/r/pull/9#issuecomment-5644766269",
            &ours("body"),
        ),
    ];
    let Ok(Some(owned)) = owned_comment_from_view("o/r", 9, &comments) else {
        panic!("a readable url on our own comment is the ordinary case");
    };
    assert_eq!(owned.id, 5_644_766_269);
}

/// Issue #178's shape once more: a marker with an unreadable id is the one
/// place where refusing beats guessing, because guessing posts a second
/// comment beside the first and does it again every sweep, forever.
#[test]
fn an_unreadable_url_on_our_own_comment_is_refused_not_dropped() {
    let comments = [gh_comment("https://github.com/o/r/pull/9", &ours("body"))];
    let Err(UsageError::Message(message)) = owned_comment_from_view("o/r", 9, &comments) else {
        panic!("our own comment with no recoverable id must refuse");
    };
    assert!(message.contains("o/r#9"), "{message}");
    assert!(message.contains("refusing to project"), "{message}");
}

/// Someone else's comment was never a candidate, so an unreadable url on it
/// costs nothing -- refusing there would stall the projection on a stranger.
#[test]
fn an_unreadable_url_on_a_foreign_comment_is_skipped() {
    let comments = [
        gh_comment("https://github.com/o/r/pull/9", "drive-by"),
        gh_comment(
            "https://github.com/o/r/pull/9#issuecomment-12",
            &ours("body"),
        ),
    ];
    let Ok(Some(owned)) = owned_comment_from_view("o/r", 9, &comments) else {
        panic!("a stranger's unreadable url is not our problem");
    };
    assert_eq!(owned.id, 12);
}

#[test]
fn a_pull_request_aethyme_has_never_commented_on_owns_nothing() {
    let comments = [gh_comment(
        "https://github.com/o/r/pull/9#issuecomment-11",
        "hello",
    )];
    assert!(matches!(
        owned_comment_from_view("o/r", 9, &comments),
        Ok(None)
    ));
}

fn assessment(fast_forward: bool, dirty: &[&str]) -> crate::ship::ShipLocalMainSyncAssessment {
    crate::ship::ShipLocalMainSyncAssessment {
        safe: fast_forward && dirty.is_empty(),
        current_branch_matches: true,
        local_head_unchanged: true,
        fast_forward,
        local_commits_not_in_integration: Vec::new(),
        tracked_dirty_paths: dirty.iter().map(|p| (*p).into()).collect(),
        untracked_paths: Vec::new(),
        conflicting_untracked_paths: Vec::new(),
    }
}

/// Issue #141: "Freshness: Ready" describes the prefix, not the repository.
/// An operator asking to publish everything must be told what is omitted.
#[test]
fn ship_plan_states_whether_the_prefix_represents_all_local_work() {
    let complete = repository_wide_publication_lines(
        &assessment(true, &[]),
        "refs/heads/main",
        "aaaa",
        "aaaa",
    );
    assert_eq!(complete.len(), 1);
    assert!(complete[0].contains("complete"), "{complete:?}");

    let diverged = repository_wide_publication_lines(
        &assessment(false, &[]),
        "refs/heads/main",
        "aaaa",
        "bbbb",
    )
    .join("\n");
    assert!(diverged.contains("INCOMPLETE"), "{diverged}");
    assert!(
        diverged.contains("git log --oneline aaaa..bbbb"),
        "the operator must be able to list what is excluded: {diverged}"
    );

    let dirty = repository_wide_publication_lines(
        &assessment(true, &["src/a.rs", "src/b.rs"]),
        "refs/heads/main",
        "aaaa",
        "aaaa",
    )
    .join("\n");
    assert!(dirty.contains("INCOMPLETE"), "{dirty}");
    assert!(
        dirty.contains("2 uncommitted tracked path(s): src/a.rs, src/b.rs"),
        "uncommitted tracked work must never be silently omitted: {dirty}"
    );
}

/// On another branch, local main says nothing about completeness.
#[test]
fn a_different_checked_out_branch_makes_no_completeness_claim() {
    let mut other = assessment(true, &[]);
    other.current_branch_matches = false;
    assert!(
        repository_wide_publication_lines(&other, "refs/heads/main", "aaaa", "bbbb").is_empty()
    );
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

#[test]
fn status_queue_projection_separates_current_from_terminal_states() {
    for status in [
        crate::MergeStatus::Submitted,
        crate::MergeStatus::Simulating,
        crate::MergeStatus::Conflict,
        crate::MergeStatus::Verified,
    ] {
        assert!(super::queue_status_is_current(status));
    }
    for status in [
        crate::MergeStatus::Promoted,
        crate::MergeStatus::ExternallyLanded,
        crate::MergeStatus::Rejected,
        crate::MergeStatus::Superseded,
    ] {
        assert!(!super::queue_status_is_current(status));
    }
}

#[test]
fn telemetry_classification_tracks_semantic_mutability() {
    for command in [
        args(&["certify"]),
        args(&["readiness", "--require", "agent-ready"]),
        args(&["hooks", "status"]),
        args(&["leases", "plan", "src/lib.rs"]),
        args(&["queue"]),
        args(&["events"]),
        args(&["events", "--follow"]),
        args(&["metrics"]),
        args(&["handoff", "--session", "7"]),
        args(&["handoff", "--worktree", "."]),
        args(&["report", "capture"]),
        args(&["report", "list"]),
        args(&["report", "show", "report.json"]),
        args(&["report", "render", "report.json"]),
        args(&["checkpoint", "plan", "--session", "7"]),
        args(&["gates", "validate"]),
        args(&["gates", "affected", "--session", "7"]),
        args(&["gates", "semantic", "--session", "7"]),
        args(&["gates", "doctor"]),
        args(&["doctor"]),
        args(&["gc", "plan"]),
        args(&["storage"]),
        args(&["storage", "plan"]),
        args(&["operations"]),
        args(&["operations", "stats"]),
        args(&["advisories", "list"]),
        args(&["advisories", "show", "1"]),
        args(&["external-events", "list"]),
        args(&["external-events", "show", "1"]),
        args(&["review", "show", "--session", "7"]),
        args(&["git", "--session", "7", "--", "status"]),
        args(&[
            "gh",
            "--session",
            "7",
            "--repo",
            "o/r",
            "--",
            "pr",
            "view",
            "1",
        ]),
    ] {
        assert!(
            !super::command_records_metric(&command),
            "read-only reporter should not record telemetry: {command:?}"
        );
    }

    for command in [
        args(&["hooks", "install"]),
        args(&["events", "prune", "--keep-days", "7"]),
        args(&["gates", "run", "--session", "7"]),
        args(&["gates", "doctor", "--probe"]),
        args(&["doctor", "--fix-version"]),
        args(&["gc", "apply", "--confirm", "digest"]),
        args(&["storage", "apply", "--confirm", "digest"]),
        args(&["report", "file", "reviewed.issue.md"]),
        args(&[
            "checkpoint",
            "apply",
            "--session",
            "7",
            "--confirm",
            "digest",
        ]),
        args(&["status"]),
        args(&["agents"]),
        args(&["leases"]),
        args(&["integration", "status"]),
        args(&["operations", "reconcile", "--operation", "1"]),
        args(&["advisories", "ack", "1"]),
        args(&["advisories", "suppress", "1"]),
        args(&["external-events", "ingest", "event.json"]),
        args(&[
            "external-events",
            "reconcile",
            "1",
            "--outcome",
            "ignore",
            "--reason",
            "not-applicable",
        ]),
        args(&[
            "review",
            "register",
            "--session",
            "7",
            "--repo",
            "o/r",
            "--pr",
            "1",
        ]),
        args(&["review", "request", "--session", "7"]),
        args(&["review", "unlock", "--session", "7"]),
        args(&["git", "--session", "7", "--", "push"]),
        args(&[
            "gh",
            "--session",
            "7",
            "--repo",
            "o/r",
            "--",
            "pr",
            "merge",
            "1",
        ]),
    ] {
        assert!(
            super::command_records_metric(&command),
            "stateful command should record telemetry: {command:?}"
        );
    }
}

#[test]
fn parse_accepts_readiness_requirement() {
    let args = ["--require".into(), "parallel-ready".into(), "--json".into()];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("readiness flags should parse"),
    };
    assert_eq!(parsed.required_mode.as_deref(), Some("parallel-ready"));
    assert!(parsed.json);
}

#[test]
fn parse_accepts_explicit_pull_request_review_target() {
    let parsed = super::parse(&args(&[
        "start",
        "--task",
        "review workspace",
        "--pull-request",
        "42",
    ]))
    .unwrap_or_else(|_| panic!("explicit pull-request target should parse"));

    assert_eq!(parsed.pull_request, Some(42));
}

#[test]
fn parse_accepts_chau7_session_identity_flags() {
    let parsed = super::parse(&args(&[
        "start-agent",
        "--task",
        "review workspace",
        "--cmd",
        "claude --continue",
        "--repo-name",
        "Aethyme",
        "--tab-name",
        "Fix auth",
        "--ai-provider",
        "claude",
    ]))
    .unwrap_or_else(|_| panic!("Chau7 session identity flags should parse"));

    assert_eq!(parsed.repo_name.as_deref(), Some("Aethyme"));
    assert_eq!(parsed.tab_name.as_deref(), Some("Fix auth"));
    assert_eq!(parsed.ai_provider.as_deref(), Some("claude"));
    assert_eq!(
        super::session_context(&parsed),
        crate::SessionContext::new(
            Some("Aethyme".into()),
            Some("Fix auth".into()),
            Some("claude".into())
        )
    );
}

#[test]
fn parse_accepts_read_only_exact_gate_scope_evaluation() {
    let parsed = match super::parse(&args(&[
        "gates",
        "scope",
        "--base",
        "refs/heads/main",
        "--head",
        "feature",
        "--json",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("exact gate scope should parse"),
    };
    assert_eq!(parsed.positional, vec!["gates", "scope"]);
    assert_eq!(parsed.base.as_deref(), Some("refs/heads/main"));
    assert_eq!(parsed.head.as_deref(), Some("feature"));
    assert!(parsed.json);
    assert!(!super::command_records_metric(&args(&[
        "gates", "scope", "--base", "main", "--head", "feature"
    ])));
    assert!(!super::command_records_metric(&args(&[
        "gates", "manifest", "--head", "feature"
    ])));
}

#[test]
fn parse_accepts_explicit_gate_doctor_probe() {
    let parsed = match super::parse(&args(&[
        "doctor",
        "--probe",
        "--only",
        "integration",
        "--json",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("gate doctor probe flags should parse"),
    };
    assert_eq!(parsed.positional, vec!["doctor"]);
    assert!(parsed.probe);
    assert_eq!(parsed.only.as_deref(), Some("integration"));
    assert!(parsed.json);
}

#[test]
fn parse_accepts_gc_plan_and_confirmed_apply() {
    let Ok(plan) = super::parse(&args(&["plan", "--json"])) else {
        panic!("gc plan should parse");
    };
    assert_eq!(plan.positional, vec!["plan"]);
    assert!(plan.json);

    let Ok(apply) = super::parse(&args(&["apply", "--confirm", "aabb"])) else {
        panic!("gc apply should parse");
    };
    assert_eq!(apply.positional, vec!["apply"]);
    assert_eq!(apply.confirm.as_deref(), Some("aabb"));
}

#[test]
fn parse_accepts_checkpoint_recovery_confirmation() {
    let parsed = super::parse(&args(&[
        "checkpoint",
        "apply",
        "--session",
        "7",
        "--confirm",
        "aabb",
        "--json",
    ]))
    .unwrap_or_else(|_| panic!("checkpoint recovery should parse"));
    assert_eq!(parsed.positional, vec!["checkpoint", "apply"]);
    assert_eq!(parsed.session, Some(7));
    assert_eq!(parsed.confirm.as_deref(), Some("aabb"));
    assert!(parsed.json);
}

#[test]
fn parse_accepts_read_only_lease_plan_with_multiple_paths() {
    let parsed = match super::parse(&args(&[
        "leases",
        "plan",
        "src/lib.rs",
        "docs/",
        "--session",
        "7",
        "--json",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("lease plan should parse"),
    };
    assert_eq!(
        parsed.positional,
        vec!["leases", "plan", "src/lib.rs", "docs/"]
    );
    assert_eq!(parsed.session, Some(7));
    assert!(parsed.json);
    assert!(!super::command_records_metric(&args(&[
        "leases",
        "plan",
        "src/lib.rs"
    ])));
}

#[test]
fn parse_accepts_repeated_planned_session_paths() {
    let parsed = match super::parse(&args(&[
        "--task",
        "rewrite policies",
        "--path",
        "generated/",
        "--path",
        "AGENTS.md",
        "--json",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("planned paths should parse"),
    };
    assert_eq!(
        parsed.planned_paths,
        vec!["generated/".to_string(), "AGENTS.md".to_string()]
    );
    assert!(parsed.json);
}

#[test]
fn parse_accepts_read_only_ship_plan() {
    let parsed = match super::parse(&args(&["ship", "plan", "--entry", "42", "--json"])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("ship plan should parse"),
    };
    assert_eq!(parsed.positional, vec!["ship", "plan"]);
    assert_eq!(parsed.entry, Some(42));
    assert!(parsed.json);
    assert!(!super::command_records_metric(&args(&[
        "ship", "plan", "--entry", "42"
    ])));
}

#[test]
fn parse_accepts_delivery_route_and_plan_digest() {
    let digest = "a".repeat(64);
    let parsed = match super::parse(&args(&[
        "ship",
        "execute",
        "--entry",
        "42",
        "--confirm",
        &"b".repeat(40),
        "--delivery",
        "pull_request",
        "--plan",
        &digest,
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("delivery route should parse"),
    };
    assert_eq!(parsed.delivery_mode.as_deref(), Some("pull_request"));
    assert_eq!(parsed.delivery_plan.as_deref(), Some(digest.as_str()));
}

#[test]
fn parse_accepts_read_only_handoff_selectors() {
    let by_session = match super::parse(&args(&["handoff", "--session", "7", "--json"])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("session handoff should parse"),
    };
    assert_eq!(by_session.session, Some(7));
    assert!(by_session.worktree.is_none());
    assert!(by_session.json);

    let by_worktree =
        match super::parse(&args(&["handoff", "--worktree", ".aethyme/worktrees/task"])) {
            Ok(parsed) => parsed,
            Err(_) => panic!("worktree handoff should parse"),
        };
    assert!(by_worktree.session.is_none());
    assert_eq!(
        by_worktree.worktree.as_deref(),
        Some(std::path::Path::new(".aethyme/worktrees/task"))
    );
    assert!(!super::command_records_metric(&args(&[
        "handoff",
        "--session",
        "7",
    ])));
}

#[test]
fn parse_accepts_offline_report_capture_outputs() {
    let parsed = match super::parse(&args(&[
        "capture",
        "--kind",
        "bug",
        "--title",
        "Gate failed",
        "--session",
        "7",
        "--include-task",
        "--output",
        "reviewed.json",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("report capture should parse"),
    };
    assert_eq!(parsed.positional, vec!["capture"]);
    assert_eq!(parsed.kind.as_deref(), Some("bug"));
    assert_eq!(parsed.title.as_deref(), Some("Gate failed"));
    assert_eq!(parsed.session, Some(7));
    assert!(parsed.include_task);
    assert_eq!(
        parsed.output.as_deref(),
        Some(std::path::Path::new("reviewed.json"))
    );
    assert!(!super::command_records_metric(&args(&[
        "report",
        "capture",
        "--kind",
        "bug",
        "--title",
        "Gate failed",
    ])));
}

#[test]
fn parse_accepts_confirmed_ship_execution() {
    let sha = "a".repeat(40);
    let parsed = match super::parse(&args(&[
        "ship",
        "execute",
        "--entry",
        "42",
        "--confirm",
        &sha,
        "--sync-main",
        "--break-glass",
        "--reason",
        "approved emergency publication",
    ])) {
        Ok(parsed) => parsed,
        Err(_) => panic!("ship execute should parse"),
    };
    assert_eq!(parsed.positional, vec!["ship", "execute"]);
    assert_eq!(parsed.entry, Some(42));
    assert_eq!(parsed.confirm.as_deref(), Some(sha.as_str()));
    assert!(parsed.sync_main);
    assert!(parsed.break_glass);
    assert_eq!(
        parsed.reason.as_deref(),
        Some("approved emergency publication")
    );
    assert!(super::command_records_metric(&args(&[
        "ship",
        "execute",
        "--entry",
        "42",
        "--confirm",
        &sha,
    ])));
}

#[test]
fn parse_accepts_explicit_doctor_version_fix() {
    let args = vec!["doctor".to_string(), "--fix-version".to_string()];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("doctor --fix-version should parse"),
    };

    assert_eq!(parsed.positional, vec!["doctor"]);
    assert!(parsed.fix_version);
}

#[test]
fn parse_accepts_quick_test_with_gate() {
    let args = vec!["quick-test".to_string(), "--with-gate".to_string()];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("quick-test --with-gate should parse"),
    };

    assert_eq!(parsed.positional, vec!["quick-test"]);
    assert!(parsed.with_gate);
}

#[test]
fn parse_accepts_integration_wait_stable_seconds() {
    let args = vec![
        "integration".to_string(),
        "wait-stable".to_string(),
        "--seconds".to_string(),
        "30".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("integration wait-stable --seconds should parse"),
    };

    assert_eq!(parsed.positional, vec!["integration", "wait-stable"]);
    assert_eq!(parsed.seconds, Some(30));
}

#[test]
fn parse_accepts_integration_reconcile_options() {
    let args = vec![
        "integration".to_string(),
        "reconcile".to_string(),
        "--upstream".to_string(),
        "origin/main".to_string(),
        "--resolution-file".to_string(),
        "reconciliation.json".to_string(),
        "--write-resolution-template".to_string(),
        "reconciliation-template.json".to_string(),
        "--apply".to_string(),
        "--confirm".to_string(),
        "a".repeat(64),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("integration reconcile options should parse"),
    };

    assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
    assert_eq!(
        parsed.resolution_file.as_deref(),
        Some(std::path::Path::new("reconciliation.json"))
    );
    assert_eq!(
        parsed.write_resolution_template.as_deref(),
        Some(std::path::Path::new("reconciliation-template.json"))
    );
    assert!(parsed.apply);
    assert_eq!(parsed.confirm, Some("a".repeat(64)));
}

#[test]
fn resolution_template_write_is_atomic_and_never_clobbers() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("resolution.json");
    let document = crate::IntegrationReconcileResolutionTemplateDocument {
        schema_version: 2,
        upstream_ref: "origin/main".into(),
        upstream_commit: "a".repeat(40),
        old_integration: "b".repeat(40),
        operator: None,
        resolutions: Vec::new(),
        unrecorded_resolutions: Vec::new(),
    };

    assert!(super::write_reconciliation_resolution_template(&output, &document).is_ok());
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(value["schema_version"], 2);
    assert!(value["operator"].is_null());
    let error = match super::write_reconciliation_resolution_template(&output, &document) {
        Err(super::UsageError::Message(error)) => error,
        _ => panic!("second write should return a no-clobber usage error"),
    };
    assert!(error.contains("refusing to overwrite"), "{error}");
}

#[test]
fn parse_accepts_guarded_exec_command_after_separator() {
    let args = vec![
        "exec".to_string(),
        "--session".to_string(),
        "7".to_string(),
        "--".to_string(),
        "cargo".to_string(),
        "fmt".to_string(),
        "--check".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("exec with -- separator should parse"),
    };

    assert_eq!(parsed.positional, vec!["exec"]);
    assert_eq!(parsed.session, Some(7));
    assert_eq!(parsed.exec_command, vec!["cargo", "fmt", "--check"]);
}

#[test]
fn parse_accepts_coordinated_github_operation() {
    let args = vec![
        "--session".to_string(),
        "7".to_string(),
        "--repo".to_string(),
        "owner/repo".to_string(),
        "--scope".to_string(),
        "pull-request:42".to_string(),
        "--effect".to_string(),
        "write".to_string(),
        "--reason".to_string(),
        "reviewed release workflow".to_string(),
        "--".to_string(),
        "pr".to_string(),
        "merge".to_string(),
        "42".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("coordinated gh operation should parse"),
    };

    assert_eq!(parsed.session, Some(7));
    assert_eq!(parsed.repository.as_deref(), Some("owner/repo"));
    assert_eq!(parsed.scope.as_deref(), Some("pull-request:42"));
    assert_eq!(parsed.effect.as_deref(), Some("write"));
    assert_eq!(parsed.reason.as_deref(), Some("reviewed release workflow"));
    assert_eq!(parsed.exec_command, vec!["pr", "merge", "42"]);
}

#[test]
fn parse_accepts_pr_check_routing_flags() {
    let args = vec![
        "check".to_string(),
        "--target".to_string(),
        "production".to_string(),
        "--pr".to_string(),
        "42".to_string(),
        "--agent".to_string(),
        "Push2prod".to_string(),
        "--dispatch".to_string(),
        "--cmd".to_string(),
        "codex exec prompt".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("pr check flags should parse"),
    };

    assert_eq!(parsed.positional, vec!["check"]);
    assert_eq!(parsed.target.as_deref(), Some("production"));
    assert_eq!(parsed.pr_number, Some(42));
    assert_eq!(parsed.agent.as_deref(), Some("Push2prod"));
    assert!(parsed.dispatch);
    assert_eq!(parsed.cmd.as_deref(), Some("codex exec prompt"));
}

/// `review state` is the reviewer's half of the handoff, and the reviewer
/// is a script. `--detail` was already a boolean elsewhere, so the free
/// text is `--note`; this pins that choice against a future rename that
/// would silently drop the text.
#[test]
fn parse_accepts_the_review_ledger_report_flags() {
    let args = [
        "review",
        "state",
        "--repo",
        "Owner/Repo",
        "--pr",
        "42",
        "--type",
        "security",
        "--state",
        "satisfied",
        "--head",
        "abc123",
        "--note",
        "no findings",
        "--completed-for-commit",
        "def456",
        "--verdict",
        "pass",
        "--reviewer-provider",
        "github",
        "--reviewer-model",
        "gpt-reviewer",
    ]
    .map(String::from)
    .to_vec();
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("review state flags should parse"),
    };
    assert_eq!(parsed.positional, vec!["review", "state"]);
    assert_eq!(parsed.repository.as_deref(), Some("Owner/Repo"));
    assert_eq!(parsed.pr_number, Some(42));
    assert_eq!(parsed.review_type.as_deref(), Some("security"));
    assert_eq!(parsed.review_state.as_deref(), Some("satisfied"));
    assert_eq!(parsed.head.as_deref(), Some("abc123"));
    assert_eq!(parsed.note.as_deref(), Some("no findings"));
    assert_eq!(parsed.completed_for_commit.as_deref(), Some("def456"));
    assert_eq!(parsed.verdict.as_deref(), Some("pass"));
    assert_eq!(parsed.reviewer_provider.as_deref(), Some("github"));
    assert_eq!(parsed.reviewer_model.as_deref(), Some("gpt-reviewer"));
    assert!(!parsed.detail, "--detail stays the boolean it already was");
}

#[test]
fn parse_accepts_metadata_only_pull_request_watch_flags() {
    let args = vec![
        "pr".to_string(),
        "start".to_string(),
        "--session".to_string(),
        "17".to_string(),
        "--repo".to_string(),
        "Owner/Repo".to_string(),
        "--pr".to_string(),
        "42".to_string(),
        "--events".to_string(),
        "comments,reviews".to_string(),
        "--seconds".to_string(),
        "90".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("watch flags should parse"),
    };
    assert_eq!(parsed.positional, vec!["pr", "start"]);
    assert_eq!(parsed.session, Some(17));
    assert_eq!(parsed.repository.as_deref(), Some("Owner/Repo"));
    assert_eq!(parsed.pr_number, Some(42));
    assert_eq!(parsed.events.as_deref(), Some("comments,reviews"));
    assert_eq!(parsed.seconds, Some(90));
}

#[test]
fn parse_accepts_foreground_pr_scheduler_limit() {
    let args = vec![
        "pr".to_string(),
        "tick".to_string(),
        "--limit".to_string(),
        "17".to_string(),
        "--json".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("scheduler tick flags should parse"),
    };
    assert_eq!(parsed.positional, vec!["pr", "tick"]);
    assert_eq!(parsed.limit, Some(17));
    assert!(parsed.json);
}

#[test]
fn parse_accepts_provider_neutral_delivery_claim_fence() {
    let args = vec![
        "complete".to_string(),
        "--id".to_string(),
        "12".to_string(),
        "--worker".to_string(),
        "chau7-main".to_string(),
        "--generation".to_string(),
        "3".to_string(),
        "--outcome".to_string(),
        "retry".to_string(),
        "--error-code".to_string(),
        "tab_busy".to_string(),
    ];
    let parsed = match super::parse(&args) {
        Ok(parsed) => parsed,
        Err(_) => panic!("delivery completion fence should parse"),
    };
    assert_eq!(parsed.positional, vec!["complete"]);
    assert_eq!(parsed.note_id, Some(12));
    assert_eq!(parsed.worker.as_deref(), Some("chau7-main"));
    assert_eq!(parsed.generation, Some(3));
    assert_eq!(parsed.outcome.as_deref(), Some("retry"));
    assert_eq!(parsed.error_code.as_deref(), Some("tab_busy"));
}

#[test]
fn upstream_relation_names_both_sides_of_divergence() {
    assert_eq!(
        super::upstream_relation(35, 213),
        "diverged: 35 local-only commits, 213 upstream-only commits"
    );
    assert_eq!(
        super::upstream_relation(0, 1),
        "local main behind by 1 commit"
    );
    assert_eq!(
        super::upstream_relation(2, 0),
        "local main ahead by 2 commits"
    );
    assert_eq!(
        super::upstream_relation(0, 0),
        "fetched upstream matches local main"
    );
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// `(subcommand, positionals, flags)` -> the refusal, if any.
fn refusal(subcommand: &str, positional: &[&str], flags: &[&str]) -> Option<String> {
    super::flag_refusal(subcommand, &strings(positional), &strings(flags))
}

/// The #285 bug class: a flag valid for some subcommand but not the one
/// given is refused, naming the flag, the subcommand and where it applies.
#[test]
fn a_flag_valid_elsewhere_is_refused_with_where_it_applies() {
    let cases: &[(&str, &[&str], &[&str], &str)] = &[
        (
            "submit",
            &[],
            &["--session", "--claim"],
            "`--claim` is not valid for `broker submit`; it applies to: start, adopt",
        ),
        (
            "status",
            &[],
            &["--verify-only"],
            "`--verify-only` is not valid for `broker status`; it applies to: submit",
        ),
        (
            "gates",
            &["draft"],
            &["--base"],
            "`--base` is not valid for `broker gates draft`; it applies to: start, start-agent, review plan, review run, review tick, gates scope",
        ),
        (
            "gates",
            &["run"],
            &["--probe"],
            "`--probe` is not valid for `broker gates run`; it applies to: gates doctor",
        ),
        (
            "leases",
            &["plan", "src/lib.rs"],
            &["--ttl"],
            "`--ttl` is not valid for `broker leases plan`; it applies to: resources renew, resources release, leases claim",
        ),
        (
            "status",
            &[],
            &["--", "echo"],
            "`--` (the command separator) is not valid for `broker status`",
        ),
        (
            "finish",
            &[],
            &["--session", "--require"],
            "`--require` is not valid for `broker finish`; it applies to: readiness",
        ),
    ];
    for (subcommand, positional, flags, expected) in cases {
        let message = refusal(subcommand, positional, flags)
            .unwrap_or_else(|| panic!("{subcommand} {positional:?} {flags:?} must refuse"));
        assert!(
            message.starts_with(expected),
            "{subcommand} {positional:?} {flags:?}: {message}"
        );
    }
}

/// `adopt --base` keeps the explanation of why no base can apply.
#[test]
fn adopt_base_refusal_keeps_its_explanation() {
    let message = refusal("adopt", &[], &["--base"]).expect("adopt --base refuses");
    assert!(
        message.contains("`--base` is not valid for `broker adopt`"),
        "{message}"
    );
    assert!(message.contains("already has its own history"), "{message}");
    assert!(message.contains("start --base <ref>"), "{message}");
}

#[test]
fn flags_the_subcommand_reads_are_accepted() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        (
            "start",
            &[],
            &["--task", "--base", "--claim", "--path", "--json"],
        ),
        (
            "adopt",
            &["/tmp/x"],
            &["--task", "--claim", "--reuse", "--sync-integration"],
        ),
        (
            "submit",
            &[],
            &["--session", "--verify-only", "--no-cache", "--json"],
        ),
        ("status", &[], &["--summary", "--json"]),
        ("gates", &["scope"], &["--base", "--head"]),
        ("gates", &["doctor"], &["--probe", "--only"]),
        ("leases", &["claim", "src/"], &["--session", "--ttl"]),
        ("watch", &["pr", "start"], &["--session", "--repo", "--pr"]),
        ("main", &["reconcile", "apply"], &["--session", "--confirm"]),
        ("exec", &[], &["--session", "--"]),
        ("cleanup", &["12"], &["--force"]),
        ("readiness", &[], &["--require"]),
        // An unknown action is held to the union; its handler names it.
        ("gates", &["nonsense"], &["--probe"]),
        // An unknown subcommand is the dispatcher's to report.
        ("no-such-subcommand", &[], &["--claim"]),
    ];
    for (subcommand, positional, flags) in cases {
        assert_eq!(
            refusal(subcommand, positional, flags),
            None,
            "{subcommand} {positional:?} {flags:?}"
        );
    }
}

/// Validation reaches the process as a usage error, exit 2.
#[test]
fn a_refused_flag_is_a_usage_error() {
    let parsed = super::parse(&strings(&["--session", "4", "--claim", "symbol:A"]))
        .unwrap_or_else(|_| panic!("parses"));
    match super::validate_flags("submit", &parsed) {
        Err(UsageError::Exit { code, message }) => {
            assert_eq!(code, crate::exit_status::USAGE);
            assert!(message.contains("`--claim`"), "{message}");
        }
        _ => panic!("submit --claim must be a usage error"),
    }
}

/// Every flag the table names must be one the parser knows; a typo here
/// would otherwise make a flag unusable everywhere.
#[test]
fn every_table_flag_is_a_parsed_flag() {
    for (path, flags) in super::FLAG_RULES {
        for flag in *flags {
            if *flag == "--" {
                continue;
            }
            let result = super::parse(&strings(&[flag, "1"]));
            if let Err(UsageError::Message(message)) = &result {
                assert!(
                    !message.starts_with("unknown flag"),
                    "{path}: {flag} is not a parsed flag"
                );
            }
        }
    }
}

/// The `(path words, flags)` each `aethyme broker ...` line of a help text
/// documents, with `a|b` actions and an optional `[a|b]` action expanded.
fn documented_invocations(text: &str) -> Vec<(Vec<String>, Vec<String>)> {
    // Handled outside this parser: `check-contract` dispatches before it,
    // and the readiness repair verbs belong to the top-level router.
    const ELSEWHERE: &[&str] = &[
        "check-contract",
        "readiness plan",
        "readiness apply",
        "readiness recover",
    ];
    let mut invocations = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("aethyme broker ") else {
            continue;
        };
        if ELSEWHERE.iter().any(|prefix| rest.starts_with(prefix)) {
            continue;
        }
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let mut paths: Vec<Vec<String>> = vec![Vec::new()];
        for token in &tokens {
            let optional = token.starts_with('[') && token.ends_with(']');
            let bare = token.trim_start_matches('[').trim_end_matches(']');
            let words: Vec<&str> = bare.split('|').collect();
            let is_action = !words.is_empty()
                && words.iter().all(|word| {
                    !word.is_empty()
                        && word.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                        && !word.starts_with('-')
                });
            if !is_action || (token.starts_with('[') && !optional) {
                break;
            }
            let mut next = Vec::new();
            for path in &paths {
                if optional {
                    next.push(path.clone());
                }
                for word in &words {
                    let mut extended = path.clone();
                    extended.push((*word).to_string());
                    next.push(extended);
                }
            }
            paths = next;
            if optional {
                break;
            }
        }
        if paths.iter().all(Vec::is_empty) {
            continue;
        }
        let mut flags = Vec::new();
        for token in &tokens {
            let trimmed = token.trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '`'));
            if trimmed == "--" {
                flags.push("--".to_string());
            }
            for part in trimmed.split('|') {
                let part = part.trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '`'));
                if part.len() > 2
                    && part.starts_with("--")
                    && part[2..]
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '-')
                {
                    flags.push(part.to_string());
                }
            }
        }
        for path in paths {
            invocations.push((path, flags.clone()));
        }
    }
    invocations
}

/// Every flag `USAGE` documents for a subcommand passes validation, so the
/// table cannot drift from the help text without this failing.
#[test]
fn every_usage_example_passes_flag_validation() {
    let invocations = documented_invocations(super::USAGE);
    assert!(invocations.len() > 100, "found {}", invocations.len());
    for (path, flags) in invocations {
        assert_eq!(
            super::flag_refusal(&path[0], &path[1..], &flags),
            None,
            "USAGE documents `broker {}` with {flags:?}",
            path.join(" ")
        );
    }
}

/// The same for the reference manual.
#[test]
fn every_cli_reference_example_passes_flag_validation() {
    let text = include_str!("../../../../../docs/reference/cli.md");
    let deprecation_rows: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("| `aethyme "))
        .filter_map(|line| {
            line.find("`aethyme broker ")
                .map(|start| &line[start + 1..])
                .and_then(|rest| rest.split('`').next())
        })
        .collect();
    // The reference spells commands the public way (`status readiness`,
    // `advanced leases claim`, `start --adopt`); flag rules key on the
    // internal command, so each example is resolved the way the router
    // resolves it before validation.
    let lines: String = text
        .lines()
        .filter_map(|line| {
            line.find("`aethyme broker ")
                .map(|start| &line[start + 1..])
                .and_then(|rest| rest.split('`').next())
        })
        .map(|line| {
            let words: Vec<String> = line
                .trim_start_matches("aethyme broker ")
                .split_whitespace()
                .map(|word| if word == "[--json]" { "--json" } else { word })
                .map(str::to_string)
                .collect();
            let resolved = super::resolve(&words);
            assert!(
                resolved.refusal.is_none(),
                "cli.md documents a refused spelling: {line}"
            );
            // Old spellings belong only in the deprecation table.
            assert!(
                resolved.deprecation.is_none() || deprecation_rows.contains(&line),
                "cli.md documents a deprecated spelling: {line}"
            );
            let mut args = resolved.args;
            // `start` merges by flag, as `run_inner` does.
            if args.first().map(String::as_str) == Some("start") {
                let has = |flag: &str| args.iter().any(|arg| arg == flag);
                if has("--adopt") || has("--reuse") || has("--replace-stale") {
                    args[0] = "adopt".to_string();
                } else if has("--cmd") {
                    args[0] = "start-agent".to_string();
                }
            }
            format!("aethyme broker {}\n", args.join(" "))
        })
        .collect();
    let invocations = documented_invocations(&lines);
    assert!(invocations.len() > 50, "found {}", invocations.len());
    for (path, flags) in invocations {
        assert_eq!(
            super::flag_refusal(&path[0], &path[1..], &flags),
            None,
            "cli.md documents `broker {}` with {flags:?}",
            path.join(" ")
        );
    }
}
