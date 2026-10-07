//! Text contracts for the deliberately explicit CI schedule. YAML syntax is
//! validated separately; these checks guard ownership of expensive coverage.
use std::path::Path;

fn workflow(name: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    std::fs::read_to_string(root.join(".github/workflows").join(name)).unwrap()
}

/// Extract an explicitly indented mapping body, excluding adjacent mappings.
fn block<'a>(text: &'a str, header: &str) -> Vec<&'a str> {
    let indent = header.len() - header.trim_start().len();
    let mut lines = text.lines();
    assert!(lines.any(|line| line == header), "missing {header}");
    lines
        .take_while(|line| {
            line.trim().is_empty()
                || line.trim_start().starts_with('#')
                || line.len() - line.trim_start().len() > indent
        })
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect()
}

#[test]
fn pr_and_main_have_one_automatic_full_workspace_owner() {
    let oss = workflow("oss-ci.yml");
    assert_eq!(
        block(&oss, "on:"),
        [
            "  pull_request:",
            "  push:",
            "    branches: [\"main\", \"master\"]"
        ]
    );
    let rust = block(&oss, "  rust-tests:");
    assert!(rust.contains(&"    if: github.event_name == 'pull_request'"));
    // The workspace suite runs under nextest. `cargo test --workspace
    // --examples` once stood in for it, but a target flag narrows cargo's
    // selection to examples only, so it ran three binaries.
    assert!(rust.contains(&"        run: ../scripts/test-like-ci.sh --full"));
    // nextest runs neither example targets nor doctests, and the
    // release-manifest assertions in
    // `crates/aethyme-broker/examples/release_manifest.rs` live in an example.
    // They run in a job of their own, beside the workspace suite, on every
    // pull request.
    let examples = block(&oss, "  rust-examples-doctests:");
    assert!(examples.contains(&"    if: github.event_name == 'pull_request'"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --examples"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --doc"));
    // No second condition can silently skip a step within this job.
    assert_eq!(
        rust.iter()
            .filter(|line| line.trim_start().starts_with("if:"))
            .count(),
        1
    );

    // The cache warm-up compiles on pushes and never runs tests, so main
    // keeps exactly one automatic full-workspace owner: Aethyme Gates.
    let warm = block(&oss, "  warm-rust-cache:");
    assert!(warm.contains(&"    if: github.event_name == 'push'"));
    assert!(warm.iter().any(|line| line.contains("--no-run")));
    assert!(
        !warm.iter().any(|line| line.contains("--profile ci")
            || line.trim() == "cargo test --locked --workspace --examples"),
        "the warm-up job must not run tests"
    );

    let gates = workflow("aethyme-gates.yml");
    assert_eq!(
        block(&gates, "on:"),
        [
            "  push:",
            "    branches: [\"main\", \"master\"]",
            "  workflow_dispatch:"
        ]
    );
    let job = block(&gates, "  gates:");
    // Unless a pull-request run already tested this exact tree, every gate
    // runs (`main_skips_only_the_gates_a_pr_run_tested_on_the_same_tree`).
    assert!(job.contains(&"            exec \"$aethyme\" broker advanced gates run --all"));
    // The job always runs; only the tree lookup is limited to pushes.
    assert!(!job.iter().any(|line| line.starts_with("    if:")));
    assert_eq!(
        job.iter()
            .filter(|line| line.trim_start().starts_with("if:"))
            .copied()
            .collect::<Vec<_>>(),
        ["        if: github.event_name == 'push'"]
    );
}

