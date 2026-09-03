use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use aethyme_broker::GitRepo;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn run_with_stdin(repo: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join("tracked.txt"), "first\n").unwrap();
    std::fs::write(
        tmp.path().join(".gitignore"),
        "gate-runs.txt\nsubmit-runs.txt\n.aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"provenance\"\ncommand = \"echo gate-output; echo run >> gate-runs.txt\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[graph]\nauthority='disabled'\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "fixture"]);
    tmp
}

#[test]
fn exact_gate_scope_manifest_is_redacted_deterministic_and_selector_complete() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::create_dir_all(tmp.path().join("backend")).unwrap();
    std::fs::write(tmp.path().join("backend/old.rs"), "old\n").unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        r#"
[[gate]]
name = "backend"
command = "SECRET_TOKEN=hidden /private/operator/backend-test"
cost = 2
triggers = ["backend/**"]

[[gate]]
name = "frontend"
command = "frontend-test"
cost = 1
triggers = ["frontend/**"]

[[gate]]
name = "always"
command = "true"
cost = 0
"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[graph]\nauthority='committed_fragments'\nrepository='fixture'\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "base"]);
    let base = git_output(tmp.path(), &["rev-parse", "HEAD"]);

    std::fs::create_dir_all(tmp.path().join("frontend")).unwrap();
    git(tmp.path(), &["mv", "backend/old.rs", "frontend/moved.rs"]);
    std::fs::create_dir_all(tmp.path().join("assets")).unwrap();
    std::fs::write(tmp.path().join("assets/blob.bin"), [0_u8, 159, 146, 150]).unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "rename and binary"]);
    let head = git_output(tmp.path(), &["rev-parse", "HEAD"]);

    // Incidental checkout policy is not an input when an exact head is named.
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname='dirty-only'\ncommand='DIRTY_SECRET'\n",
    )
    .unwrap();

    let manifest = stdout(run(
        tmp.path(),
        &["gates", "manifest", "--head", &head, "--json"],
    ));
    let repeated = stdout(run(
        tmp.path(),
        &["gates", "manifest", "--head", &head, "--json"],
    ));
    assert_eq!(manifest, repeated);
    assert!(!manifest.contains("SECRET_TOKEN"), "{manifest}");
    assert!(!manifest.contains("/private/operator"), "{manifest}");
    assert!(!manifest.contains("DIRTY_SECRET"), "{manifest}");
    assert!(
        !manifest.contains(&tmp.path().display().to_string()),
        "{manifest}"
    );
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(manifest["policy_head_sha"], head);
    assert_eq!(manifest["manifest"]["schema_version"], 2);
    assert_eq!(manifest["manifest"]["semantic_advice"]["enforced"], false);
    assert_eq!(manifest["manifest"]["graph_integrity"]["enforced"], true);
    assert_eq!(
        manifest["manifest"]["graph_integrity"]["repository"],
        "fixture"
    );
    assert_eq!(
        manifest["manifest"]["graph_integrity"]["policy_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        manifest["manifest"]["gates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|gate| gate["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["always", "frontend", "backend"]
    );

    let scope = stdout(run(
        tmp.path(),
        &["gates", "scope", "--base", &base, "--head", &head, "--json"],
    ));
    assert!(
        !scope.contains(&tmp.path().display().to_string()),
        "{scope}"
    );
    let scope: serde_json::Value = serde_json::from_str(&scope).unwrap();
    assert_eq!(scope["base_sha"], base);
    assert_eq!(scope["head_sha"], head);
    assert_eq!(scope["graph_integrity"]["enforced"], true);
    assert_eq!(
        scope["changed_paths"],
        serde_json::json!(["assets/blob.bin", "backend/old.rs", "frontend/moved.rs"])
    );
    assert_eq!(
        scope["selected_gates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|gate| gate["gate"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["always", "frontend", "backend"]
    );
    assert_eq!(scope["semantic_suggestions_enforced"], false);
    assert_eq!(scope["semantic_suggestions_included"], false);

    git(tmp.path(), &["commit", "--allow-empty", "-qm", "empty"]);
    let empty = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    let empty_scope = stdout(run(
        tmp.path(),
        &[
            "gates", "scope", "--base", &head, "--head", &empty, "--json",
        ],
    ));
    let empty_scope: serde_json::Value = serde_json::from_str(&empty_scope).unwrap();
    assert_eq!(empty_scope["changed_paths"], serde_json::json!([]));
    assert_eq!(empty_scope["selected_gates"][0]["gate"], "always");

    std::fs::write(tmp.path().join(".aethyme/gates.toml"), "not = [valid").unwrap();
    git(tmp.path(), &["add", ".aethyme/gates.toml"]);
    git(tmp.path(), &["commit", "-qm", "corrupted policy"]);
    let before = git_output(tmp.path(), &["status", "--porcelain=v1"]);
    let corrupted = run(
        tmp.path(),
        &["gates", "manifest", "--head", "HEAD", "--json"],
    );
    assert!(!corrupted.status.success());
    assert!(
        String::from_utf8_lossy(&corrupted.stderr).contains("gates.toml"),
        "{}",
        String::from_utf8_lossy(&corrupted.stderr)
    );
    let missing = run(
        tmp.path(),
        &[
            "gates",
            "scope",
            "--base",
            "missing-base",
            "--head",
            &head,
            "--json",
        ],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot resolve base ref"));
    assert_eq!(
        git_output(tmp.path(), &["status", "--porcelain=v1"]),
        before
    );
}

fn tree_hash(repo: &Path) -> String {
    GitRepo::discover(repo)
        .unwrap()
        .working_tree_hash()
        .unwrap()
}

fn run_count(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}

fn submit_verification_case(gates: Option<&str>) -> (serde_json::Value, String) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join("tracked.txt"), "base\n").unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = 'manual'\n",
    )
    .unwrap();
    if let Some(gates) = gates {
        std::fs::write(tmp.path().join(".aethyme/gates.toml"), gates).unwrap();
    }
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "fixture"]);

    let worktree = tmp.path().join(".aethyme/worktrees/submit");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/submit",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(
        &worktree,
        &["adopt", "--task", "gate evidence", "--json"],
    ));
    let adopted: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session = adopted["id"].as_i64().unwrap().to_string();
    std::fs::create_dir_all(worktree.join("scripts")).unwrap();
    std::fs::write(worktree.join("scripts/check.sh"), "true\n").unwrap();
    git(&worktree, &["add", "scripts/check.sh"]);
    git(&worktree, &["commit", "-qm", "add script"]);

    let json = stdout(run(&worktree, &["submit", "--session", &session, "--json"]));
    let json = serde_json::from_str(&json).unwrap();
    let text = stdout(run(&worktree, &["submit", "--session", &session]));
    (json, text)
}

