use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{
    AdoptMode, AdoptOptions, Broker, BrokerStore, GitRepo, NewSession, RepositoryContract,
    SessionOrigin, SessionStatus, hooks,
};
use aethyme_testkit::{aethyme_bin, tmp_dir};
use serde_json::Value;

fn repository(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    let initialized = Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(initialized.status.success());
    commit_all(&repo, "initial");
    repo
}

fn command(repo: &Path) -> Command {
    let mut command = Command::new(aethyme_bin());
    command
        .current_dir(repo)
        .env_remove("AETHYME_ROOT")
        .env("XDG_CONFIG_HOME", repo.join("empty-config"));
    command
}

fn commit_all(repo: &Path, message: &str) {
    let added = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(added.status.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Aethyme Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
}

fn run(repo: &Path, args: &[&str]) -> Output {
    command(repo).args(args).output().unwrap()
}

fn old_canonical_deployment(repo: &Path) {
    let deployed = run(repo, &["deploy", "--repo", repo.to_str().unwrap()]);
    assert!(
        deployed.status.success(),
        "{}",
        String::from_utf8_lossy(&deployed.stderr)
    );
    fs::remove_file(repo.join(".aethyme/repository.json")).unwrap();
    commit_all(repo, "deploy old repository contract");
}

fn adopt_legacy_session(repo: &Path) -> i64 {
    let mut broker = Broker::open(repo).unwrap();
    broker
        .adopt_with_options(
            repo,
            Some("legacy session"),
            AdoptOptions {
                mode: AdoptMode::New,
                sync_integration: false,
            },
        )
        .unwrap()
        .session
        .id
}

fn register_version_pinned_session(repo: &Path, version: &str) -> i64 {
    let checkout = GitRepo::discover(repo).unwrap();
    let head = git_stdout(repo, &["rev-parse", "HEAD"]);
    BrokerStore::open_in_repo(repo)
        .unwrap()
        .register_session(&NewSession {
            worktree_path: checkout.root().to_string_lossy().into_owned(),
            branch: git_stdout(repo, &["branch", "--show-current"]),
            origin: SessionOrigin::Adopted,
            task: Some("session accepted before Homebrew update".into()),
            diff_base: Some(head.clone()),
            adoption_base: Some(head),
            repository_contract: Some(RepositoryContract {
                repository_schema: None,
                deployment_state_digest: "a".repeat(64),
                aethyme_version: version.into(),
                gate_definition_digest: None,
                backfilled: false,
            }),
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap()
        .id
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn canonical_upgrade_is_read_only_then_digest_bound_and_verifiable() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);

    let before = run(&repo, &["upgrade", "plan", "--json"]);
    let plan = json(&before);
    assert_eq!(plan["from_schema"], 0);
    assert_eq!(plan["to_schema"], 1);
    assert_eq!(plan["safe"], true);
    assert_eq!(plan["applied"], false);
    assert_eq!(
        plan["migrations"],
        serde_json::json!(["repository-deployment-v1"])
    );
    assert_eq!(git_status(&repo), "");
    assert!(!String::from_utf8_lossy(&before.stdout).contains(repo.to_string_lossy().as_ref()));

    let rejected = run(
        &repo,
        &["upgrade", "apply", "--confirm", &"0".repeat(64), "--json"],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("changed after review"));
    assert_eq!(git_status(&repo), "");

    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(&repo, &["upgrade", "apply", "--confirm", digest, "--json"]);
    let report = json(&applied);
    assert_eq!(report["applied"], true);
    assert_eq!(report["from_schema"], 0);
    assert_eq!(
        report["migrations"],
        serde_json::json!(["repository-deployment-v1"])
    );
    assert_eq!(report["plan_digest"], digest);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(repo.join(".aethyme/repository.json")).unwrap())
            .unwrap()["schema_version"],
        1
    );

    let verified = run(
        &repo,
        &["deploy", "verify", "--repo", repo.to_str().unwrap()],
    );
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn dirty_repository_blocks_upgrade_without_mutation() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    fs::write(repo.join("README.md"), "locally edited\n").unwrap();

    let plan = json(&run(&repo, &["upgrade", "plan", "--json"]));
    assert_eq!(plan["safe"], false);
    assert!(
        plan["blockers"][0]
            .as_str()
            .unwrap()
            .contains("worktree is dirty")
    );
    let blocker = plan["blockers"][0].as_str().unwrap();
    assert!(blocker.contains("managed Aethyme pre-commit lane"));
    assert!(blocker.contains("broker finish --session <id>"));
    assert!(!blocker.contains("stash"));

    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(&repo, &["upgrade", "apply", "--confirm", digest]);
    assert!(!applied.status.success());
    assert!(!repo.join(".aethyme/repository.json").exists());
}

