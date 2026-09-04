use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", message]);
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = temporary.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::create_dir_all(repo.join(".aethyme")).unwrap();
    fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority='committed_fragments'\nrepository='fixture'\n",
    )
    .unwrap();
    fs::write(
        repo.join(".aethyme/engine-version"),
        format!("{}\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    fs::write(
        repo.join(".gitignore"),
        ".aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/graph_store.redb\n.aethyme/graph_store.redb.staging\n",
    )
    .unwrap();
    fs::write(repo.join("app.py"), "def answer():\n    return 1\n").unwrap();
    commit_all(&repo, "fixture");
    (temporary, repo)
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(repo)
        .env_remove("AETHYME_ROOT")
        .env("XDG_CONFIG_HOME", repo.join("empty-config"))
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn plan(repo: &Path) -> Value {
    serde_json::from_str(&success(run(
        repo,
        &["graph", "refresh", "plan", "--repo", ".", "--json"],
    )))
    .unwrap()
}

#[test]
fn plan_is_read_only_deterministic_and_contains_no_absolute_paths_or_source() {
    let (_temporary, repo) = fixture();
    let before = git(&repo, &["status", "--porcelain"]);
    let first = plan(&repo);
    let second = plan(&repo);
    let mut first_authority = first.clone();
    let mut second_authority = second.clone();
    first_authority
        .as_object_mut()
        .unwrap()
        .remove("performance");
    second_authority
        .as_object_mut()
        .unwrap()
        .remove("performance");
    assert_eq!(first_authority, second_authority);
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["canonical_repository"], "fixture");
    assert_eq!(first["compatibility"], "compatible");
    assert_eq!(first["safe_to_execute"], true);
    assert_eq!(first["work"]["disposable_clones"], 1);
    assert_eq!(first["work"]["source_index_runs"], 1);
    assert!(
        first["performance"]["source_indexing"]["bytes_read"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        first["performance"]["fragment_serialization"]["bytes_written"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(first["performance"]["counts"]["files"].as_u64().unwrap() >= 1);
    assert!(first["changes"].as_array().unwrap().len() >= 2);
    assert_eq!(first["derived_store"]["status"], "missing");
    assert_eq!(git(&repo, &["status", "--porcelain"]), before);
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(!encoded.contains(&repo.display().to_string()), "{encoded}");
    assert!(!encoded.contains("return 1"), "{encoded}");
}

#[test]
fn confirmed_execute_refreshes_fragments_then_materializes_the_local_store() {
    let (_temporary, repo) = fixture();
    let planned = plan(&repo);
    let digest = planned["plan_sha256"].as_str().unwrap();

    let mismatch = run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            &"0".repeat(64),
        ],
    );
    assert!(!mismatch.status.success());
    assert!(!repo.join(".aethyme/graph_store.redb").exists());

    success(run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ],
    ));
    assert!(repo.join(".aethyme/graph/app.py.bin").is_file());
    assert!(repo.join(".aethyme/graph_store.redb").is_file());

    let status: Value = serde_json::from_str(&success(run(
        &repo,
        &["graph", "status", "--repo", ".", "--json"],
    )))
    .unwrap();
    assert_eq!(status["derived_store"]["status"], "current");
    assert_eq!(status["fragments"]["working_tree_matches_proposal"], true);
    assert!(status["next_action"].as_str().unwrap().contains("commit"));

    commit_all(&repo, "refresh graph");
    let current = plan(&repo);
    assert_eq!(current["fragments"]["status"], "passed");
    assert_eq!(current["changes"], serde_json::json!([]));
}

