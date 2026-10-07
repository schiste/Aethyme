//! #584: `review plan` measures the change and, by default, only flags it.

use std::path::Path;
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().into()
}

/// A repository with `policy` committed on `main`, and a `change` branch that
/// writes `files` on top of it.
fn repository(policy: &str, files: &[(&str, String)]) -> tempfile::TempDir {
    repository_with_base(policy, &[], files)
}

/// As [`repository`], with `base_files` committed on `main` too.
fn repository_with_base(
    policy: &str,
    base_files: &[(&str, String)],
    files: &[(&str, String)],
) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(path, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(path.join(".aethyme")).unwrap();
    std::fs::write(path.join(".aethyme/config.toml"), policy).unwrap();
    std::fs::write(
        path.join(".gitignore"),
        "/.aethyme/broker.db*\n/host-state/\n",
    )
    .unwrap();
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    for (file, content) in base_files {
        let target = path.join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, content).unwrap();
    }
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "initial"]);
    git(path, &["checkout", "-q", "-b", "change"]);
    for (file, content) in files {
        let target = path.join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, content).unwrap();
    }
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "change"]);
    root
}

fn plan(root: &Path) -> serde_json::Value {
    let output = common::broker_cli(
        CLI,
        &["advanced", "review", "plan", "--base", "main", "--pr", "7"],
    )
    .current_dir(root)
    .env("AETHYME_HOST_STATE_DIR", root.join("host-state"))
    .env("AETHYME_AGENT_PID", std::process::id().to_string())
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "review plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn lines(count: usize) -> String {
    (0..count).map(|line| format!("line {line}\n")).collect()
}

fn added_labels(plan: &serde_json::Value) -> Vec<String> {
    plan["projection"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["action"] == "add_labels")
        .flat_map(|action| action["names"].as_array().unwrap().clone())
        .map(|name| name.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn projection_alone_flags_size_and_risk_and_does_nothing_else() {
    let root = repository(
        "[review.projection]\nenabled = true\n",
        &[("docs/guide.md", lines(3))],
    );
    let plan = plan(root.path());

    assert_eq!(plan["change"]["tier"], "trivial");
    assert_eq!(plan["change"]["risk"], "none");
    assert_eq!(plan["change"]["size"]["files_changed"], 1);
    assert_eq!(plan["change"]["size"]["churn"], 3);

    // Flagging only: no rule fired, nothing was decided, nothing dispatched.
    assert_eq!(plan["eligible"], serde_json::json!([]));
    assert_eq!(plan["decisions"], serde_json::json!([]));
    assert_eq!(plan["dispatch"], serde_json::json!([]));
    assert_eq!(plan["performed"], false);

    let labels = added_labels(&plan);
    assert!(
        labels.contains(&"aethyme/size:trivial".to_string()),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"aethyme/risk:none".to_string()),
        "{labels:?}"
    );
    // Every projected action is a comment or a label: no mention, no review,
    // no waiver.
    for action in plan["projection"].as_array().unwrap() {
        let kind = action["action"].as_str().unwrap();
        assert!(
            matches!(kind, "create_comment" | "create_label" | "add_labels"),
            "unexpected action {action}"
        );
    }
    let comment = plan["projection"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["action"] == "create_comment")
        .unwrap();
    assert!(
        comment["body"]
            .as_str()
            .unwrap()
            .contains("Size **trivial** (1 files, +3/−0); risk **none**"),
        "{comment}"
    );
}

#[test]
fn a_change_over_pr_size_touching_a_workflow_is_large_and_risky() {
    let root = repository(
        "[review]\npr_size = { max_files = 5, max_changed_lines = 50 }\n\n\
         [review.projection]\nenabled = true\n",
        &[
            (".github/workflows/ci.yml", lines(10)),
            ("src/lib.rs", lines(60)),
        ],
    );
    let plan = plan(root.path());
    assert_eq!(plan["change"]["tier"], "large");
    assert_eq!(plan["change"]["risk"], "high");
    assert_eq!(plan["change"]["risky"], true);
    assert_eq!(
        plan["change"]["signals"]["workflows"],
        serde_json::json!([".github/workflows/ci.yml"])
    );
    assert_eq!(plan["change"]["thresholds"]["pr_size"]["max_files"], 5);
    let labels = added_labels(&plan);
    assert!(
        labels.contains(&"aethyme/size:large".to_string()),
        "{labels:?}"
    );
    assert!(
        labels.contains(&"aethyme/risk:high".to_string()),
        "{labels:?}"
    );
    assert_eq!(plan["dispatch"], serde_json::json!([]));
}

#[test]
fn without_projection_the_change_is_measured_but_nothing_is_projected() {
    let root = repository("", &[("src/lib.rs", lines(5))]);
    let plan = plan(root.path());
    assert_eq!(plan["change"]["tier"], "trivial");
    assert_eq!(plan["projection_enabled"], false);
    assert_eq!(plan["projection"], serde_json::json!([]));
}

#[test]
fn generated_files_and_lockfiles_do_not_count_toward_size() {
    // The base declares `gen/**` generated; the change adds files under it.
    let root = repository_with_base(
        "[review.projection]\nenabled = true\n",
        &[(".gitattributes", "gen/** linguist-generated\n".to_string())],
        &[
            ("gen/out.rs", lines(5000)),
            ("Cargo.lock", lines(3000)),
            ("src/lib.rs", lines(2)),
        ],
    );
    let plan = plan(root.path());
    // Only `src/lib.rs` counts; the generated file and the lockfile do not.
    assert_eq!(plan["change"]["size"]["files_changed"], 1);
    assert_eq!(plan["change"]["tier"], "trivial");
    assert_eq!(
        plan["change"]["signals"]["dependency_manifest"],
        serde_json::json!(["Cargo.lock"])
    );
}

#[test]
fn a_change_cannot_mark_its_own_files_generated_to_shrink_its_size() {
    // The head's `.gitattributes` marks everything generated; the base's does
    // not, so every file still counts and the change stays large.
    let root = repository(
        "[review.projection]\nenabled = true\n",
        &[
            (".gitattributes", "* linguist-generated\n".to_string()),
            ("src/lib.rs", lines(2000)),
        ],
    );
    let plan = plan(root.path());
    assert_eq!(
        plan["change"]["size"]["files_changed"], 2,
        "{}",
        plan["change"]
    );
    assert_eq!(plan["change"]["size"]["lines_added"], 2001);
    assert_eq!(plan["change"]["tier"], "large");
    assert!(plan["change"]["size"].get("excluded").is_none());
}
