//! End-to-end merge queue tests (issues #20-#23): submit → simulate →
//! gates on the merged tree → verified (nothing is promoted; work lands
//! through each session's pull request), and conflicts rejected pre-gate
//! with the agent-facing instruction drop.

use aethyme_broker::{
    Broker, GitRepo, IntegrationReconcileOptions, MergeStatus, StatusAdviceSeverity,
    SubmissionGateVerificationStatus, VERIFIED_AGAINST_INTEGRATION, VERIFIED_AGAINST_LOCAL_DEFAULT,
};
use std::path::Path;
use std::process::Command;

mod common;

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
    // Broker operations spawn their own Git processes, so configure the
    // disposable repository itself instead of relying on the test runner's
    // global identity or on environment variables attached to `sh` above.
    sh(root, &["config", "user.name", "Aethyme Test"]);
    sh(
        root,
        &["config", "user.email", "aethyme-test@example.invalid"],
    );
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.py"), "a = 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "b = 1\n").unwrap();
    std::fs::write(
        root.join(".gitignore"),
        "gate-ran.txt\n.aethyme/broker-action-required.md\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"marker\"\ncommand = \"echo ran >> gate-ran.txt\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn agent_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    agent_worktree_at(root, name, "main")
}

fn agent_worktree_at(root: &Path, name: &str, base: &str) -> std::path::PathBuf {
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
            base,
        ],
    );
    path
}

fn commit_edit(worktree: &Path, file: &str, content: &str) {
    std::fs::write(worktree.join(file), content).unwrap();
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", "edit"]);
}

#[test]
fn submit_reuses_its_preflight_patch_history_plan_when_inputs_stay_fixed() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    // Submission-plan reuse requires the same canonical origin identity used
    // by coordinated local Git writers. The URL is identity-only; no fetch runs.
    sh(
        tmp.path(),
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
    );

    // Finish fixture history before opening the broker: its default database
    // lives in the repository and must not be staged by later branch commits.
    // Main and integration each receive an independent commit from the same
    // initial tree. Integration holding a commit main lacks, with no fetched
    // default branch, submit verifies against main (what sessions are cut
    // from); the session is cut from main's parent, so its plan includes both
    // nonempty patch streams: the session's commit and main's.
    sh(tmp.path(), &["switch", "-qc", "integration-only"]);
    std::fs::write(
        tmp.path().join("src/integration-only.py"),
        "integration = 1\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", "--", "src/integration-only.py"]);
    sh(tmp.path(), &["commit", "-qm", "edit integration"]);
    let integration = resolve(tmp.path(), "HEAD");
    sh(tmp.path(), &["switch", "main"]);
    std::fs::write(tmp.path().join("src/main-only.py"), "main = 1\n").unwrap();
    sh(tmp.path(), &["add", "--", "src/main-only.py"]);
    sh(tmp.path(), &["commit", "-qm", "edit main"]);

    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    // Broker startup may reconcile refs, so restore the intended divergent
    // integration tip after open and before building the session plan.
    sh(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", &integration],
    );
    assert_ne!(
        resolve(tmp.path(), "refs/heads/aethyme/integration"),
        resolve(tmp.path(), "HEAD"),
        "fixture integration must remain distinct from main"
    );
    assert_eq!(
        resolve(tmp.path(), "refs/heads/aethyme/integration^"),
        resolve(tmp.path(), "HEAD^"),
        "fixture integration and main must diverge from their shared initial commit"
    );

    let session_worktree = agent_worktree_at(tmp.path(), "trace-reuse", "main^");
    let session = broker
        .adopt(&session_worktree, Some("trace submit plan reuse"))
        .unwrap();
    std::fs::write(
        session_worktree.join("src/session-only.py"),
        "session = 1\n",
    )
    .unwrap();
    sh(&session_worktree, &["add", "--", "src/session-only.py"]);
    sh(&session_worktree, &["commit", "-qm", "edit session"]);

    let database_path = broker.store().db_path().to_path_buf();
    assert!(
        database_path.is_file(),
        "fixture broker database must still exist before child startup"
    );
    broker
        .store()
        .session(session.id)
        .expect("the adopted session must be readable before child startup");
    let tracked = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(tmp.path())
        .output()
        .expect("list tracked fixture paths");
    assert!(tracked.status.success(), "git ls-files failed");
    let tracked_paths = String::from_utf8_lossy(&tracked.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut database_prefixes = vec![".aethyme/broker.db".to_owned()];
    if let Ok(relative_database) = database_path.strip_prefix(tmp.path()) {
        database_prefixes.push(relative_database.to_string_lossy().into_owned());
    }
    let tracked_database_paths = tracked_paths
        .iter()
        .filter(|path| {
            database_prefixes.iter().any(|prefix| {
                path.as_str() == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('-'))
            })
        })
        .collect::<Vec<_>>();
    assert!(
        tracked_database_paths.is_empty(),
        "the live broker database and WAL files must remain untracked: {tracked_database_paths:?}"
    );

    let trace_path = tmp.path().parent().unwrap().join("submit-trace.json");
    let session_id = session.id.to_string();
    let mut command = common::broker_cli(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["submit", "--session", &session_id, "--json"],
    );
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_SYSTEM",
            tmp.path().join("missing-system-gitconfig"),
        )
        .env(
            "GIT_CONFIG_GLOBAL",
            tmp.path().join("missing-global-gitconfig"),
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1");
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    let output = command
        .current_dir(tmp.path())
        .env("GIT_TRACE2_EVENT", &trace_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "broker submit failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let trace = std::fs::read_to_string(&trace_path).unwrap();
    let patch_history_reads = trace
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .expect("Git Trace2 event log contains valid JSON records")
        })
        .filter(|event| event["event"] == "start")
        .filter(|event| {
            event["argv"].as_array().is_some_and(|argv| {
                argv.iter().any(|arg| arg == "log")
                    && argv.iter().any(|arg| arg == "-p")
                    && argv.iter().any(|arg| arg == "--format=commit %H")
            })
        })
        .count();
    assert_eq!(
        patch_history_reads, 4,
        "contract preflight and submit preflight each read session and integration patch histories once; JSON mode skips the separate human render, and simulation must reuse the submit preflight plan; trace:\n{trace}"
    );
}