#[test]
fn submit_distinguishes_missing_unmatched_and_passing_gate_evidence() {
    let (missing, missing_text) = submit_verification_case(None);
    assert_eq!(
        missing["submission_plan"]["merged_tree_paths"],
        serde_json::json!(["scripts/check.sh"])
    );
    assert!(missing_text.contains("session-owned commits: 1"));
    assert!(missing_text.contains("inherited baseline history (not replayed): 0"));
    assert!(missing_text.contains("merged-tree delta: 1 file(s)"));
    assert!(missing_text.contains("scripts/check.sh"));
    assert_eq!(missing["gate_verification"]["status"], "no_configuration");
    assert_eq!(missing["gate_verification"]["selected_gates"], 0);
    assert!(missing_text.contains("no .aethyme/gates.toml"));
    assert!(missing_text.contains("conflict-checked"));
    assert!(!missing_text.contains("→ verified"));

    let (unmatched, unmatched_text) = submit_verification_case(Some(
        "[[gate]]\nname = 'rust-only'\ncommand = 'true'\ntriggers = ['src/**/*.rs']\n",
    ));
    assert_eq!(
        unmatched["gate_verification"]["status"],
        "no_gates_triggered"
    );
    assert_eq!(unmatched["gate_verification"]["configured_gates"], 1);
    assert_eq!(unmatched["gate_verification"]["selected_gates"], 0);
    assert!(unmatched_text.contains("no gate matched this diff"));
    assert!(unmatched_text.contains("conflict-checked"));
    assert!(!unmatched_text.contains("→ verified"));

    let (passed, passed_text) = submit_verification_case(Some(
        "[[gate]]\nname = 'shell'\ncommand = 'true'\ntriggers = ['scripts/*.sh']\n",
    ));
    assert_eq!(passed["gate_verification"]["status"], "passed");
    assert_eq!(passed["gate_verification"]["selected_gates"], 1);
    assert!(passed_text.contains("1 selected gate(s) passed"));
    assert!(passed_text.contains("→ verified"));
}

