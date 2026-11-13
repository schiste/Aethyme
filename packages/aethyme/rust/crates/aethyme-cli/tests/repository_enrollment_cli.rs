use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_testkit::{aethyme_bin, tmp_dir};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    run_with_env(repo, args, None)
}

fn command_with_env(repo: &Path, args: &[&str], extra: Option<(&str, &str)>) -> Command {
    let binary = aethyme_bin();
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(&existing_path));
    let mut command = Command::new(&binary);
    command
        .args(args)
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", repo.join("config-home"))
        .env("AETHYME_HOST_STATE_DIR", repo.join(".git/host-state"))
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
    if let Some((key, value)) = extra {
        command.env(key, value);
    }
    command
}

fn run_with_env(repo: &Path, args: &[&str], extra: Option<(&str, &str)>) -> Output {
    command_with_env(repo, args, extra).output().unwrap()
}

fn failure_context(repo: &Path, output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut context = format!("stdout={stdout} stderr={stderr}");
    let operation_id = stderr
        .split_once("ship push operation ")
        .and_then(|(_, remainder)| remainder.split_whitespace().next())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|id| *id > 0);
    let Some(operation_id) = operation_id else {
        return context;
    };

    let operation_id = operation_id.to_string();
    let mut command = command_with_env(
        repo,
        &[
            "broker",
            "advanced",
            "operations",
            "show",
            &operation_id,
            "--json",
        ],
        None,
    );
    match command.output() {
        Ok(shown) => context.push_str(&format!(
            "\noperation {operation_id} show (exit {:?}): stdout={} stderr={}",
            shown.status.code(),
            String::from_utf8_lossy(&shown.stdout),
            String::from_utf8_lossy(&shown.stderr)
        )),
        Err(error) => context.push_str(&format!(
            "\noperation {operation_id} show could not be launched: {error}"
        )),
    }
    context
}

struct Fixture {
    _root: tempfile::TempDir,
    seed: PathBuf,
    clone: PathBuf,
}

fn fixture() -> Fixture {
    let root = tmp_dir();
    let remote = root.path().join("remote.git");
    let seed = root.path().join("seed");
    let clone = root.path().join("clone");
    assert!(
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&remote)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&seed)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(seed.join("README.md"), "fixture\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-qm", "initial"]);
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-qu", "origin", "main"]);
    git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    assert!(
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&clone)
            .status()
            .unwrap()
            .success()
    );
    Fixture {
        _root: root,
        seed,
        clone,
    }
}