#[cfg(unix)]
#[test]
fn homebrew_upgrade_mid_session_preserves_commit_and_recovery_lanes() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    let session = register_version_pinned_session(&repo, "0.2.1");
    let session_arg = session.to_string();

    // 09:51 — the previously installed binary and managed hook accept work.
    let installed_binary = temp.path().join("homebrew/bin/aethyme");
    fs::create_dir_all(installed_binary.parent().unwrap()).unwrap();
    write_executable(&installed_binary, "#!/bin/sh\nexit 0\n");
    hooks::install(&GitRepo::discover(&repo).unwrap(), &installed_binary).unwrap();
    fs::write(repo.join("before-update.txt"), "accepted before update\n").unwrap();
    git(&repo, &["add", "before-update.txt"]);
    assert!(
        git_commit(&repo, "commit before Homebrew update")
            .status
            .success()
    );

    // 09:58 — Homebrew replaces the executable at the same path with the
    // schema-1 binary while the accepted schema-0 session remains active.
    write_executable(
        &installed_binary,
        &format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", aethyme_bin().display()),
    );
    fs::write(
        repo.join("mid-session-wip.txt"),
        "must remain committable\n",
    )
    .unwrap();
    git(&repo, &["add", "mid-session-wip.txt"]);

    let upgrade_plan = json(&run(&repo, &["upgrade", "plan", "--json"]));
    let dirty_blocker = upgrade_plan["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|blocker| blocker.contains("worktree is dirty"))
        .unwrap();
    assert!(dirty_blocker.contains("managed Aethyme pre-commit lane"));
    assert!(!dirty_blocker.contains("stash"));

    // 10:11 — diagnostic and coordinated recovery surfaces remain reachable
    // while the new binary sees the old repository deployment.
    for args in [
        &["broker", "status", "--json"][..],
        &["broker", "leases", "--json"][..],
        &["broker", "operations", "--json"][..],
        &[
            "broker",
            "git",
            "--session",
            &session_arg,
            "--",
            "status",
            "--short",
        ][..],
    ] {
        let output = run(&repo, args);
        assert!(
            output.status.success(),
            "recovery lane {args:?} was frozen: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let report_filing = run(
        &repo,
        &[
            "broker",
            "report",
            "file",
            "missing.issue.md",
            "--repo",
            "owner/repo",
            "--confirm",
            &"a".repeat(64),
        ],
    );
    assert!(!report_filing.status.success());
    assert!(!String::from_utf8_lossy(&report_filing.stderr).contains("embedded upgrade"));

    // No --no-verify escape hatch: the updated binary honors the pinned
    // session contract through the ordinary managed pre-commit lane.
    let committed = git_commit(&repo, "commit after Homebrew update");
    assert!(
        committed.status.success(),
        "updated binary stranded the session: {}",
        String::from_utf8_lossy(&committed.stderr)
    );

    for args in [
        &["broker", "start", "--task", "must stay blocked"][..],
        &["broker", "ship", "execute"][..],
    ] {
        let blocked = run(&repo, args);
        assert!(
            !blocked.status.success(),
            "incompatible mutation ran: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&blocked.stderr);
        assert!(stderr.contains("broker command refused"), "{stderr}");
        assert!(stderr.contains("managed pre-commit lane"), "{stderr}");
        assert!(!stderr.contains("stash"), "{stderr}");
    }

    let finish = json(&run(
        &repo,
        &["broker", "finish", "--session", &session_arg, "--json"],
    ));
    assert_eq!(finish["status"], "blocked");
    assert!(
        finish["recommended_next_action"]
            .as_str()
            .unwrap()
            .contains("broker submit")
    );
}

#[test]
fn canonical_local_only_refusal_remains_exact() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);

    let refused = run(&repo, &["upgrade", "plan", "--local-only"]);
    assert!(!refused.status.success());
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "aethyme upgrade: repository is enrolled as Canonical, not LocalOnly\n"
    );
}

#[test]
fn newer_repository_names_coordinated_operation_refusals() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    fs::write(
        repo.join(".aethyme/repository.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "applied_migrations": ["repository-deployment-v1"]
        }))
        .unwrap(),
    )
    .unwrap();

    let refused = run(
        &repo,
        &[
            "broker",
            "gh",
            "--session",
            "1",
            "--repo",
            "owner/repo",
            "--",
            "issue",
            "list",
        ],
    );
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("coordinated operation refused"), "{stderr}");
    assert!(
        stderr.contains("update Aethyme before retrying the coordinated operation"),
        "{stderr}"
    );
}

