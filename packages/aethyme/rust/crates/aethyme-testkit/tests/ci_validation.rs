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
    assert!(rust.contains(&"    if: ${{ !cancelled() && github.event_name == 'pull_request' }}"));
    assert!(rust.contains(&"    needs: classify_changes"));
    // The workspace suite runs under nextest. `cargo test --workspace
    // --examples` once stood in for it, but a target flag narrows cargo's
    // selection to examples only, so it ran three binaries.
    assert!(rust.contains(&"        run: ../scripts/test-like-ci.sh --full"));
    // nextest runs neither example targets nor doctests, and the
    // release-manifest assertions in
    // `crates/aethyme-broker/examples/release_manifest.rs` live in an example.
    // They run in a job of their own, beside the workspace suite, on every
    // full-validation pull request. Docs-fast pull requests run only parity.
    let examples = block(&oss, "  rust-examples-doctests:");
    assert!(examples.contains(&"    needs: classify_changes"));
    assert!(examples.contains(&"    if: ${{ !cancelled() && github.event_name == 'pull_request' && (needs.classify_changes.result != 'success' || needs.classify_changes.outputs.docs_fast != 'true') }}"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --examples"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --doc"));
    // The job-level event guard and the full-suite/parity step guards are
    // explicit. Unknown classifier results must select the full suite.
    let rust_conditions = rust
        .iter()
        .filter(|line| line.trim_start().starts_with("if:"))
        .map(|line| line.trim())
        .collect::<Vec<_>>();
    assert_eq!(
        rust_conditions.len(),
        8,
        "the Rust job has one PR guard, six full-suite guards, and one docs-fast parity guard"
    );
    assert_eq!(
        rust_conditions
            .iter()
            .filter(|line| **line == "if: ${{ needs.classify_changes.result != 'success' || needs.classify_changes.outputs.docs_fast != 'true' }}")
            .count(),
        6,
        "all expensive Rust steps must run when classification fails or selects the full suite"
    );
    assert_eq!(
        rust_conditions
            .iter()
            .filter(|line| **line == "if: ${{ needs.classify_changes.result == 'success' && needs.classify_changes.outputs.docs_fast == 'true' }}")
            .count(),
        1,
        "only the mirrored parity step may run on the docs-fast path"
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
    assert!(record.contains(&"    if: ${{ github.event_name == 'pull_request' && needs.classify_changes.result == 'success' && needs.classify_changes.outputs.docs_fast != 'true' }}"));
    assert!(record.contains(&"    needs: [classify_changes, rust-tests, rust-examples-doctests]"));
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
fn product_proof_runs_on_main_and_falls_back_for_uncertain_or_full_prs() {
    let oss = workflow("oss-ci.yml");
    let product = block(&oss, "  product-path-no-python:");
    assert!(product.contains(&"    needs: classify_changes"));
    assert!(product.contains(&"    if: ${{ !cancelled() && (github.event_name != 'pull_request' || needs.classify_changes.result != 'success' || needs.classify_changes.outputs.docs_fast != 'true') }}"));
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
fn docs_fast_lane_is_fail_closed_and_never_records_full_suite_coverage() {
    let oss = workflow("oss-ci.yml");
    let classifier = block(&oss, "  classify_changes:");
    assert!(classifier.contains(&"    outputs:"));
    assert!(classifier.contains(&"      docs_fast: ${{ steps.classify.outputs.docs_fast }}"));
    assert!(classifier.contains(&"      - name: Verify classifier controls"));
    assert!(classifier.contains(&"        run: bash .github/scripts/test-docs-fast-lane.sh"));
    assert!(classifier.contains(&"          EVENT_NAME: ${{ github.event_name }}"));
    assert!(
        classifier.contains(&"          PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}")
    );
    assert!(classifier.contains(&"          EVENT_SHA: ${{ github.sha }}"));
    assert!(classifier.contains(&"          if [[ \"$EVENT_NAME\" != \"pull_request\" ]]; then"));
    assert!(classifier.contains(&"            echo \"docs_fast=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(classifier.contains(&"              echo \"docs_fast=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(classifier.contains(&"              exit 1"));

    let rust = block(&oss, "  rust-tests:");
    assert!(rust.contains(&"      - name: Run mirrored skill parity tests without Cargo"));
    assert!(rust.iter().any(|line| {
        line.contains("rustc --edition=2021 --test .github/scripts/skill-parity-harness.rs")
    }));
    assert!(rust.contains(&"      - name: Run workspace tests"));
    assert!(rust.contains(&"        run: ../scripts/test-like-ci.sh --full"));

    let examples = block(&oss, "  rust-examples-doctests:");
    assert!(examples.contains(&"      - name: Run example tests"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --examples"));
    assert!(examples.contains(&"      - name: Run doctests"));
    assert!(examples.contains(&"        run: cargo test --locked --workspace --doc"));

    let record = block(&oss, "  record-tested-tree:");
    assert!(record.contains(&"    needs: [classify_changes, rust-tests, rust-examples-doctests]"));
    assert!(record.contains(&"    if: ${{ github.event_name == 'pull_request' && needs.classify_changes.result == 'success' && needs.classify_changes.outputs.docs_fast != 'true' }}"));

    let lint = block(&oss, "  lint:");
    assert!(lint.contains(&"    needs: classify_changes"));
    assert!(lint.contains(&"    if: ${{ !cancelled() && (github.event_name != 'pull_request' || needs.classify_changes.result != 'success' || needs.classify_changes.outputs.docs_fast != 'true') }}"));

    // These controls keep workflow classification tied to the shell
    // classifier's own fail-closed path cases. The required product proof and
    // full-suite checks run whenever classification is unavailable or false.
    let classifier_script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(5)
            .unwrap()
            .join(".github/scripts/docs-fast-lane.sh"),
    )
    .unwrap();
    assert!(classifier_script.contains("select_full_suite"));
    assert!(classifier_script.contains("printf 'false\\n'"));
    assert!(classifier_script.contains("records > 0"));
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

/// CI and local commands share coverage selection while using profiles
/// that fit their jobs. Workflows still require the explicit full CI profile;
/// local defaults stay bounded even when changed paths require workspace
/// coverage.
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
    assert!(script.contains("cargo nextest run --locked"));
    assert!(script.contains("--profile ci"));
    assert!(script.contains("test_command=(cargo nextest run --locked"));
    assert!(script.contains("--no-tests fail"));
    assert!(script.contains("--profile local"));
    assert!(script.contains("selection=(-p \"$package\" --lib)"));
    assert!(
        script.contains("cargo build --locked --workspace --bins --jobs \"$local_build_jobs\"")
    );
    assert!(script.contains("export CARGO_BUILD_JOBS=\"$local_build_jobs\""));
    assert!(script.contains("coverage=workspace"));
    assert!(script.contains("Cargo.toml|Cargo.lock|.cargo/*"));
    assert!(script.contains("coverage=focused"));
    assert!(script.contains("AETHYME_TESTKIT_PREBUILT_BINS=1"));
    assert!(script.contains("unset AETHYME_TESTKIT_PREBUILT_BINS"));
    assert!(!script.contains("--test-threads=1|--nocapture|--no-capture|-j1"));
    assert!(script.contains("|| status=$?"));
    assert!(script.contains("exit \"$status\""));
    assert!(script.contains("unset AETHYME_WORKTREE_ROOT AETHYME_HOST_STATE_DIR XDG_STATE_HOME"));

    let nextest =
        std::fs::read_to_string(root.join("packages/aethyme/rust/.config/nextest.toml")).unwrap();
    assert!(nextest.contains("[profile.local]"));
    assert!(nextest.contains("test-threads = 2"));
    assert!(nextest.contains("retries = 0"));
    assert!(nextest.contains("fail-fast = true"));
    assert!(nextest.contains("failure-output = \"final\""));
    assert!(nextest.contains("status-level = \"fail\""));
    assert!(nextest.contains("[profile.ci]"));
    assert!(nextest.contains("[profile.ci.junit]"));
}