#[test]
fn gate_cli_reports_tree_provenance_for_executed_and_cached_results() {
    let tmp = fixture();
    let first_tree = tree_hash(tmp.path());

    let executed_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(executed_text.contains(&format!("(tree {})", &first_tree[..12])));
    assert!(!executed_text.contains("(cached)"));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    let cached_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let cached: serde_json::Value = serde_json::from_str(&cached_json).unwrap();
    assert_eq!(cached[0]["cached"], true);
    assert_eq!(cached[0]["tree_hash"], first_tree);
    assert_eq!(cached[0]["wait_duration_ms"], 0);
    assert!(cached[0]["first_output_ms"].as_i64().is_some());
    assert!(cached[0]["output_bytes"].as_i64().unwrap() > 0);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    std::fs::write(tmp.path().join("tracked.txt"), "second\n").unwrap();
    let second_tree = tree_hash(tmp.path());
    assert_ne!(second_tree, first_tree);

    let executed_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let executed: serde_json::Value = serde_json::from_str(&executed_json).unwrap();
    assert_eq!(executed[0]["cached"], false);
    assert_eq!(executed[0]["tree_hash"], second_tree);
    assert!(executed[0]["wait_duration_ms"].as_i64().is_some());
    assert!(executed[0]["first_output_ms"].as_i64().is_some());
    assert!(executed[0]["output_bytes"].as_i64().unwrap() > 0);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 2);

    let cached_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(cached_text.contains("(cached)"));
    assert!(cached_text.contains(&format!("(tree {})", &second_tree[..12])));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 2);

    let bypassed_json = stdout(run(
        tmp.path(),
        &["gates", "run", "--all", "--no-cache", "--json"],
    ));
    let bypassed: serde_json::Value = serde_json::from_str(&bypassed_json).unwrap();
    assert_eq!(bypassed[0]["cached"], false);
    assert_eq!(bypassed[0]["tree_hash"], second_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 3);

    let refreshed_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let refreshed: serde_json::Value = serde_json::from_str(&refreshed_json).unwrap();
    assert_eq!(refreshed[0]["cached"], true);
    assert_eq!(refreshed[0]["tree_hash"], second_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 3);

    let adopted = stdout(run(
        tmp.path(),
        &["adopt", "--task", "session gates", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    let session_bypass = stdout(run(
        tmp.path(),
        &[
            "gates",
            "run",
            "--session",
            &session_id,
            "--no-cache",
            "--json",
        ],
    ));
    let session_bypass: serde_json::Value = serde_json::from_str(&session_bypass).unwrap();
    assert_eq!(session_bypass[0]["cached"], false);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 4);

    let session_cached = stdout(run(
        tmp.path(),
        &["gates", "run", "--session", &session_id, "--json"],
    ));
    let session_cached: serde_json::Value = serde_json::from_str(&session_cached).unwrap();
    assert_eq!(session_cached[0]["cached"], true);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 4);
}

#[test]
fn gate_validate_reads_the_invoking_worktree_snapshot() {
    let tmp = fixture();
    let worktree = tmp.path().join(".aethyme/worktrees/validate");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/validate",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(
        worktree.join(".aethyme/gates.toml"),
        "[[gate]]\nname='worktree-only'\ncommand='true'\n",
    )
    .unwrap();

    let output = stdout(run(&worktree, &["gates", "validate", "--json"]));
    let gates: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(gates[0]["name"], "worktree-only");
}