#[test]
fn verify_only_submission_plan_and_submit_use_the_same_upstream_base() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", ".aethyme/config.toml"]);
    sh(tmp.path(), &["commit", "-qm", "configure verify-only mode"]);
    let stale_integration = resolve(tmp.path(), "HEAD");
    sh(
        tmp.path(),
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            &stale_integration,
        ],
    );

    sh(tmp.path(), &["switch", "-qc", "external-upstream", "main"]);
    std::fs::write(tmp.path().join("src/upstream.txt"), "upstream\n").unwrap();
    sh(tmp.path(), &["add", "src/upstream.txt"]);
    sh(
        tmp.path(),
        &["commit", "-qm", "advance fetched default branch"],
    );
    let upstream = resolve(tmp.path(), "HEAD");
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", &upstream],
    );
    sh(
        tmp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    sh(tmp.path(), &["switch", "main"]);

    assert_eq!(
        GitRepo::discover(tmp.path()).unwrap().upstream_default(),
        Some(("origin/main".to_string(), upstream.clone()))
    );

    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let worktree = agent_worktree(tmp.path(), "verify-only-base-parity");
    let session = broker
        .adopt(&worktree, Some("verify-only base parity"))
        .unwrap();
    commit_edit(&worktree, "src/notes.md", "session work\n");

    let plan = broker.submission_plan(session.id).unwrap();
    assert_eq!(plan.integration_head, upstream);
    assert_ne!(plan.integration_head, stale_integration);
    // Planning is read-only even when the disposable integration ref is stale.
    assert_eq!(
        resolve(tmp.path(), "aethyme/integration"),
        stale_integration
    );

    let outcome = broker.submit(session.id).unwrap();
    let verified_against = outcome.verified_against.as_ref().unwrap();
    assert_eq!(verified_against.source, "upstream");
    assert_eq!(verified_against.reference, "origin/main");
    assert_eq!(verified_against.commit, upstream);
    assert_eq!(
        outcome.submission_plan.integration_head,
        plan.integration_head
    );
    assert!(!outcome.promoted);
}