fn plan(repo: &Path) -> serde_json::Value {
    let output = run(repo, &["deploy", "plan", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn plan_is_read_only_digest_bound_and_based_on_current_upstream() {
    let fixture = fixture();
    let before_refs = git(&fixture.clone, &["show-ref"]);
    let before_status = git(&fixture.clone, &["status", "--short"]);

    std::fs::write(fixture.seed.join("upstream.txt"), "advanced\n").unwrap();
    git(&fixture.seed, &["add", "upstream.txt"]);
    git(&fixture.seed, &["commit", "-qm", "advance upstream"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);
    let upstream = git(&fixture.seed, &["rev-parse", "HEAD"]);

    let report = plan(&fixture.clone);
    let repeated = plan(&fixture.clone);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["remote_base"]["exact_sha"], upstream);
    assert_eq!(report["local_behind_upstream_commits"], 1);
    assert_eq!(report["generated_tree"]["from_schema"], 0);
    assert_eq!(report["safe"], true);
    assert_eq!(report["hook_manager"]["kind"], "absent");
    assert_eq!(report["plan_digest"].as_str().unwrap().len(), 64);
    assert_eq!(report["plan_digest"], repeated["plan_digest"]);
    assert!(
        report["planned_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "AGENTS.md")
    );
    assert_eq!(git(&fixture.clone, &["show-ref"]), before_refs);
    assert_eq!(git(&fixture.clone, &["status", "--short"]), before_status);
    assert!(!fixture.clone.join(".aethyme").exists());
}

/// Leave a local `aethyme/integration` holding a commit neither the local nor
/// the upstream default branch has, as local promotion used to.
fn leave_distinct_integration_work(repo: &Path) -> String {
    git(repo, &["checkout", "-qb", "leftover"]);
    std::fs::write(repo.join("leftover.txt"), "promoted once\n").unwrap();
    git(repo, &["add", "leftover.txt"]);
    git(repo, &["commit", "-qm", "leftover promotion"]);
    let leftover = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "-q", "main"]);
    git(repo, &["branch", "-qD", "leftover"]);
    git(repo, &["branch", "aethyme/integration", &leftover]);
    leftover
}

/// With a fetched default branch, sessions are cut from and verified against
/// it and enrollment never touches integration, so leftover integration work
/// is not read: it neither blocks the plan nor gets a preservation ref.
#[test]
fn plan_ignores_integration_when_a_default_branch_is_fetched() {
    let fixture = fixture();
    leave_distinct_integration_work(&fixture.clone);

    let report = plan(&fixture.clone);
    assert_eq!(report["safe"], true, "{report:#}");
    assert_eq!(report["integration_can_rebase_to_upstream"], true);
    assert!(report["integration"]["exact_sha"].is_null(), "{report:#}");
    let encoded = serde_json::to_string(&report["preservation_refs"]).unwrap();
    assert!(!encoded.contains("/integration"), "{encoded}");
    assert!(
        report["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|blocker| !blocker.as_str().unwrap().contains("integration")),
        "{report:#}"
    );
}

/// Without a fetched default branch `submit` still falls back to
/// integration, so the plan keeps reading, preserving and checking it.
#[test]
fn plan_still_checks_integration_without_a_fetched_default_branch() {
    let fixture = fixture();
    let leftover = leave_distinct_integration_work(&fixture.clone);
    git(
        &fixture.clone,
        &["remote", "set-head", "origin", "--delete"],
    );
    git(&fixture.clone, &["branch", "--unset-upstream", "main"]);
    git(
        &fixture.clone,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );

    let report = plan(&fixture.clone);
    assert_eq!(report["safe"], false, "{report:#}");
    assert_eq!(report["integration_can_rebase_to_upstream"], false);
    assert_eq!(report["integration"]["exact_sha"], leftover);
    assert!(
        report["preservation_refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|preserved| preserved["target_sha"] == leftover.as_str()),
        "{report:#}"
    );
}

#[test]
fn plan_distinguishes_disjoint_overlap_and_foreign_hook_ownership() {
    let fixture = fixture();
    std::fs::write(fixture.clone.join("notes.local"), "keep me\n").unwrap();
    let disjoint = plan(&fixture.clone);
    assert_eq!(disjoint["safe"], true);
    assert_eq!(disjoint["disjoint_dirty_paths"][0], "notes.local");

    std::fs::write(fixture.clone.join("AGENTS.md"), "custom\n").unwrap();
    let overlap = plan(&fixture.clone);
    assert_eq!(overlap["safe"], false);
    assert_eq!(overlap["overlapping_dirty_paths"][0], "AGENTS.md");

    std::fs::remove_file(fixture.clone.join("AGENTS.md")).unwrap();
    git(&fixture.clone, &["config", "core.hooksPath", ".husky"]);
    let hooks = plan(&fixture.clone);
    assert_eq!(hooks["safe"], false);
    assert_eq!(hooks["hook_manager"]["kind"], "foreign");
    let encoded = serde_json::to_string(&hooks).unwrap();
    assert!(!encoded.contains(".husky"));
    assert!(encoded.contains("will not overwrite"));
}

#[test]
fn confirmed_execute_preserves_publishes_verifies_syncs_and_resumes() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();

    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        failure_context(&fixture.clone, &executed)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["completed"], true);
    assert_eq!(
        report["local_main_synchronized"],
        true,
        "report: {report}; status: {}",
        git(&fixture.clone, &["status", "--short"])
    );
    assert_eq!(
        report["publication_sha"], report["verified_remote_sha"],
        "publication is verified from remote evidence"
    );

    let remote_main = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"])
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(git(&fixture.clone, &["rev-parse", "main"]), remote_main);
    assert!(
        git(
            &fixture.clone,
            &["show", &format!("{remote_main}:.aethyme/repository.json")]
        )
        .contains("schema_version")
    );
    let preserved = report["preservation_refs"][0]["ref_name"].as_str().unwrap();
    assert_eq!(git(&fixture.clone, &["rev-parse", preserved]), original);

    let resumed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(resumed.status.success());
    let resumed: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["publication_sha"], report["publication_sha"]);
    assert_eq!(resumed["queue_entry_id"], report["queue_entry_id"]);
}

#[test]
fn execute_publishes_but_does_not_sync_a_dirty_primary_checkout() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.clone.join("notes.local"), "uncommitted\n").unwrap();
    let planned = plan(&fixture.clone);
    assert_eq!(planned["safe"], true);
    let digest = planned["plan_digest"].as_str().unwrap();

    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        failure_context(&fixture.clone, &executed)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["local_main_synchronized"], false);
    assert_eq!(git(&fixture.clone, &["rev-parse", "main"]), original);
    assert_eq!(
        std::fs::read_to_string(fixture.clone.join("notes.local")).unwrap(),
        "uncommitted\n"
    );
    assert_ne!(report["verified_remote_sha"], original);
}