#[test]
fn pre_push_adapter_proves_the_clean_outgoing_head_and_refuses_drift() {
    let tmp = fixture();
    let head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    let update = format!(
        "refs/heads/main {head} refs/heads/main {}\n",
        "0".repeat(40)
    );

    let verified = stdout(run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url", "--json"],
        &update,
    ));
    let report: serde_json::Value = serde_json::from_str(&verified).unwrap();
    assert_eq!(report["plan"]["remote"], "origin");
    assert_eq!(report["plan"]["pushed_sha"], head);
    assert_eq!(report["plan"]["updates"].as_array().unwrap().len(), 1);
    assert_eq!(report["gate_outcomes"][0]["status"], "pass");
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    std::fs::write(tmp.path().join("tracked.txt"), "dirty\n").unwrap();
    let dirty = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &update,
    );
    assert!(!dirty.status.success());
    assert!(String::from_utf8_lossy(&dirty.stderr).contains("requires a clean checkout"));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    let wrong_tip = format!(
        "refs/heads/main {} refs/heads/main {}\n",
        "a".repeat(40),
        "0".repeat(40)
    );
    let mismatch = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &wrong_tip,
    );
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("is not this checkout's HEAD"));

    let multiple_tips = format!(
        "refs/heads/a {} refs/heads/a {}\nrefs/heads/b {} refs/heads/b {}\n",
        "a".repeat(40),
        "0".repeat(40),
        "b".repeat(40),
        "0".repeat(40),
    );
    let ambiguous = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &multiple_tips,
    );
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("cannot prove multiple different local tips")
    );

    let empty = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        "",
    );
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("received no ref updates"));

    let deletion = format!("(delete) {} refs/heads/old {head}\n", "0".repeat(40));
    let deleted = stdout(run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url", "--json"],
        &deletion,
    ));
    let report: serde_json::Value = serde_json::from_str(&deleted).unwrap();
    assert!(report["plan"]["pushed_sha"].is_null());
    assert!(report["gate_outcomes"].as_array().unwrap().is_empty());
}

#[test]
fn submit_cli_reports_an_unchanged_worktree_submission_as_a_noop() {
    let tmp = fixture();
    let worktree = tmp.path().join(".aethyme/worktrees/noop-submit");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/noop-submit",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(
        &worktree,
        &["adopt", "--task", "inspect only", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();

    let json = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let outcome: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(outcome["no_changes"], true);
    assert_eq!(outcome["promoted"], false);
    assert_eq!(outcome["entry"]["status"], "superseded");
    assert_eq!(outcome["gate_outcomes"], serde_json::json!([]));
    assert!(!tmp.path().join("gate-runs.txt").exists());

    let text = stdout(run(&worktree, &["submit", "--session", &session_id]));
    assert!(text.contains("no pending session-owned content"));
    assert!(text.contains("integration was not moved and no gates ran"));
    assert!(!text.contains("gate wall time"));
}

#[test]
fn submit_cli_bypasses_and_then_refreshes_the_merged_tree_cache() {
    let tmp = fixture();
    let counter = tmp.path().join("submit-runs.txt");
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"manual\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        format!(
            "[[gate]]\nname = \"submit-cache\"\ncommand = \"echo run >> '{}'\"\n",
            counter.display()
        ),
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "configure submit gate"]);

    let worktree = tmp.path().join(".aethyme/worktrees/submit-cache");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/submit-cache",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(
        &worktree,
        &["adopt", "--task", "submit cache", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(worktree.join("tracked.txt"), "submitted\n").unwrap();
    git(&worktree, &["add", "tracked.txt"]);
    git(&worktree, &["commit", "-qm", "submitted change"]);

    let first = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["gate_outcomes"][0]["cached"], false);
    assert_eq!(run_count(&counter), 1);

    let cached = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let cached: serde_json::Value = serde_json::from_str(&cached).unwrap();
    assert_eq!(cached["gate_outcomes"][0]["cached"], true);
    assert_eq!(run_count(&counter), 1);

    let bypassed = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--no-cache", "--json"],
    ));
    let bypassed: serde_json::Value = serde_json::from_str(&bypassed).unwrap();
    assert_eq!(bypassed["gate_outcomes"][0]["cached"], false);
    assert_eq!(run_count(&counter), 2);

    let refreshed = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let refreshed: serde_json::Value = serde_json::from_str(&refreshed).unwrap();
    assert_eq!(refreshed["gate_outcomes"][0]["cached"], true);
    assert_eq!(run_count(&counter), 2);
}