fn resolve(root: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The verification commit submit minted and gated for this entry.
fn verification_commit(entry: &aethyme_broker::MergeQueueEntry) -> String {
    serde_json::from_str::<serde_json::Value>(entry.details_json.as_deref().unwrap())
        .unwrap()["merge_commit"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A repository whose consumer inventory tracks `legacy_entry_point`, which
/// `src/a.py` uses, plus one adopted agent worktree.
fn contract_tracked_repo(root: &Path, name: &str) -> (Broker, i64, std::path::PathBuf) {
    init_repo(root);
    let doc = root.join("packages/aethyme/docs/architecture/cross-process-consumers.md");
    std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
    std::fs::write(&doc, "# Consumers\n\n- `legacy_entry_point`\n").unwrap();
    std::fs::write(root.join("src/a.py"), "a = legacy_entry_point\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "track a consumer"]);
    let mut broker = common::open_fixture_broker(
        root,
        root.parent()
            .expect("fixture repository has an owning temp directory"),
    )
    .unwrap()
    .into_broker();
    let worktree = agent_worktree(root, name);
    let session = broker.adopt(&worktree, Some(name)).unwrap();
    (broker, session.id, worktree)
}

fn cli_submit_command(broker: &Broker, root: &Path, session: i64) -> Command {
    let session = session.to_string();
    let mut command = common::broker_cli(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["submit", "--session", &session],
    );
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    command.current_dir(root);
    command
}

// #418: the contract decision is checked before the gates start, against the
// session's commits, which are all the broker's contract gate can see.
#[test]
fn submit_refuses_an_undeclared_tracked_removal_before_any_gate_runs() {
    let tmp = common::FixtureRepository::new();
    let (broker, session, worktree) = contract_tracked_repo(tmp.path(), "undeclared");
    commit_edit(&worktree, "src/a.py", "a = 2\n");
    let mut command = cli_submit_command(&broker, tmp.path(), session);
    drop(broker);

    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("contract-decision preflight failed before any gate ran"),
        "{stderr}"
    );
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    assert!(
        broker
            .store()
            .merge_queue()
            .unwrap()
            .iter()
            .all(|entry| entry.session_id != session),
        "a refused submission must never reach the queue or its gates"
    );
}

// The preflight judges committed work: an uncommitted removal never
// integrates, so it must not refuse a submission whose commits are clean.
#[test]
fn submit_preflight_ignores_uncommitted_tracked_removals() {
    let tmp = common::FixtureRepository::new();
    let (broker, session, worktree) = contract_tracked_repo(tmp.path(), "uncommitted");
    commit_edit(&worktree, "src/b.py", "b = 2\n");
    std::fs::write(worktree.join("src/a.py"), "a = 2\n").unwrap();
    let mut command = cli_submit_command(&broker, tmp.path(), session);
    drop(broker);

    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("contract-decision preflight failed"),
        "{stderr}"
    );
}

#[test]
fn verification_commit_preserves_only_the_owned_contract_decision() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let worktree = agent_worktree(tmp.path(), "contract-decision");
    let session = broker
        .adopt(&worktree, Some("document an agent protocol"))
        .unwrap();
    std::fs::write(worktree.join("src/a.py"), "a = protocol\n").unwrap();
    sh(&worktree, &["add", "src/a.py"]);
    sh(
        &worktree,
        &[
            "commit",
            "-qm",
            "docs: document protocol",
            "-m",
            "Private implementation rationale.\n\nContract decision: introduce",
        ],
    );

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let verified = verification_commit(&outcome.entry);
    let message = Command::new("git")
        .args(["show", "-s", "--format=%B", &verified])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(message.status.success());
    let message = String::from_utf8_lossy(&message.stdout);
    assert!(message.contains("Contract decision: introduce"));
    assert!(!message.contains("Private implementation rationale"));
}

// The contract gate runs against the verification commit. A justified `none` must
// reach it, or no truthful label can pass once the matcher fires.
#[test]
fn verification_commit_carries_the_contract_justification() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let worktree = agent_worktree(tmp.path(), "contract-justification");
    let session = broker
        .adopt(&worktree, Some("remove generated data"))
        .unwrap();
    std::fs::write(worktree.join("src/a.py"), "a = data\n").unwrap();
    sh(&worktree, &["add", "src/a.py"]);
    sh(
        &worktree,
        &[
            "commit",
            "-qm",
            "chore: drop generated data",
            "-m",
            "Private implementation rationale.\n\nContract decision: none\n\n\
             Contract justification: only generated data leaves the tree",
        ],
    );

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let verified = verification_commit(&outcome.entry);
    let message = Command::new("git")
        .args(["show", "-s", "--format=%B", &verified])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(message.status.success());
    let message = String::from_utf8_lossy(&message.stdout);
    assert!(message.contains("Contract decision: none"));
    assert!(
        message.contains("Contract justification: only generated data leaves the tree"),
        "{message}"
    );
    assert!(!message.contains("Private implementation rationale"));
}

#[test]
fn conflicting_submission_rejected_pre_gate_with_instruction_drop() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();

    let wt_a = agent_worktree(tmp.path(), "a");
    let wt_b = agent_worktree(tmp.path(), "b");
    let a = broker.adopt(&wt_a, None).unwrap();
    let b = broker.adopt(&wt_b, None).unwrap();

    // Both change the same line of the same file.
    commit_edit(&wt_a, "src/a.py", "a = 111\n");
    commit_edit(&wt_b, "src/a.py", "a = 222\n");
    // Refresh leases before A lands. Once it lands, its implicit lease may
    // correctly disappear because its work is integrated; the Git conflict
    // remains real even when no live lease owner remains to name.
    broker.refresh_leases().unwrap();

    let out_a = broker.submit(a.id).unwrap();
    assert_eq!(out_a.entry.status, MergeStatus::Verified);
    // A lands the way work lands: by pull request.
    aethyme_testkit::land_as_pull_request(
        tmp.path(),
        "agent/a",
        aethyme_testkit::MergeStyle::MergeCommit,
    );

    let out_b = broker.submit(b.id).unwrap();
    assert_eq!(out_b.entry.status, MergeStatus::Conflict);
    assert_eq!(
        broker.store().session(b.id).unwrap().accepted_session_head,
        None,
        "a conflicted submission must not advance the checkpoint"
    );
    assert_eq!(out_b.conflicts, vec!["src/a.py".to_string()]);
    assert!(
        out_b.gate_outcomes.is_empty(),
        "conflict rejected BEFORE any gate ran"
    );
    // Marker file proves the gate never executed for B's submission
    // beyond A's earlier verified run.
    let details = out_b.entry.details_json.as_deref().unwrap();
    let details_json: serde_json::Value = serde_json::from_str(details).unwrap();
    assert!(
        details_json["blocking_sessions"].is_array(),
        "conflict details must retain the blocking-session evidence field: {details}"
    );

    // The agent-facing instruction drop exists in B's worktree and names
    // the conflicting file and the blocking session (#21 decision).
    let note = std::fs::read_to_string(wt_b.join(aethyme_broker::ACTION_REQUIRED_RELPATH)).unwrap();
    assert!(note.contains("src/a.py"));
    assert!(note.contains(&format!("session {}", b.id)) || note.contains("Blocking session"));
    assert!(note.contains("rebase"));

    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.latest-submit-conflict")
        .expect("latest conflict should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.session_id, Some(b.id));
    assert_eq!(advice.queue_entry_id, Some(out_b.entry.id));
    assert!(advice.summary.contains("conflicted"));
    assert!(
        advice.evidence.iter().any(|line| line.contains("src/a.py")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command.contains("broker-action-required.md")),
        "{advice:?}"
    );
    assert!(!advice.summary.contains("stash"), "{advice:?}");
    assert!(
        advice
            .commands
            .iter()
            .all(|command| !command.contains("stash")),
        "{advice:?}"
    );

    let err = broker.repair(b.id).unwrap_err().to_string();
    assert!(
        err.contains("repair paused during rebase"),
        "repair should apply the documented rebase path and stop for true content conflicts: {err}"
    );
}