/// Main's Aethyme Gates skip a gate only when a pull-request run of this
/// repository passed both suites on a byte-identical tree, and then only the
/// gates those suites cover. Any other gate, including one added later, runs.
#[test]
fn main_skips_only_the_gates_a_pr_run_tested_on_the_same_tree() {
    let oss = workflow("oss-ci.yml");
    let record = block(&oss, "  record-tested-tree:");
    assert!(record.contains(&"    if: github.event_name == 'pull_request'"));
    assert!(record.contains(&"    needs: [rust-tests, rust-examples-doctests]"));
    assert!(record.contains(&"          tree=\"$(git rev-parse 'HEAD^{tree}')\""));
    assert!(record.contains(&"          name: tested-tree-${{ env.TESTED_TREE }}"));

    let gates = workflow("aethyme-gates.yml");
    let job = block(&gates, "  gates:");
    assert!(job.contains(&"          tree=\"$(git rev-parse 'HEAD^{tree}')\""));
    // An artifact uploaded by a fork's run never counts.
    assert!(
        job.iter()
            .any(|line| line.contains(".workflow_run.head_repository_id == $repo_id"))
    );
    assert!(job.contains(
        &"          COVERED_BY_PR_SUITE: \"cargo-test fast-guards workflow-contract gate-policy\""
    ));
    // The gates to run come from the manifest, not from a list in the workflow.
    // An assignment, so a failing manifest fails the step instead of running
    // no gate at all.
    assert!(job.iter().any(|line| {
        line.trim_start().starts_with("gates=\"$(")
            && line
                .contains("broker advanced gates manifest --json | jq -r '.manifest.gates[].name'")
    }));
    assert!(job.contains(&"          for gate in $gates; do"));
    assert!(job.contains(
        &"              *) \"$aethyme\" broker advanced gates run --all --only \"$gate\" ;;"
    ));

    // Every skipped gate must exist, and the contract gate, which reads the
    // merged PR's body, is never skipped.
    let policy = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(5)
            .unwrap()
            .join(".aethyme/gates.toml"),
    )
    .unwrap();
    for gate in [
        "cargo-test",
        "fast-guards",
        "workflow-contract",
        "gate-policy",
    ] {
        assert!(
            policy.contains(&format!("name = \"{gate}\"")),
            "{gate} is not a gate"
        );
    }
    assert!(!job.iter().any(
        |line| line.contains("COVERED_BY_PR_SUITE") && line.contains("cross-process-contract")
    ));
}

/// macOS is tested nightly, and that run must be the whole workspace: until
/// 2026-10-06 it was `cargo test --workspace --examples`, which runs example
/// targets only.
#[test]
fn macos_nightly_runs_the_whole_workspace_suite() {
    let nightly = workflow("macos-nightly.yml");
    let job = block(&nightly, "  rust-tests-macos-full:");
    assert!(job.contains(&"        run: ../scripts/test-like-ci.sh --full"));
    assert!(job.contains(&"        run: cargo test --locked --workspace --examples"));
    assert!(job.contains(&"        run: cargo test --locked --workspace --doc"));
    assert!(job.contains(&"        run: cargo build --locked --workspace --bins"));
}

#[test]
fn linux_nightly_publishes_workspace_coverage_without_a_threshold() {
    let nightly = workflow("macos-nightly.yml");
    assert_eq!(
        block(&nightly, "on:"),
        [
            "  schedule:",
            "    - cron: \"17 3 * * *\"",
            "  workflow_dispatch:"
        ]
    );

    let job = block(&nightly, "  rust-coverage-linux:");
    let job_text = job.join("\n");
    assert!(job_text.contains("runs-on: ubuntu-latest"));
    assert!(
        job_text.contains("cargo llvm-cov --locked --workspace --html --output-dir coverage/html")
    );
    assert!(job_text.contains("cargo llvm-cov report --lcov --output-path coverage/lcov.info"));
    assert!(job_text.contains("GITHUB_STEP_SUMMARY"));
    assert!(job_text.contains("workspace-total"));
    assert!(job_text.contains("$i == \"crates\""));
    assert!(job_text.contains("total[crate] += file_total"));
    assert!(job_text.contains("covered[crate] += file_covered"));
    assert!(job_text.contains("packages/aethyme/rust/coverage/lcov.info"));
    assert!(job_text.contains("packages/aethyme/rust/coverage/html/"));
    assert!(job_text.contains("actions/upload-artifact@"));
    assert!(!job_text.contains("--fail-under"));
}