#[test]
fn session_gate_proof_is_reused_by_unchanged_integration_submission() {
    let tmp = fixture();
    let counter = tmp.path().join("submit-runs.txt");
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        format!(
            "[[gate]]\nname='exact-tree'\ncommand=\"echo run >> '{}'\"\ntriggers=['tracked.txt']\n",
            counter.display()
        ),
    )
    .unwrap();
    git(tmp.path(), &["add", ".aethyme/gates.toml"]);
    git(tmp.path(), &["commit", "-qm", "configure exact-tree gate"]);

    let worktree = tmp.path().join(".aethyme/worktrees/exact-tree");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/exact-tree",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(&worktree, &["adopt", "--task", "prove once", "--json"]));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(worktree.join("tracked.txt"), "proved\n").unwrap();
    git(&worktree, &["add", "tracked.txt"]);
    git(&worktree, &["commit", "-qm", "change tracked input"]);

    let preflight = stdout(run(
        &worktree,
        &["gates", "run", "--session", &session_id, "--json"],
    ));
    let preflight: serde_json::Value = serde_json::from_str(&preflight).unwrap();
    assert_eq!(preflight[0]["cached"], false);
    assert_eq!(run_count(&counter), 1);

    let submission = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let submission: serde_json::Value = serde_json::from_str(&submission).unwrap();
    assert_eq!(submission["gate_outcomes"][0]["cached"], true);
    assert_eq!(
        submission["gate_outcomes"][0]["tree_hash"],
        preflight[0]["tree_hash"]
    );
    assert_eq!(run_count(&counter), 1);
}