#[test]
fn submission_with_unchanged_result_is_a_truthful_noop() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let worktree = agent_worktree(tmp.path(), "nothing-to-submit");
    let session = broker.adopt(&worktree, Some("inspect only")).unwrap();
    let submitted_head = resolve(&worktree, "HEAD");
    let integration_before = broker.integration_head().unwrap().1;
    let integration_tree = resolve(tmp.path(), "aethyme/integration^{tree}");

    let outcome = broker.submit(session.id).unwrap();

    assert!(outcome.no_changes);
    assert!(!outcome.promoted);
    assert_eq!(outcome.entry.status, MergeStatus::Superseded);
    assert!(outcome.gate_outcomes.is_empty());
    assert_eq!(
        outcome.gate_verification.status,
        SubmissionGateVerificationStatus::NotRun
    );
    assert_eq!(broker.integration_head().unwrap().1, integration_before);
    let details: serde_json::Value =
        serde_json::from_str(outcome.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["reason"], "submission produces no content change");
    let accepted = broker.store().session(session.id).unwrap();
    assert_eq!(accepted.accepted_session_head, Some(submitted_head));
    assert_eq!(
        accepted.accepted_integration_commit,
        Some(integration_before)
    );
    assert_eq!(accepted.accepted_integration_tree, Some(integration_tree));
    assert_eq!(accepted.accepted_queue_entry_id, Some(outcome.entry.id));
    assert!(accepted.accepted_at.is_some());
}