#[test]
fn stale_main_enrolls_from_upstream_and_preexisting_sibling_detects_activation() {
    let fixture = fixture();
    let original = git(&fixture.clone, &["rev-parse", "HEAD"]);
    let sibling = fixture._root.path().join("pre-enrollment-sibling");
    git(
        &fixture.clone,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            sibling.to_str().unwrap(),
            &original,
        ],
    );
    std::fs::write(fixture.seed.join("upstream.txt"), "preserve upstream\n").unwrap();
    git(&fixture.seed, &["add", "upstream.txt"]);
    git(&fixture.seed, &["commit", "-qm", "advance upstream"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);

    let planned = plan(&fixture.clone);
    assert_eq!(planned["local_behind_upstream_commits"], 1);
    let digest = planned["plan_digest"].as_str().unwrap();
    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        failure_context(&fixture.clone, &executed)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["local_main_synchronized"], true);
    let local_main = git(&fixture.clone, &["rev-parse", "main"]);
    assert_eq!(
        git(
            &fixture.clone,
            &["show", &format!("{local_main}:upstream.txt")]
        ),
        "preserve upstream"
    );
    let pre_push = fixture.clone.join(".git/hooks/pre-push");
    assert!(
        pre_push.is_file(),
        "{} was not installed",
        pre_push.display()
    );
    let hook = std::fs::read_to_string(&pre_push).unwrap();
    assert!(hook.contains("broker hooks pre-push"), "{hook}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&pre_push).unwrap().permissions().mode() & 0o111,
            0,
            "{} is not executable",
            pre_push.display()
        );
    }
    assert_eq!(
        PathBuf::from(git(
            &sibling,
            &["rev-parse", "--git-path", "hooks/pre-push"]
        ))
        .canonicalize()
        .unwrap(),
        pre_push.canonicalize().unwrap()
    );
    assert_eq!(
        Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(&sibling)
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    git(&sibling, &["fetch", "-q", "origin", "main"]);
    git(&sibling, &["checkout", "-q", "--detach", "origin/main"]);

    let commit = Command::new("git")
        .args(["commit", "--allow-empty", "-m", "unmanaged sibling commit"])
        .current_dir(&sibling)
        .env("GIT_AUTHOR_NAME", "Aethyme Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Aethyme Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let push = Command::new("git")
        .args(["push", "origin", "HEAD:main"])
        .current_dir(&sibling)
        .output()
        .unwrap();
    assert!(!push.status.success());
    let refusal = String::from_utf8_lossy(&push.stderr);
    assert!(
        refusal.contains("Aethyme") && refusal.contains("protected"),
        "{refusal}"
    );
}

#[test]
fn remote_movement_after_review_refuses_before_preservation() {
    let fixture = fixture();
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();
    std::fs::write(fixture.seed.join("moved.txt"), "moved\n").unwrap();
    git(&fixture.seed, &["add", "moved.txt"]);
    git(&fixture.seed, &["commit", "-qm", "move remote"]);
    git(&fixture.seed, &["push", "-q", "origin", "main"]);

    let refused = run(&fixture.clone, &["deploy", "execute", "--confirm", digest]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("changed after review"));
    assert!(
        git(
            &fixture.clone,
            &["for-each-ref", "refs/heads/aethyme/preservation"]
        )
        .is_empty()
    );
}

// One test per phase boundary: each builds its own fixture, so they run in
// parallel instead of one test holding four sequential deploys.
#[test]
fn persisted_phase_boundary_after_preserved_resumes_without_duplicate_publication() {
    resume_after_persisted_phase_without_duplicate_publication("preserved");
}

#[test]
fn persisted_phase_boundary_after_outputs_applied_resumes_without_duplicate_publication() {
    resume_after_persisted_phase_without_duplicate_publication("outputs_applied");
}

// Enrollment no longer promotes: the boundary between gating the enrollment
// commit and publishing it is now persisted as `verified` (formerly
// `promoted`). An old `promoted` journal is covered separately below.
#[test]
fn persisted_phase_boundary_after_verified_resumes_without_duplicate_publication() {
    resume_after_persisted_phase_without_duplicate_publication("verified");
}

#[test]
fn persisted_phase_boundary_after_published_resumes_without_duplicate_publication() {
    resume_after_persisted_phase_without_duplicate_publication("published");
}

fn resume_after_persisted_phase_without_duplicate_publication(phase: &str) {
    let fixture = fixture();
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();
    let interrupted = run_with_env(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest],
        Some(("AETHYME_TEST_ENROLLMENT_STOP_AFTER_PHASE", phase)),
    );
    assert!(!interrupted.status.success(), "phase {phase}");
    assert!(
        String::from_utf8_lossy(&interrupted.stderr).contains("test interruption"),
        "phase {phase}: {}",
        String::from_utf8_lossy(&interrupted.stderr)
    );

    let resumed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        resumed.status.success(),
        "phase {phase}: {}",
        failure_context(&fixture.clone, &resumed)
    );
    let report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(report["completed"], true, "phase {phase}");
    assert_eq!(
        report["publication_sha"], report["verified_remote_sha"],
        "phase {phase}"
    );
}