#[test]
fn materialize_rebuilds_only_the_local_store_from_verified_committed_fragments() {
    let (_temporary, repo) = fixture();
    let planned = plan(&repo);
    let digest = planned["plan_sha256"].as_str().unwrap();
    success(run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ],
    ));
    commit_all(&repo, "commit graph");
    fs::remove_file(repo.join(".aethyme/graph_store.redb")).unwrap();
    let before = git(&repo, &["status", "--porcelain"]);

    let report: Value = serde_json::from_str(&success(run(
        &repo,
        &["graph", "materialize", "--repo", ".", "--json"],
    )))
    .unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["canonical_repository"], "fixture");
    assert_eq!(report["fragment_status"], "passed");
    assert_eq!(report["action"], "materialize_after_fragment_verification");
    assert_eq!(report["work"]["disposable_clones"], 0);
    assert_eq!(report["work"]["source_index_runs"], 0);
    assert_eq!(report["source_head"], git(&repo, &["rev-parse", "HEAD"]));
    assert!(report["file_count"].as_u64().unwrap() >= 1);
    assert!(
        report["performance"]["redb_materialization"]["bytes_read"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        report["performance"]["redb_materialization"]["bytes_written"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["performance"]["counts"]["nodes"].as_u64().unwrap() >= 1);
    assert!(report["performance"]["counts"]["edges"].is_number());
    assert!(repo.join(".aethyme/graph_store.redb").is_file());
    assert_eq!(git(&repo, &["status", "--porcelain"]), before);

    let current: Value = serde_json::from_str(&success(run(
        &repo,
        &["graph", "materialize", "--repo", ".", "--json"],
    )))
    .unwrap();
    assert_eq!(current["action"], "none");
    assert_eq!(
        current["performance"]["redb_materialization"]["bytes_written"],
        0
    );

    let explored: Value = serde_json::from_str(&success(run(
        &repo,
        &[
            "explore",
            "--repo",
            ".",
            "--request",
            "locate the answer function",
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )))
    .unwrap();
    let performance = &explored["observability"]["performance"];
    assert!(performance["repository_discovery_elapsed_us"].is_number());
    assert!(performance["graph_store_open_elapsed_us"].is_number());
    assert!(performance["query_execution_elapsed_us"].is_number());
    assert!(performance["total_elapsed_us"].is_number());
    assert!(performance["store_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn disabled_status_is_a_cheap_healthy_no_op() {
    let (_temporary, repo) = fixture();
    fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority='disabled'\n",
    )
    .unwrap();
    commit_all(&repo, "disable graph");

    let status: Value = serde_json::from_str(&success(run(
        &repo,
        &["graph", "status", "--repo", ".", "--json"],
    )))
    .unwrap();
    assert_eq!(status["compatibility"], "authority_disabled");
    assert_eq!(status["healthy"], true);
    assert_eq!(status["action_required"], false);
    assert_eq!(status["refresh_plan_required"], false);
    assert_eq!(status["blockers"], serde_json::json!([]));
    assert_eq!(status["fragments"]["status"], "disabled");
    assert_eq!(status["derived_store"]["status"], "disabled");
    assert_eq!(status["derived_store"]["action"], "none");
    assert_eq!(status["work"]["disposable_clones"], 0);
    assert_eq!(status["work"]["source_index_runs"], 0);
    assert_eq!(
        status["performance"]["fragment_validation"]["bytes_read"],
        0
    );
    assert_eq!(status["performance"]["source_indexing"]["bytes_read"], 0);
    assert!(
        status["next_action"]
            .as_str()
            .unwrap()
            .contains("no action is required")
    );
}

#[test]
fn materialize_refuses_disabled_or_stale_committed_graph_authority() {
    let (_temporary, repo) = fixture();
    fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority='disabled'\n",
    )
    .unwrap();
    commit_all(&repo, "disable graph");
    let disabled = run(&repo, &["graph", "materialize", "--repo", "."]);
    assert!(!disabled.status.success());
    assert!(
        String::from_utf8_lossy(&disabled.stderr)
            .contains("requires compatible committed graph authority")
    );

    fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority='committed_fragments'\nrepository='fixture'\n",
    )
    .unwrap();
    commit_all(&repo, "restore graph authority");
    let stale = run(&repo, &["graph", "materialize", "--repo", "."]);
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stderr)
            .contains("committed graph fragments do not match committed HEAD")
    );
}