#[test]
fn base_gate_policy_judges_a_submission_that_weakens_or_deletes_its_gates() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    // The failing gate is committed at the base: submission policy comes from
    // the tree the change lands on, not from the session.
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"fail\"\ncommand = \"exit 7\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "base: failing gate"]);
    let wt = agent_worktree(tmp.path(), "x");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 3\n");

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Rejected);
    assert_eq!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .accepted_session_head,
        None,
        "a rejected submission must not advance the checkpoint"
    );
    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.latest-submit-rejected")
        .expect("latest rejected submit should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.session_id, Some(session.id));
    assert_eq!(advice.queue_entry_id, Some(outcome.entry.id));
    assert!(advice.summary.contains("fail"));
    assert!(
        advice
            .evidence
            .iter()
            .any(|line| line.contains("gate fail status fail")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command == &format!("aethyme broker submit --session {}", session.id)),
        "{advice:?}"
    );

    // The session cannot pass by weakening its own gate: the base policy
    // still judges it, and the attempted change is recorded.
    std::fs::write(
        wt.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"fail\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    commit_edit(&wt, "src/a.py", "a = 4\n");
    let weakened = broker.submit(session.id).unwrap();
    assert_eq!(weakened.entry.status, MergeStatus::Rejected);
    let deferred = broker
        .store()
        .events_after_filtered(0, 50, Some("merge.policy_deferred"))
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(deferred.last().unwrap().payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(payload["gates_changed"], true);
    assert_eq!(payload["graph_policy_changed"], false);

    // Deleting the gate config does not escape verification either.
    std::fs::remove_file(wt.join(".aethyme/gates.toml")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 5\n");
    let deleted = broker.submit(session.id).unwrap();
    assert_eq!(deleted.entry.status, MergeStatus::Rejected);
    assert!(!deleted.gate_outcomes.is_empty());

    // Fixing the gate at the base flips it to verified.
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"ok\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "base: passing gate"]);
    // A fresh session cut after the base fix verifies; the rejected session's
    // history edits the gate file and would have to be rebased first.
    let wt_fixed = agent_worktree(tmp.path(), "fixed");
    let fixed = broker.adopt(&wt_fixed, None).unwrap();
    commit_edit(&wt_fixed, "src/a.py", "a = 6\n");
    let outcome = broker.submit(fixed.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    assert!(!outcome.promoted, "nothing is promoted");
}

#[test]
fn repo_without_gates_is_a_pure_conflict_manager() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();

    // Conflict-only mode is committed repository state at the base: a
    // session removing gates.toml on its own would not escape its gates.
    std::fs::remove_file(tmp.path().join(".aethyme/gates.toml")).unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "base: conflict-only"]);
    let wt = agent_worktree(tmp.path(), "solo");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 9\n");

    // Clean merge, zero gates configured -> verified with no gate run,
    // and the outcome makes that visible (empty gate_outcomes).
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    assert!(outcome.gate_outcomes.is_empty());
    assert_eq!(
        outcome.gate_verification.status,
        SubmissionGateVerificationStatus::NoConfiguration
    );

    // Conflicts are still caught: the first session lands by pull request,
    // and a second session edits the same line.
    aethyme_testkit::land_as_pull_request(
        tmp.path(),
        "agent/solo",
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    let wt_b = agent_worktree(tmp.path(), "rival");
    let rival = broker.adopt(&wt_b, None).unwrap();
    commit_edit(&wt_b, "src/a.py", "a = 10\n");
    let out = broker.submit(rival.id).unwrap();
    assert_eq!(out.entry.status, MergeStatus::Conflict);
}

#[test]
fn integration_follows_main_without_a_fetched_default_branch() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();

    // Branch created at main HEAD; main then advances → integration
    // fast-forwards on the next touch (issue #40), with an event.
    let (_branch, base) = broker.integration_head().unwrap();
    assert_eq!(base, resolve(tmp.path(), "main"));
    std::fs::write(tmp.path().join("src/new.py"), "n\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "main advances"]);
    let (_branch, refreshed) = broker.integration_head().unwrap();
    assert_eq!(refreshed, resolve(tmp.path(), "main"), "fast-forwarded");
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "merge.integration_refreshed")
    );
}

#[test]
fn integration_follows_main_but_never_clobbers_unmerged_promotions() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let (_branch, base) = broker.integration_head().unwrap();
    assert_eq!(base, resolve(tmp.path(), "main"));

    // A commit local promotion left on integration, which main lacks:
    // advancing main afterwards must NOT clobber it. The no-remote fallback
    // only fast-forwards a ref that is an ancestor of the main checkout.
    let promoted_tip = common::commit_onto_integration(
        tmp.path(),
        &[("src/promoted.py", "promoted = 1\n")],
        "work promoted before local promotion was removed",
    );
    std::fs::write(tmp.path().join("src/other.py"), "o\n").unwrap();
    sh(tmp.path(), &["add", "src/other.py"]);
    sh(tmp.path(), &["commit", "-qm", "main advances again"]);
    let (_branch, still) = broker.integration_head().unwrap();
    assert_eq!(
        still, promoted_tip,
        "unmerged promotions are never clobbered by a refresh"
    );
    assert_eq!(
        resolve(tmp.path(), "refs/heads/aethyme/integration"),
        promoted_tip
    );
}

/// Formerly promoting, no remote: integration holds commits local main lacks.
/// Sessions are cut from local main, so submit verifies against local main --
/// not the stale integration ref -- and says why; status names the leftover
/// commits and the command that resolves them.
#[test]
fn a_formerly_promoting_repository_without_a_remote_verifies_against_local_main() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    broker.integration_head().unwrap();
    let main = resolve(tmp.path(), "main");
    let leftover = common::commit_onto_integration(
        tmp.path(),
        &[("src/a.py", "a = 99\n")],
        "leftover promotion",
    );

    let status = broker.status(0).unwrap();
    let row = status
        .advice
        .iter()
        .find(|row| row.id == "integration.leftover-work")
        .expect("a formerly promoting repository without a remote gets the leftover row");
    assert_eq!(row.severity, StatusAdviceSeverity::Warning);
    assert!(
        row.summary.contains("1 commit main lacks"),
        "{}",
        row.summary
    );
    assert!(
        row.summary.contains("submit verifies against main"),
        "{}",
        row.summary
    );
    assert_eq!(
        row.commands,
        vec!["aethyme broker advanced integration reconcile --upstream main --dry-run"]
    );

    // The session holds main's a.py; integration's leftover rewrites the same
    // line. Judged against integration this would conflict with work the
    // session does not hold; judged against main it verifies.
    let wt = agent_worktree(tmp.path(), "after-promotion");
    let session = broker.adopt(&wt, Some("work after promotion")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let plan = broker.submission_plan(session.id).unwrap();
    assert_eq!(plan.integration_head, main);
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    assert_eq!(outcome.entry.base_commit, main);
    let base = outcome.verified_against.as_ref().unwrap();
    assert_eq!(base.source, VERIFIED_AGAINST_LOCAL_DEFAULT);
    assert_eq!(base.reference, "main");
    assert_eq!(base.commit, main);
    assert!(
        base.fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("aethyme/integration holds 1 commit main lacks")),
        "{base:?}"
    );
    assert_eq!(
        resolve(tmp.path(), "refs/heads/aethyme/integration"),
        leftover,
        "verification never moves the leftover integration ref"
    );
}