fn integration_branch(repo: &Path) -> String {
    git(repo, &["for-each-ref", "refs/heads/aethyme/integration"])
}

#[test]
fn first_enrollment_publishes_without_creating_the_integration_branch() {
    let fixture = fixture();
    assert!(integration_branch(&fixture.clone).is_empty());
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();

    let executed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        executed.status.success(),
        "{}",
        failure_context(&fixture.clone, &executed)
    );
    let report: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(report["completed"], true);
    // The enrollment commit itself is what reaches the remote.
    assert_eq!(report["publication_sha"], report["enrollment_commit"]);
    assert_eq!(report["verified_remote_sha"], report["enrollment_commit"]);
    let remote_main = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"]);
    assert!(remote_main.starts_with(report["enrollment_commit"].as_str().unwrap()));
    assert!(
        integration_branch(&fixture.clone).is_empty(),
        "enrollment must neither create nor move the integration branch"
    );
}

/// An older binary interrupted after its `promoted` phase left a journal
/// naming a promotion SHA and a queue entry. Resuming must not trust either:
/// here both are unusable, so the commit is verified again and the enrollment
/// commit, not the recorded SHA, is published exactly once.
#[test]
fn old_promoted_phase_journal_reverifies_and_publishes_the_enrollment_commit() {
    let fixture = fixture();
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap();
    let interrupted = run_with_env(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest],
        Some(("AETHYME_TEST_ENROLLMENT_STOP_AFTER_PHASE", "verified")),
    );
    assert!(!interrupted.status.success());
    let journal_path = fixture
        .clone
        .join(".git/aethyme-broker/enrollment-publication.json");
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
    let enrollment_commit = journal["enrollment_commit"].as_str().unwrap().to_string();
    let remote_before = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"]);
    assert!(!remote_before.starts_with(&enrollment_commit));
    journal["phase"] = "promoted".into();
    journal["queue_entry_id"] = 999_999.into();
    journal["publication_sha"] = "f".repeat(40).into();
    std::fs::write(&journal_path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();

    let resumed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(
        resumed.status.success(),
        "{}",
        failure_context(&fixture.clone, &resumed)
    );
    let report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(report["completed"], true);
    assert_eq!(report["publication_sha"], enrollment_commit.as_str());
    assert_eq!(report["verified_remote_sha"], enrollment_commit.as_str());
    assert_ne!(report["queue_entry_id"], 999_999);
    let remote_after = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"]);
    assert!(remote_after.starts_with(&enrollment_commit));
    assert!(integration_branch(&fixture.clone).is_empty());

    // Rerunning a completed enrollment reports it again without publishing.
    let repeated = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", digest, "--json"],
    );
    assert!(repeated.status.success());
    let repeated: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated["publication_sha"], enrollment_commit.as_str());
}