#[test]
fn materialize_refuses_exact_head_drift_without_cloning_or_indexing() {
    let (_temporary, repo) = fixture();
    let planned = plan(&repo);
    success(run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            planned["plan_sha256"].as_str().unwrap(),
        ],
    ));
    commit_all(&repo, "commit graph");
    fs::write(repo.join("app.py"), "def answer():\n    return 2\n").unwrap();
    commit_all(&repo, "change source without graph");

    let status: Value = serde_json::from_str(&success(run(
        &repo,
        &["graph", "status", "--repo", ".", "--json"],
    )))
    .unwrap();
    assert_eq!(status["fragments"]["status"], "stale");
    assert_eq!(status["refresh_plan_required"], true);
    assert_eq!(status["work"]["disposable_clones"], 0);
    assert_eq!(status["work"]["source_index_runs"], 0);
    assert!(
        status["diagnosis"]
            .as_str()
            .unwrap()
            .contains("exact source tree")
    );

    let materialize = run(&repo, &["graph", "materialize", "--repo", "."]);
    assert!(!materialize.status.success());
    assert!(
        String::from_utf8_lossy(&materialize.stderr)
            .contains("committed graph fragments do not match committed HEAD")
    );
}

#[test]
fn explore_returns_structured_degradation_when_the_opt_in_store_is_missing() {
    let (_temporary, repo) = fixture();
    let response: Value = serde_json::from_str(&success(run(
        &repo,
        &[
            "explore",
            "--repo",
            ".",
            "--request",
            "locate the answer function",
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )))
    .unwrap();
    assert_eq!(response["status"], "degraded");
    assert_eq!(response["safe_to_use_as_answer"], false);
    assert_eq!(response["safe_to_use_as_navigation"], false);
    assert_eq!(response["answer"], serde_json::json!([]));
    assert_eq!(
        response["degraded_reasons"],
        serde_json::json!(["graph_store_missing"])
    );
    assert_eq!(
        response["observability"]["graph_store"]["status"],
        "missing"
    );
    assert!(
        !serde_json::to_string(&response)
            .unwrap()
            .contains(repo.to_string_lossy().as_ref())
    );
    assert!(
        response["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("aethyme graph materialize --repo .")
    );
}

#[test]
fn explore_degradation_uses_the_typed_graph_policy() {
    let (_temporary, repo) = fixture();
    fs::write(
        repo.join(".aethyme/config.toml"),
        "[graph]\nauthority  =  \"committed_fragments\"\nrepository = \"fixture\"\n",
    )
    .unwrap();
    commit_all(&repo, "format graph policy differently");

    let response: Value = serde_json::from_str(&success(run(
        &repo,
        &[
            "explore",
            "--repo",
            ".",
            "--request",
            "locate the answer function",
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )))
    .unwrap();
    assert!(
        response["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("aethyme graph materialize")
    );
}

#[test]
fn explore_degradation_keeps_graph_enrollment_optional() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = temporary.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "fixture\n").unwrap();
    commit_all(&repo, "fixture");

    let response: Value = serde_json::from_str(&success(run(
        &repo,
        &[
            "explore",
            "--repo",
            ".",
            "--request",
            "orient me",
            "--format",
            "answer-json",
        ],
    )))
    .unwrap();
    assert_eq!(response["status"], "degraded");
    assert!(
        response["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("Graph support is optional")
    );
    assert!(
        response["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("--with-graph")
    );
}

#[test]
fn wrong_pin_and_dirty_overlap_are_explicit_blockers() {
    let (_temporary, repo) = fixture();
    fs::write(repo.join(".aethyme/engine-version"), "0.0.0\n").unwrap();
    commit_all(&repo, "wrong pin");
    let incompatible = plan(&repo);
    assert_eq!(incompatible["compatibility"], "version_mismatch");
    assert_eq!(incompatible["safe_to_execute"], false);
    assert!(
        incompatible["blockers"][0]
            .as_str()
            .unwrap()
            .contains("signed compatible release")
    );

    fs::write(
        repo.join(".aethyme/engine-version"),
        format!("{}\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    commit_all(&repo, "restore pin");
    let initial = plan(&repo);
    let digest = initial["plan_sha256"].as_str().unwrap();
    success(run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ],
    ));
    commit_all(&repo, "initial graph");
    fs::write(repo.join("app.py"), "def answer():\n    return 2\n").unwrap();
    commit_all(&repo, "source change");
    fs::write(repo.join(".aethyme/graph/app.py.bin"), b"corrupted").unwrap();
    let blocked = plan(&repo);
    assert_eq!(blocked["safe_to_execute"], false);
    assert_eq!(
        blocked["overlapping_dirty_paths"],
        serde_json::json!([".aethyme/graph/app.py.bin"])
    );
}

#[test]
fn disjoint_dirty_work_is_not_an_input_and_does_not_block() {
    let (_temporary, repo) = fixture();
    fs::write(
        repo.join("local-notes.txt"),
        "not an input to committed HEAD\n",
    )
    .unwrap();
    let planned = plan(&repo);
    assert_eq!(planned["safe_to_execute"], true);
    assert_eq!(
        planned["disjoint_dirty_paths"],
        serde_json::json!(["local-notes.txt"])
    );
}

#[test]
fn execute_refuses_when_committed_head_moves_after_review() {
    let (_temporary, repo) = fixture();
    let planned = plan(&repo);
    let digest = planned["plan_sha256"].as_str().unwrap();
    fs::write(repo.join("added.py"), "def later():\n    return 2\n").unwrap();
    commit_all(&repo, "move head");

    let refused = run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ],
    );
    assert!(!refused.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(output.contains("state changed after review"), "{output}");
    assert!(!repo.join(".aethyme/graph_store.redb").exists());
}

#[test]
fn committed_corrupt_fragments_are_repaired_from_committed_source() {
    let (_temporary, repo) = fixture();
    fs::create_dir_all(repo.join(".aethyme/graph")).unwrap();
    fs::write(repo.join(".aethyme/graph/app.py.bin"), b"corrupted").unwrap();
    commit_all(&repo, "corrupt graph fragment");

    let planned = plan(&repo);
    assert_eq!(planned["safe_to_execute"], true);
    assert!(planned["changes"].as_array().unwrap().iter().any(|change| {
        change["path"] == ".aethyme/graph/app.py.bin" && change["action"] == "update"
    }));
    let digest = planned["plan_sha256"].as_str().unwrap();
    success(run(
        &repo,
        &[
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ],
    ));
    assert_ne!(
        fs::read(repo.join(".aethyme/graph/app.py.bin")).unwrap(),
        b"corrupted"
    );
}

#[test]
fn active_sessions_block_shared_fragment_writes() {
    let (_temporary, repo) = fixture();
    let mut broker = aethyme_broker::Broker::open(&repo).unwrap();
    let session = broker.adopt(&repo, Some("hold graph policy")).unwrap();
    let planned = plan(&repo);
    assert_eq!(planned["safe_to_execute"], false);
    assert_eq!(planned["active_sessions"][0]["session_id"], session.id);
    assert!(
        planned["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker.as_str().unwrap().contains("sessions are live"))
    );
}

#[test]
fn interrupted_apply_is_completed_only_by_explicit_digest_recovery() {
    let (_temporary, repo) = fixture();
    let planned = plan(&repo);
    let digest = planned["plan_sha256"].as_str().unwrap();
    let interrupted = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args([
            "graph",
            "refresh",
            "execute",
            "--repo",
            ".",
            "--confirm",
            digest,
        ])
        .current_dir(&repo)
        .env("AETHYME_TEST_GRAPH_REFRESH_CRASH_AFTER_FILES", "1")
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert!(!repo.join(".aethyme/graph_store.redb").exists());

    success(run(
        &repo,
        &[
            "graph", "refresh", "recover", "--repo", ".", "--plan", digest,
        ],
    ));
    assert!(repo.join(".aethyme/graph/app.py.bin").is_file());
    assert!(repo.join(".aethyme/graph_store.redb").is_file());
}

#[cfg(unix)]
#[test]
fn symlinked_graph_output_is_refused_without_writing_through_it() {
    use std::os::unix::fs::symlink;

    let (_temporary, repo) = fixture();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), repo.join(".aethyme/graph")).unwrap();
    let planned = plan(&repo);
    assert_eq!(planned["safe_to_execute"], false);
    assert!(
        planned["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker.as_str().unwrap().contains("symlink"))
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