/// The no-remote fallback is unchanged while integration holds nothing local
/// main lacks: it follows main and is still the verification base.
#[test]
fn a_repository_without_a_remote_still_verifies_against_a_current_integration() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let wt = agent_worktree(tmp.path(), "current-integration");
    let session = broker.adopt(&wt, Some("ordinary work")).unwrap();
    commit_edit(&wt, "src/b.py", "b = 2\n");
    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let base = outcome.verified_against.as_ref().unwrap();
    assert_eq!(base.source, VERIFIED_AGAINST_INTEGRATION);
    assert_eq!(base.commit, resolve(tmp.path(), "main"));
    assert!(
        broker
            .status(0)
            .unwrap()
            .advice
            .iter()
            .all(|row| row.id != "integration.leftover-work")
    );
}

#[test]
fn failed_reconciliation_rolls_back_ref_and_database() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let wt = agent_worktree(tmp.path(), "rollback-promotion");
    let session = broker.adopt(&wt, Some("rollback proof")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let submitted = broker.submit(session.id).unwrap();
    assert_eq!(submitted.entry.status, MergeStatus::Verified);
    common::promote_like_before_removal(tmp.path(), &submitted.entry);
    let old_integration = resolve(tmp.path(), "aethyme/integration");

    sh(tmp.path(), &["switch", "-qc", "rollback-upstream", "main"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "external squash"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let dry_run = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(dry_run.safe, "{dry_run:#?}");

    let db =
        rusqlite::Connection::open(aethyme_broker::broker_db_path(tmp.path()).unwrap()).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_reconcile
         BEFORE INSERT ON integration_reconciliations
         BEGIN SELECT RAISE(FAIL, 'injected reconciliation failure'); END;",
    )
    .unwrap();
    drop(db);

    let error = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: dry_run.plan_digest,
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected reconciliation failure")
    );
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
    let queue = broker.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|entry| entry.id == submitted.entry.id)
            .unwrap()
            .status,
        MergeStatus::Promoted,
        "failed apply must leave the broker row unchanged"
    );
}

#[test]
fn broker_open_finishes_reconciliation_interrupted_after_ref_move() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let wt = agent_worktree(tmp.path(), "crash-recovery-promotion");
    let session = broker.adopt(&wt, Some("crash recovery proof")).unwrap();
    commit_edit(&wt, "src/a.py", "a = 2\n");
    let submitted = broker.submit(session.id).unwrap();
    assert_eq!(submitted.entry.status, MergeStatus::Verified);
    common::promote_like_before_removal(tmp.path(), &submitted.entry);

    sh(
        tmp.path(),
        &["switch", "-qc", "crash-recovery-upstream", "main"],
    );
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "src/a.py"]);
    sh(tmp.path(), &["commit", "-qm", "external squash"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(tmp.path(), &["switch", "main"]);

    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(report.safe, "{report:#?}");
    let entry = &report.entries[0];
    let details = serde_json::json!({
        "branch": report.branch,
        "commit": entry.old_merge_commit,
        "externally_landed": true,
        "classification": "already_landed",
        "upstream_ref": report.upstream_ref,
        "upstream_landing": entry.upstream_landing,
    })
    .to_string();
    let db_path = aethyme_broker::broker_db_path(tmp.path()).unwrap();
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO integration_reconciliation_intent
            (id, branch, upstream_ref, local_main_commit, old_integration,
             upstream_commit, new_integration, created_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, 1)",
        rusqlite::params![
            report.branch,
            report.upstream_ref,
            report.local_main,
            report.old_integration,
            report.upstream_head,
            report.new_integration,
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO integration_reconciliation_intent_entries
            (queue_entry_id, status, details_json, classification,
             old_merge_commit, upstream_landing)
         VALUES (?1, 'externally_landed', ?2, 'already_landed', ?3, ?4)",
        rusqlite::params![
            entry.queue_entry_id,
            details,
            entry.old_merge_commit,
            entry.upstream_landing,
        ],
    )
    .unwrap();
    drop(db);
    sh(
        tmp.path(),
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            &report.new_integration,
            &report.old_integration,
        ],
    );
    drop(broker); // Simulate process death before phase-two SQLite commit.

    let mut recovered = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let queue = recovered.store().merge_queue().unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|queue_entry| queue_entry.id == submitted.entry.id)
            .unwrap()
            .status,
        MergeStatus::ExternallyLanded
    );
    let db = rusqlite::Connection::open(db_path).unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM integration_reconciliation_intent",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "recovery must consume the durable intent");
}