/// Interrupt a confirmed enrollment after `phase`, rewrite its journal with
/// `edit` as an older binary would have left it, and resume it.
fn resume_rewritten_journal(
    stop_after: &str,
    edit: impl FnOnce(&Fixture, &mut serde_json::Value),
) -> (Fixture, serde_json::Value, String) {
    let fixture = fixture();
    let planned = plan(&fixture.clone);
    let digest = planned["plan_digest"].as_str().unwrap().to_string();
    let interrupted = run_with_env(
        &fixture.clone,
        &["deploy", "execute", "--confirm", &digest],
        Some(("AETHYME_TEST_ENROLLMENT_STOP_AFTER_PHASE", stop_after)),
    );
    assert!(!interrupted.status.success());
    let journal_path = fixture
        .clone
        .join(".git/aethyme-broker/enrollment-publication.json");
    let mut journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
    edit(&fixture, &mut journal);
    std::fs::write(&journal_path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
    let resumed = run(
        &fixture.clone,
        &["deploy", "execute", "--confirm", &digest, "--json"],
    );
    assert!(
        resumed.status.success(),
        "{}",
        failure_context(&fixture.clone, &resumed)
    );
    let report: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(report["completed"], true);
    let remote = git(&fixture.seed, &["ls-remote", "origin", "refs/heads/main"]);
    (fixture, report, remote)
}

#[test]
fn old_promoted_phase_journal_with_a_vouching_entry_publishes_the_enrollment_commit() {
    let (fixture, report, remote) = resume_rewritten_journal("verified", |_, journal| {
        journal["phase"] = "promoted".into();
    });
    assert_eq!(report["publication_sha"], report["enrollment_commit"]);
    assert!(remote.starts_with(report["enrollment_commit"].as_str().unwrap()));
    assert!(integration_branch(&fixture.clone).is_empty());
}

#[test]
fn old_base_prepared_phase_journal_continues_without_integration() {
    let (fixture, report, remote) =
        resume_rewritten_journal("remote_materialized", |_, journal| {
            journal["phase"] = "base_prepared".into();
        });
    assert_eq!(report["publication_sha"], report["enrollment_commit"]);
    assert!(remote.starts_with(report["enrollment_commit"].as_str().unwrap()));
    assert!(integration_branch(&fixture.clone).is_empty());
}

/// An older binary pushed its merge promotion of the enrollment commit and
/// crashed before recording it. The remote then names that promotion, which
/// is the reviewed base plus exactly the verified tree, so the resume accepts
/// it instead of refusing forever, and pushes nothing.
#[test]
fn old_promoted_journal_whose_promotion_reached_the_remote_completes() {
    let mut promotion = String::new();
    let (_fixture, report, remote) = resume_rewritten_journal("verified", |fixture, journal| {
        let commit = journal["enrollment_commit"].as_str().unwrap().to_string();
        let base = journal["remote_base"]["exact_sha"]
            .as_str()
            .unwrap()
            .to_string();
        let tree = git(
            &fixture.clone,
            &["rev-parse", &format!("{commit}^{{tree}}")],
        );
        promotion = git(
            &fixture.clone,
            &[
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-p",
                &commit,
                "-m",
                "promote enrollment",
            ],
        );
        git(
            &fixture.clone,
            &[
                "push",
                "-q",
                "--no-verify",
                "origin",
                &format!("{promotion}:refs/heads/main"),
            ],
        );
        journal["phase"] = "promoted".into();
        journal["publication_sha"] = promotion.clone().into();
    });
    assert_eq!(report["publication_sha"], promotion.as_str());
    assert_eq!(report["verified_remote_sha"], promotion.as_str());
    assert!(remote.starts_with(&promotion));
}