#[test]
fn obsolete_repository_status_is_a_genuinely_non_mutating_snapshot() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    let session = adopt_legacy_session(&repo);
    fs::write(repo.join("legacy-wip.txt"), "not leased by a snapshot\n").unwrap();

    let before_store = BrokerStore::open_in_repo(&repo).unwrap();
    let before_events = before_store.events_after(0, 10_000).unwrap();
    let before_leases = before_store.active_leases().unwrap();
    drop(before_store);
    let metrics_path = repo.join(".aethyme/logs/command-metrics.jsonl");
    let before_metrics = fs::read(&metrics_path).ok();

    let status = run(&repo, &["broker", "status", "--json"]);
    let report = json(&status);
    assert!(report["agents"].as_array().unwrap().iter().any(|agent| {
        agent["id"] == session && agent["repository_contract"]["repository_schema"].is_null()
    }));

    let after_store = BrokerStore::open_in_repo(&repo).unwrap();
    assert_eq!(
        serde_json::to_value(after_store.events_after(0, 10_000).unwrap()).unwrap(),
        serde_json::to_value(before_events).unwrap()
    );
    assert_eq!(
        serde_json::to_value(after_store.active_leases().unwrap()).unwrap(),
        serde_json::to_value(before_leases).unwrap()
    );
    assert_eq!(fs::read(&metrics_path).ok(), before_metrics);
}

#[test]
fn older_repository_allows_diagnostics_recovery_and_only_pinned_continuation() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    let session = adopt_legacy_session(&repo);

    for args in [
        &["broker", "status", "--json"][..],
        &["broker", "integration", "status", "--json"][..],
        &["broker", "agents", "--json"][..],
        &["broker", "leases", "--json"][..],
        &["broker", "operations", "--json"][..],
        &["broker", "events", "--json"][..],
        &["broker", "report", "list", "--json"][..],
        &[
            "broker",
            "report",
            "capture",
            "--kind",
            "bug",
            "--title",
            "legacy diagnostics",
            "--stdout",
        ][..],
    ] {
        let output = run(&repo, args);
        assert!(
            output.status.success(),
            "diagnostic command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let session_arg = session.to_string();
    let continued = run(
        &repo,
        &["broker", "exec", "--session", &session_arg, "--", "true"],
    );
    assert!(
        continued.status.success(),
        "{}",
        String::from_utf8_lossy(&continued.stderr)
    );
    let coordinated_git = run(
        &repo,
        &[
            "broker",
            "git",
            "--session",
            &session_arg,
            "--",
            "status",
            "--short",
        ],
    );
    assert!(
        coordinated_git.status.success(),
        "{}",
        String::from_utf8_lossy(&coordinated_git.stderr)
    );

    let finished = run(
        &repo,
        &["broker", "finish", "--session", &session_arg, "--json"],
    );
    assert!(
        finished.status.success(),
        "{}",
        String::from_utf8_lossy(&finished.stderr)
    );
    let handoff = run(
        &repo,
        &["broker", "handoff", "--session", &session_arg, "--json"],
    );
    assert!(
        handoff.status.success(),
        "{}",
        String::from_utf8_lossy(&handoff.stderr)
    );

    for args in [
        &["broker", "submit", "--session", "9999"][..],
        &["broker", "start", "--task", "work"][..],
        &["broker", "adopt", "--reuse", "--task", "work"][..],
        &["broker", "ship", "execute"][..],
    ] {
        let output = run(&repo, args);
        assert!(!output.status.success(), "broker command ran: {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("repository deployment requires an embedded upgrade"),
            "unexpected refusal for {args:?}: {stderr}"
        );
    }

    let upgrade = run(&repo, &["upgrade", "plan", "--json"]);
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
}

#[test]
fn managed_pre_commit_allows_an_accepted_session_pinned_to_the_old_contract() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    let session = adopt_legacy_session(&repo);
    let stored = BrokerStore::open_in_repo(&repo)
        .unwrap()
        .session(session)
        .unwrap();
    assert_eq!(
        stored
            .repository_contract
            .as_ref()
            .and_then(|contract| contract.repository_schema),
        None,
        "the accepted session remains pinned to schema 0"
    );

    let checkout = GitRepo::discover(&repo).unwrap();
    hooks::install(&checkout, &aethyme_bin()).unwrap();
    fs::write(repo.join("accepted-wip.txt"), "preserved across update\n").unwrap();
    git(&repo, &["add", "accepted-wip.txt"]);

    let committed = git_commit(&repo, "commit accepted legacy work");
    assert!(
        committed.status.success(),
        "accepted pinned session was stranded:\n{}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert_eq!(git_status(&repo), "");
}

#[test]
fn managed_pre_commit_refuses_without_an_eligible_session_and_preserves_work() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    let session = adopt_legacy_session(&repo);
    let mut store = BrokerStore::open_in_repo(&repo).unwrap();
    store
        .set_session_status(session, SessionStatus::Exited, None)
        .unwrap();

    let checkout = GitRepo::discover(&repo).unwrap();
    hooks::install(&checkout, &aethyme_bin()).unwrap();
    fs::write(repo.join("retained-wip.txt"), "must remain staged\n").unwrap();
    git(&repo, &["add", "retained-wip.txt"]);
    let head_before = git_stdout(&repo, &["rev-parse", "HEAD"]);

    let refused = git_commit(&repo, "must be refused");
    assert!(!refused.status.success());
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "git commit refused by Aethyme pre-commit:\n\
         repository deployment schema 0 must be upgraded to schema 1.\n\
         Your changes remain in the worktree.\n"
    );
    assert_eq!(git_stdout(&repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        fs::read_to_string(repo.join("retained-wip.txt")).unwrap(),
        "must remain staged\n"
    );
    assert!(git_status(&repo).contains("A  retained-wip.txt"));
}