#[test]
fn reconcile_refuses_to_discard_unrecorded_integration_commits() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    broker.integration_head().unwrap();
    let old_integration = common::commit_onto_integration(
        tmp.path(),
        &[("src/unrecorded.py", "keep = 1\n")],
        "unrecorded integration work",
    );
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "main"],
    );

    let report = broker
        .reconcile_integration(IntegrationReconcileOptions {
            upstream: "origin/main".into(),
            apply: true,
            resolution_file: None,
            confirm: None,
        })
        .unwrap();
    assert!(!report.safe);
    assert!(!report.applied);
    assert!(
        report.warnings[0].contains("unrecorded work"),
        "{:?}",
        report.warnings
    );
    assert_eq!(resolve(tmp.path(), "aethyme/integration"), old_integration);
}

// ── #42: main-checkout sessions get real (advisory) verification ────────

#[test]
fn main_checkout_submission_selects_gates_from_pre_refresh_base() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();

    // First submission from the main checkout creates the integration
    // branch at the session's own head — no earlier verified state exists,
    // so this one is legitimately vacuous.
    let session = broker.adopt(tmp.path(), Some("main-root work")).unwrap();
    std::fs::write(tmp.path().join("src/a.py"), "a = 2\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "first main change"]);
    let first = broker.submit(session.id).unwrap();
    assert_eq!(first.entry.status, MergeStatus::Verified);

    // Second submission: integration now lags main by exactly this new
    // commit. Pre-#42 the follows-main refresh made base == head and the
    // entry verified with gates:[] — now the gate must actually run.
    std::fs::write(tmp.path().join("src/b.py"), "b = 2\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "second main change"]);
    let second = broker.submit(session.id).unwrap();
    assert_eq!(second.entry.status, MergeStatus::Verified);
    assert!(
        !second.gate_outcomes.is_empty(),
        "main-checkout submission must select gates from the pre-refresh \
         integration head, not verify vacuously (#42)"
    );
    assert_eq!(second.gate_outcomes[0].gate, "marker");
}

#[test]
fn status_advice_warns_about_dirty_worktree_wip() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();

    let wt = agent_worktree(tmp.path(), "dirty");
    let session = broker.adopt(&wt, Some("dirty wip")).unwrap();
    std::fs::write(wt.join("src/a.py"), "a = 99\n").unwrap();

    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.dirty-worktree")
        .expect("dirty worktree should produce status advice");
    assert_eq!(advice.severity, StatusAdviceSeverity::Warning);
    assert_eq!(advice.session_id, Some(session.id));
    assert!(advice.summary.contains("uncommitted change"));
    assert!(
        advice.evidence.iter().any(|line| line.contains("src/a.py")),
        "{advice:?}"
    );
    assert!(
        advice
            .commands
            .iter()
            .any(|command| command.contains("status --short")),
        "{advice:?}"
    );
}

#[test]
fn status_reports_branch_drift_and_dirty_changes_in_an_adopted_primary_checkout() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let session = broker
        .adopt(tmp.path(), Some("adopt primary checkout"))
        .unwrap();

    sh(tmp.path(), &["checkout", "-q", "-b", "other-work"]);
    std::fs::write(tmp.path().join("src/a.py"), "a = 99\n").unwrap();

    let status = broker.status(0).unwrap();
    assert_eq!(status.summary.dirty_sessions, 1, "{status:#?}");
    let drift = status
        .advice
        .iter()
        .find(|advice| advice.id == "session.checkout-drift")
        .expect("branch drift should be surfaced");
    assert_eq!(drift.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(drift.session_id, Some(session.id));
    assert!(drift.summary.contains("\"main\""), "{drift:?}");
    assert!(drift.summary.contains("\"other-work\""), "{drift:?}");
    assert!(
        drift
            .evidence
            .iter()
            .any(|line| line == "recorded branch: main"),
        "{drift:?}"
    );
    assert!(
        status.advice.iter().any(|advice| {
            advice.id == "session.dirty-worktree"
                && advice.session_id == Some(session.id)
                && advice.evidence.iter().any(|line| line.contains("src/a.py"))
        }),
        "{status:#?}"
    );
}

#[test]
fn submit_refuses_an_adopted_checkout_that_switched_branches_before_queueing() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let session = broker
        .adopt(tmp.path(), Some("submit branch guard"))
        .unwrap();

    sh(tmp.path(), &["checkout", "-q", "-b", "other-work"]);
    commit_edit(tmp.path(), "src/a.py", "a = 2\n");

    let error = broker
        .submit(session.id)
        .expect_err("drift must fail closed");
    assert!(
        matches!(
            &error,
            aethyme_broker::BrokerOpError::SessionCheckoutDrift {
                session_id,
                recorded_branch,
                actual_branch,
                ..
            } if *session_id == session.id
                && recorded_branch.as_ref() == "main"
                && actual_branch.as_ref() == "other-work"
        ),
        "{error}"
    );

    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    let queued: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM merge_queue WHERE session_id = ?1",
            [session.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0, "a refused submit leaves no queue row");
}