#[test]
fn independent_repositories_share_gate_host_resources_and_release_them() {
    let first = fixture();
    let second = fixture();
    let state = tempfile::tempdir().unwrap();
    let config = r#"
[[gate]]
name = "host-resources"
command = "test -n \"$AETHYME_RESOURCE_DOCKER_PROJECT\" && test \"$AETHYME_RESOURCE_DATABASE\" = host-test-shared && sleep 6"
cache = false
resource_ttl_seconds = 15

[[gate.resources]]
key = "docker_project"
kind = "namespace"
prefix = "gate-test"

[[gate.resources]]
key = "database"
kind = "exclusive_key"
name = "host-test-shared"
"#;
    for repo in [first.path(), second.path()] {
        std::fs::write(repo.join(".aethyme/gates.toml"), config).unwrap();
        git(repo, &["add", ".aethyme/gates.toml"]);
        git(repo, &["commit", "-qm", "configure host resource gate"]);
    }

    let running = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(first.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let db_path = state.path().join("host-resources.db");
    let mut observed = false;
    for _ in 0..100 {
        if db_path.exists()
            && aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
                .and_then(|coordinator| coordinator.list(false))
                .is_ok_and(|leases| leases.len() == 1)
        {
            observed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(observed, "first gate never acquired its host bundle");

    let blocked = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(second.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    let blocked_json: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked_json[0]["status"], "error");
    assert_eq!(blocked_json[0]["failure_class"], "resource_contention");
    assert!(blocked_json[0].get("resource_lease").is_none());

    let completed = running.wait_with_output().unwrap();
    assert!(
        completed.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&completed.stdout),
        String::from_utf8_lossy(&completed.stderr)
    );
    let completed_json: serde_json::Value = serde_json::from_slice(&completed.stdout).unwrap();
    assert_eq!(completed_json[0]["status"], "pass");
    assert_eq!(
        completed_json[0]["resource_lease"]["allocations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        completed_json[0]["definition_hash"].as_str().unwrap().len(),
        64
    );
    let initial_expiry = completed_json[0]["resource_lease"]["expires_at"]
        .as_i64()
        .unwrap();
    let inventory = aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
        .unwrap()
        .list(true)
        .unwrap();
    assert!(
        inventory[0].expires_at > initial_expiry,
        "long-running gate did not renew its host resource lease"
    );
    assert!(
        aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty(),
        "successful gate did not release its host resources"
    );

    let failing_config = config.replace(" && sleep 6", " && false");
    std::fs::write(first.path().join(".aethyme/gates.toml"), failing_config).unwrap();
    let failed = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(first.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(
        aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty(),
        "failing gate did not release its host resources"
    );
}

#[test]
fn managed_gate_cache_is_bounded_exported_and_exclusively_leased() {
    let repo = fixture();
    let host_state = tempfile::tempdir().unwrap();
    let host_cache = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join(".aethyme/gates.toml"),
        r#"
[[gate]]
name = "managed-cache"
command = "test -d \"$AETHYME_GATE_CACHE_DIR\" && printf 1234 > \"$AETHYME_GATE_CACHE_DIR/artifact\""
cache = false
resource_wait_seconds = 1

[gate.managed_cache]
key = "fixture"
max_bytes = 3
"#,
    )
    .unwrap();

    let run = || {
        Command::new(CLI)
            .args(["gates", "run", "--all", "--no-cache", "--json"])
            .current_dir(repo.path())
            .env("AETHYME_HOST_STATE_DIR", host_state.path())
            .env("AETHYME_HOST_CACHE_DIR", host_cache.path())
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first[0]["managed_cache"]["bytes_before"], 0);
    assert_eq!(first[0]["managed_cache"]["bytes_after"], 4);
    assert_eq!(first[0]["managed_cache"]["rotated_before_run"], false);
    assert_eq!(
        first[0]["resource_lease"]["allocations"][0]["kind"],
        "exclusive_key"
    );

    let second = run();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second[0]["managed_cache"]["bytes_before"], 4);
    assert_eq!(second[0]["managed_cache"]["bytes_after"], 4);
    assert_eq!(second[0]["managed_cache"]["rotated_before_run"], true);
}

#[test]
fn named_gate_rerun_executes_only_that_gate_and_replays_failure_tail() {
    let repo = fixture();
    std::fs::write(
        repo.path().join(".aethyme/gates.toml"),
        r#"
[[gate]]
name = "unrelated"
command = "printf unrelated >> gate-runs.txt"

[[gate]]
name = "spelling"
command = "printf 'diagnostic-one\ndiagnostic-two\n' >&2; exit 7"
"#,
    )
    .unwrap();

    let failed = run(
        repo.path(),
        &["gates", "run", "--all", "--only", "spelling", "--no-cache"],
    );
    assert!(!failed.status.success());
    let stdout = String::from_utf8(failed.stdout).unwrap();
    let stderr = String::from_utf8(failed.stderr).unwrap();
    assert!(stdout.contains("spelling"));
    assert!(!stdout.contains("unrelated"));
    assert!(stderr.contains("gate spelling output (last 2 line(s))"));
    assert!(stderr.contains("diagnostic-one"));
    assert!(stderr.contains("diagnostic-two"));
    assert!(!repo.path().join("gate-runs.txt").exists());

    let unknown = run(repo.path(), &["gates", "run", "--all", "--only", "missing"]);
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8(unknown.stderr)
            .unwrap()
            .contains("no configured gate named")
    );
}

#[test]
fn submit_replays_a_bounded_gate_failure_tail() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join("tracked.txt"), "base\n").unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = 'failure'\ncommand = \"printf 'submit-diagnostic\\n' >&2; exit 1\"\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "fixture"]);

    let worktree = tmp.path().join(".aethyme/worktrees/submit-failure");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/submit-failure",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted: serde_json::Value = serde_json::from_str(&stdout(run(
        &worktree,
        &["adopt", "--task", "failure output", "--json"],
    )))
    .unwrap();
    let session = adopted["id"].as_i64().unwrap().to_string();
    std::fs::write(worktree.join("tracked.txt"), "changed\n").unwrap();
    git(&worktree, &["add", "tracked.txt"]);
    git(&worktree, &["commit", "-qm", "change"]);

    let failed = run(&worktree, &["submit", "--session", &session]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8(failed.stderr).unwrap();
    assert!(stderr.contains("gate failure output (last 1 line(s))"));
    assert!(stderr.contains("submit-diagnostic"));
}