#[test]
fn duplicate_binary_suite_remains_available_manually() {
    let local = workflow("aethyme-local-tests.yml");
    assert_eq!(block(&local, "on:"), ["  workflow_dispatch:"]);
    let job = block(&local, "  binary-driven-tests:");
    assert!(job.contains(&"        run: cargo test --locked -p aethyme-cli -p aethyme-testkit"));
    assert!(job.contains(&"        run: cargo build --locked --quiet --bin aethyme --bin aethyme-engine-cli --bin aethyme-graph-index"));
    assert!(!job.iter().any(|line| line.trim_start().starts_with("if:")));
}

#[test]
fn distinct_product_proof_remains_automatic_for_both_events() {
    let oss = workflow("oss-ci.yml");
    let product = block(&oss, "  product-path-no-python:");
    assert!(
        !product
            .iter()
            .any(|line| line.trim_start().starts_with("if:"))
    );
    assert!(
        product
            .iter()
            .any(|line| line.contains("aethyme deploy verify --repo ."))
    );
    assert!(
        product
            .iter()
            .any(|line| line.contains("aethyme verify-targets --repo ."))
    );
}

#[test]
fn changed_workflows_have_read_only_permissions() {
    for name in ["oss-ci.yml", "aethyme-local-tests.yml"] {
        assert_eq!(block(&workflow(name), "permissions:"), ["  contents: read"]);
    }
    // The contract gate reads the merged PR's body on main; still read-only.
    assert_eq!(
        block(&workflow("aethyme-gates.yml"), "permissions:"),
        [
            "  contents: read",
            "  actions: read",
            "  pull-requests: read"
        ]
    );
}

/// A contract decision accepted on a pull request must be accepted again on
/// its merge commit (PR #514 turned main red on 2026-10-04 because the gate
/// read only commit messages while the PR check read only the PR body).
#[test]
fn contract_check_reads_the_same_sources_on_prs_and_on_main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let policy = std::fs::read_to_string(root.join(".aethyme/gates.toml")).unwrap();
    let gate = policy
        .split("[[gate]]")
        .find(|gate| gate.contains("name = \"cross-process-contract\""))
        .unwrap();
    assert!(gate.contains("--commit-messages --merged-pr"), "{gate}");

    let pr_check = workflow("cross-process-contract.yml");
    assert!(pr_check.contains("--pr-body /tmp/pr-body.md \\\n            --commit-messages"));

    let gates = workflow("aethyme-gates.yml");
    let job = block(&gates, "  gates:");
    assert!(job.contains(&"          GH_TOKEN: ${{ github.token }}"));
}

#[test]
fn workflow_only_changes_run_this_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let policy = std::fs::read_to_string(root.join(".aethyme/gates.toml")).unwrap();
    let gate = policy
        .split("[[gate]]")
        .find(|gate| gate.contains("name = \"workflow-contract\""))
        .unwrap();
    assert!(gate.contains("--test ci_validation"));
    assert!(gate.contains("triggers = [\".github/workflows/**\"]"));
}

/// CI and a workstation run the workspace suite through one command
/// (#598). A workflow that calls `cargo nextest run` directly, or a script
/// that stops using the CI profile or starts allowing serial runs, would let
/// the two drift apart again.
#[test]
fn ci_and_local_runs_share_one_test_command() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    for entry in std::fs::read_dir(root.join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
        {
            assert!(
                !line.contains("cargo nextest run --locked --workspace --profile"),
                "{} runs nextest directly instead of test-like-ci.sh: {line}",
                path.display()
            );
        }
    }
    let script =
        std::fs::read_to_string(root.join("packages/aethyme/scripts/test-like-ci.sh")).unwrap();
    assert!(script.contains("cargo nextest run --locked --workspace --profile ci"));
    assert!(script.contains("AETHYME_TESTKIT_PREBUILT_BINS=1"));
    assert!(script.contains("--test-threads=1|--nocapture|--no-capture|-j1"));
    let nextest =
        std::fs::read_to_string(root.join("packages/aethyme/rust/.config/nextest.toml")).unwrap();
    assert!(nextest.contains("[profile.ci]"));
    assert!(nextest.contains("[profile.ci.junit]"));
}