#[test]
fn submit_accepts_a_session_adopted_on_a_detached_checkout() {
    // Routed reviews adopt a detached checkout, so the session records `HEAD`
    // as its branch. Staying detached is not drift; `refs/heads/HEAD` does
    // not exist and must not be required.
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    sh(tmp.path(), &["checkout", "-q", "--detach"]);
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    let session = broker.adopt(tmp.path(), Some("detached adoption")).unwrap();
    assert_eq!(session.branch, "HEAD");
    commit_edit(tmp.path(), "src/a.py", "a = 2\n");

    let status = broker.status(0).unwrap();
    assert!(
        !status
            .advice
            .iter()
            .any(|advice| advice.id == "session.checkout-drift"),
        "{status:#?}"
    );
    let result = broker.submit(session.id);
    assert!(
        !matches!(
            result,
            Err(aethyme_broker::BrokerOpError::SessionCheckoutDrift { .. })
        ),
        "{result:?}"
    );
}

fn commit_base_gate(root: &Path, gate_toml: &str) {
    std::fs::write(root.join(".aethyme/gates.toml"), gate_toml).unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "base: gate"]);
}

/// A gate the broker's own deadline killed never judged the change, so the
/// submission is deferred rather than rejected -- visible in status and in
/// finish, and never promoted. The same tree timing out again is evidence
/// about the change, so the repeat is a rejection: a change that hangs cannot
/// be deferred forever.
#[test]
fn a_first_broker_timeout_defers_the_submission_and_a_repeat_rejects_it() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    commit_base_gate(
        tmp.path(),
        "[[gate]]\nname = \"slow\"\ncommand = \"sleep 5\"\ntimeout_seconds = 1\ntriggers = [\"**/*.py\"]\n",
    );
    let wt = agent_worktree(tmp.path(), "slow");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 3\n");

    let first = broker.submit(session.id).unwrap();
    assert_eq!(
        first.gate_verification.status,
        SubmissionGateVerificationStatus::Deferred
    );
    assert_eq!(first.entry.status, MergeStatus::Submitted);
    assert!(!first.promoted, "a change no gate judged must not promote");
    assert!(first.gate_outcomes[0].is_host_fault());
    let details: serde_json::Value =
        serde_json::from_str(first.entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["deferred"], serde_json::Value::Bool(true));

    let status = broker.status(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|item| item.id == "session.latest-submit-deferred")
        .expect("a deferred submit must surface in status");
    assert_eq!(advice.severity, StatusAdviceSeverity::Blocked);
    assert_eq!(advice.queue_entry_id, Some(first.entry.id));
    assert!(advice.summary.contains("slow"), "{}", advice.summary);
    assert!(advice.summary.contains("did not complete"));
    assert!(advice.summary.contains("only authorized validation"));
    assert!(advice.summary.contains("do not complete this submission"));
    assert!(
        status
            .advice
            .iter()
            .all(|item| item.id != "session.latest-submit-rejected"),
        "a deferral is not a rejection"
    );
    let finish = broker.finish(session.id).unwrap();
    assert!(
        finish.warnings.iter().any(|w| w.contains("was deferred")),
        "finish must not tell the agent to wait for an entry nothing runs: {:?}",
        finish.warnings
    );

    assert!(
        finish
            .warnings
            .iter()
            .any(|w| w.contains("authorized validation"))
    );
    assert!(
        finish
            .warnings
            .iter()
            .any(|w| w.contains("do not complete"))
    );

    let second = broker.submit(session.id).unwrap();
    assert_eq!(
        second.gate_verification.status,
        SubmissionGateVerificationStatus::Failed,
        "a repeated timeout on the same tree is a verdict"
    );
    assert_eq!(second.entry.status, MergeStatus::Rejected);
    assert!(!second.gate_outcomes[0].is_host_fault());
}

/// The gate log is the change's own output. A failing test that merely says
/// "timed out" is classified like a host problem for advisories, but it is
/// still a rejection: deferring it would ask the agent to resubmit unchanged
/// code forever.
#[test]
fn test_output_that_looks_like_a_host_fault_is_still_a_rejection() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = common::open_fixture_repository_broker(&tmp)
        .unwrap()
        .into_broker();
    commit_base_gate(
        tmp.path(),
        "[[gate]]\nname = \"flaky-net\"\ncommand = \"echo 'request timed out'; exit 1\"\ntriggers = [\"**/*.py\"]\n",
    );
    let wt = agent_worktree(tmp.path(), "net");
    let session = broker.adopt(&wt, None).unwrap();
    commit_edit(&wt, "src/a.py", "a = 3\n");

    let outcome = broker.submit(session.id).unwrap();
    assert_eq!(
        outcome.gate_verification.status,
        SubmissionGateVerificationStatus::Failed
    );
    assert_eq!(outcome.entry.status, MergeStatus::Rejected);
    assert!(!outcome.gate_outcomes[0].is_host_fault());
}