#[test]
fn repository_compatibility_states_preserve_refusal_remediation() {
    let cases = [
        (
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "applied_migrations": ["repository-deployment-v1"]
            }))
            .unwrap(),
            "newer than this binary supports",
            false,
        ),
        (
            b"{not-json\n".to_vec(),
            "deployment marker is invalid",
            false,
        ),
        (
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 0,
                "applied_migrations": ["repository-deployment-v1:in-progress"]
            }))
            .unwrap(),
            "repository deployment requires an embedded upgrade",
            true,
        ),
    ];

    for (marker, expected, recovery_allowed) in cases {
        let temp = tmp_dir();
        let repo = repository(temp.path());
        old_canonical_deployment(&repo);
        fs::write(repo.join(".aethyme/repository.json"), marker).unwrap();

        let diagnostic = run(&repo, &["broker", "status", "--json"]);
        assert!(
            diagnostic.status.success(),
            "read-only diagnostics must survive {expected}: {}",
            String::from_utf8_lossy(&diagnostic.stderr)
        );

        let mutation = run(&repo, &["broker", "start", "--task", "blocked"]);
        assert!(!mutation.status.success());
        let stderr = String::from_utf8_lossy(&mutation.stderr);
        assert!(stderr.contains(expected), "expected {expected:?}: {stderr}");

        let recovery = run(&repo, &["broker", "close", "--session", "9999"]);
        let recovery_stderr = String::from_utf8_lossy(&recovery.stderr);
        assert!(!recovery.status.success());
        assert_eq!(
            recovery_stderr.contains(expected),
            !recovery_allowed,
            "unexpected recovery routing for {expected}: {recovery_stderr}"
        );

        let upgrade = run(&repo, &["upgrade", "plan", "--json"]);
        assert!(
            upgrade.status.success(),
            "upgrade capability was blocked before dispatch: {}",
            String::from_utf8_lossy(&upgrade.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn plan_refuses_managed_paths_that_escape_through_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    fs::remove_dir_all(repo.join(".codex")).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, repo.join(".codex")).unwrap();

    let plan = json(&run(&repo, &["upgrade", "plan", "--json"]));
    assert_eq!(plan["safe"], false);
    assert!(plan["blockers"].as_array().unwrap().iter().any(|blocker| {
        blocker
            .as_str()
            .unwrap()
            .contains("never write through symlinks")
    }));
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn local_only_upgrade_stays_untracked_and_does_not_follow_the_clone() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let bridge = run(
        &repo,
        &["deploy", "bridge", "--repo", repo.to_str().unwrap()],
    );
    assert!(bridge.status.success());
    commit_all(&repo, "add local activation bridge");
    let deployed = run(
        &repo,
        &["deploy", "--local-only", "--repo", repo.to_str().unwrap()],
    );
    assert!(deployed.status.success());
    fs::remove_file(repo.join(".aethyme/local/repository.json")).unwrap();
    assert_eq!(git_status(&repo), "");

    let plan = json(&run(&repo, &["upgrade", "plan", "--local-only", "--json"]));
    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(
        &repo,
        &[
            "upgrade",
            "apply",
            "--local-only",
            "--confirm",
            digest,
            "--json",
        ],
    );
    assert_eq!(json(&applied)["applied"], true);
    assert!(repo.join(".aethyme/local/repository.json").is_file());
    assert_eq!(git_status(&repo), "");
}

fn git_status(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(repo, args).stdout)
        .trim()
        .to_string()
}

fn git_commit(repo: &Path, message: &str) -> Output {
    Command::new("git")
        .args([
            "-c",
            "user.name=Aethyme Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            message,
        ])
        .current_dir(repo)
        .output()
        .unwrap()
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}
