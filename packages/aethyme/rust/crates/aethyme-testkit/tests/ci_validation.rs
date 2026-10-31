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
    assert!(examples.contains(&"    if: ${{ !cancelled() && github.event_name == 'pull_request' && (needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));
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
        11,
        "the Rust job has one PR guard, eight full-lane guards, one shared parity guard and one performance validation guard"
    );
    assert_eq!(
        rust_conditions
            .iter()
            .filter(|line| **line == "if: ${{ needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true') }}")
            .count(),
        8,
        "seven Rust steps and the Python controls must run when classification fails or selects the full suite"
    );
    assert_eq!(
        rust_conditions
            .iter()
            .filter(|line| **line == "if: ${{ needs.classify_changes.result == 'success' && (needs.classify_changes.outputs.docs_fast == 'true' || needs.classify_changes.outputs.performance_fast == 'true') }}")
            .count(),
        1,
        "the parity step must run on either fast path"
    );

    assert!(rust_conditions.contains(&"if: ${{ needs.classify_changes.result == 'success' && needs.classify_changes.outputs.performance_fast == 'true' }}"));
    assert!(rust.contains(&"        run: bash .github/scripts/validate-performance-fast-lane.sh"));

    // The added lightweight coverage belongs to the full lane too. Pin its
    // condition and exact suites, rather than only accepting another guard.
    let rust_body = rust.join("\n");
    let controls = block(
        &rust_body,
        "      - name: Run broker performance controls in the full lane",
    );
    assert_eq!(controls.iter().filter(|line| line.trim() == "if: ${{ needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true') }}").count(), 1);
    assert_eq!(
        controls
            .iter()
            .filter(|line| line.trim() == "-s packages/aethyme-eval/benchmarks/performance \\")
            .count(),
        3
    );
    assert_eq!(
        controls
            .iter()
            .filter(|line| line.trim().starts_with("python3 -m unittest discover"))
            .count(),
        3
    );
    assert_eq!(
        controls
            .iter()
            .filter_map(|line| line.trim().strip_prefix("-p "))
            .collect::<Vec<_>>(),
        [
            "'test_broker_latency.py' -v",
            "'test_broker_concurrent_submit.py' -v",
            "'test_git_worktree_config.py' -v",
        ]
    );
    assert!(
        !controls
            .iter()
            .any(|line| line.contains("cargo") || line.contains("go run"))
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
        [
            "        if: github.event_name == 'push'",
            "        if: failure()"
        ]
    );
}

#[test]
fn main_gate_router_prebuild_reuses_the_managed_debug_target() {
    let gates = workflow("aethyme-gates.yml");
    let locate_cache = gates.find("      - name: Locate the gates").unwrap();
    let cache = gates
        .find("      - name: Cache the gates' managed build")
        .unwrap();
    let build = gates
        .find("      - name: Build aethyme in the gates' managed cache")
        .unwrap();
    let lookup = gates
        .find("      - name: Look for a pull-request run that tested this tree")
        .unwrap();
    let run = gates.find("      - name: Run all gates").unwrap();
    assert!(locate_cache < cache && cache < build && build < lookup && lookup < run);
    assert!(
        !gates.contains("Cache the aethyme release build"),
        "the gate router must not be built twice in separate targets"
    );

    let build_step = block(
        &gates,
        "      - name: Build aethyme in the gates' managed cache",
    )
    .join("\n");
    for setting in [
        "CHAU7_CTO_OPTIM_ACTIVE: \"1\"",
        "CARGO_INCREMENTAL: \"0\"",
        "CARGO_BUILD_JOBS: \"1\"",
        "CARGO_PROFILE_DEV_DEBUG: \"1\"",
        "CARGO_TARGET_DIR: ${{ env.GATE_CACHE_DIR }}",
        "run: cargo build --locked --manifest-path packages/aethyme/rust/Cargo.toml --bin aethyme",
    ] {
        assert!(
            build_step.contains(setting),
            "gate-router prebuild must match the cross-process gate profile: missing {setting}"
        );
    }
    assert!(gates[run..].contains("aethyme=\"$GATE_CACHE_DIR/debug/aethyme\""));
}

/// Failed nextest output must survive later passing doc/example output. The
/// failure-only step adds diagnostics; it never changes gate selection/results.
#[test]
fn main_prints_complete_bounded_failure_logs_without_quiet_success_noise() {
    let gates = workflow("aethyme-gates.yml");
    let run = gates.find("      - name: Run all gates").unwrap();
    let diagnostic = gates
        .find("      - name: Show complete failed gate logs")
        .unwrap();
    let cleanup = gates
        .find("      - name: Keep only third-party artifacts")
        .unwrap();
    assert!(run < diagnostic && diagnostic < cleanup);
    let diagnostic = &gates[diagnostic..cleanup];
    assert!(diagnostic.contains("        if: failure()"));
    assert!(diagnostic.contains("        shell: bash"));
    assert!(diagnostic.contains("log_dir=.aethyme/logs/gates"));
    assert!(diagnostic.contains("-type f -name '*.fail-*.log' -print0"));
    assert!(diagnostic.contains("while IFS= read -r -d '' log; do"));
    assert!(diagnostic.contains("cat -- \"$log\""));
    assert!(diagnostic.contains("::stop-commands::$token"));
    assert!(diagnostic.contains("trap 'printf"));
    assert!(diagnostic.contains("-N16 -tx1 /dev/urandom"));
    assert!(!diagnostic.contains("tail ") && !diagnostic.contains("head "));
    assert!(!diagnostic.contains("continue-on-error"));
}

#[cfg(unix)]
fn failed_gate_log_script() -> String {
    let gates = workflow("aethyme-gates.yml");
    let diagnostic = gates
        .split("      - name: Show complete failed gate logs")
        .nth(1)
        .unwrap();
    block(diagnostic, "        run: |")
        .into_iter()
        .map(|line| line.strip_prefix("          ").unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
#[test]
fn failed_gate_diagnostics_keep_an_early_failure_and_literal_log_filenames() {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join(".aethyme/logs/gates");
    std::fs::create_dir_all(&logs).unwrap();
    let marker = "EARLY_WORKSPACE_FAILURE_MUST_SURVIVE";
    let contents = format!(
        "{marker}\n::error::untrusted annotation\n::stop-commands::attacker\n{}",
        "later doctest passed\n".repeat(100)
    );
    // Exercise NUL-delimited names and quoting, rather than a whitespace loop.
    std::fs::write(
        logs.join("cargo test\n::warning::quoted 'name'.fail-1.log"),
        &contents,
    )
    .unwrap();
    std::fs::write(
        logs.join("successful-gate.log"),
        "SUCCESS_LOG_MUST_STAY_QUIET",
    )
    .unwrap();
    let output = std::process::Command::new("bash")
        .args(["-c", &failed_gate_log_script()])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&contents),
        "early failure or later context was omitted"
    );
    let mut lines = stdout.lines();
    let first = lines.next().unwrap();
    let token = first.strip_prefix("::stop-commands::").unwrap();
    assert!(token.starts_with("aethyme-"));
    assert_eq!(token.len(), "aethyme-".len() + 32);
    assert!(
        token["aethyme-".len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    );
    assert_eq!(lines.next_back().unwrap(), format!("::{token}::"));
    let payload_start = stdout.find(marker).unwrap();
    let payload_end = stdout.rfind(&format!("::{token}::")).unwrap();
    assert!(payload_start > first.len() && payload_start < payload_end);
    assert!(stdout.contains("\n::warning::quoted 'name'.fail-1.log"));
    assert!(!stdout.contains("SUCCESS_LOG_MUST_STAY_QUIET"));
}

#[cfg(unix)]
#[test]
fn failed_gate_diagnostics_report_failure_before_log_creation() {
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("bash")
        .args(["-c", &failed_gate_log_script()])
        .current_dir(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("No broker gate logs")
    );
}

/// Main's Aethyme Gates skip a gate only when a pull-request run of this
/// repository passed both suites on a byte-identical tree, and then only the
/// gates those suites cover. Any other gate, including one added later, runs.
#[test]
fn main_skips_only_the_gates_a_pr_run_tested_on_the_same_tree() {
    let oss = workflow("oss-ci.yml");
    let record = block(&oss, "  record-tested-tree:");
    assert!(record.contains(&"    if: ${{ github.event_name == 'pull_request' && needs.classify_changes.result == 'success' && needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true' }}"));
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
    assert!(job_text.contains("tool: cargo-nextest@0.9.146"));
    // The shared harness builds and shares instrumented product binaries.
    // Clear old coverage before the harness; keep retries disabled here.
    let mut remaining = job_text.as_str();
    for command in [
        "cargo llvm-cov show-env --sh",
        "eval \"$coverage_environment\"",
        "cargo llvm-cov clean --workspace",
        "../scripts/test-like-ci.sh --full -- --retries 0",
        "cargo llvm-cov report --html --output-dir coverage/html",
    ] {
        let offset = remaining
            .find(command)
            .unwrap_or_else(|| panic!("missing or out-of-order coverage command: {command}"));
        remaining = &remaining[offset + command.len()..];
    }
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
    assert!(product.contains(&"    if: ${{ !cancelled() && (github.event_name != 'pull_request' || needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));
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
    assert!(classifier.contains(&"          bash .github/scripts/test-docs-fast-lane.sh"));
    assert!(classifier.contains(&"          bash .github/scripts/test-performance-fast-lane.sh"));
    assert!(classifier.contains(&"          EVENT_NAME: ${{ github.event_name }}"));
    assert!(
        classifier.contains(&"          PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}")
    );
    assert!(classifier.contains(&"          EVENT_SHA: ${{ github.sha }}"));
    assert!(classifier.contains(&"          if [[ \"$EVENT_NAME\" != \"pull_request\" ]]; then"));
    assert!(classifier.contains(&"            echo \"docs_fast=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(classifier.contains(&"              echo \"docs_fast=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(classifier.contains(&"              exit 1"));

    assert!(
        classifier
            .contains(&"      performance_fast: ${{ steps.classify.outputs.performance_fast }}")
    );
    // Both classifiers must come from the trusted PR base. A classifier change
    // in this PR cannot grant itself permission to skip product coverage.
    for script in ["docs-fast-lane.sh", "performance-fast-lane.sh"] {
        assert!(classifier.iter().any(|line| {
            line.contains(&format!(
                "git show \"$PR_BASE_SHA:.github/scripts/{script}\""
            ))
        }));
    }
    assert!(classifier.contains(&"            true:true)"));

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
    assert!(record.contains(&"    if: ${{ github.event_name == 'pull_request' && needs.classify_changes.result == 'success' && needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true' }}"));

    let lint = block(&oss, "  lint:");
    assert!(lint.contains(&"    needs: classify_changes"));
    assert!(lint.contains(&"    if: ${{ !cancelled() && (github.event_name != 'pull_request' || needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));

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
    let contract_env = block(&pr_check, "env:");
    let main_ci = workflow("oss-ci.yml");
    let main_env = block(&main_ci, "env:");
    assert!(main_env.contains(&"  CARGO_NET_RETRY: \"10\""));
    assert!(
        contract_env.contains(&"  CARGO_NET_RETRY: \"10\""),
        "the contract workflow must match the trusted cache warmer's environment fingerprint"
    );
    let contract_cache = block(&pr_check, "      - name: Cache Rust build");
    assert!(contract_cache.contains(&"          shared-key: rust-tests"));
    assert!(
        contract_cache.contains(&"          save-if: false"),
        "PR checks may restore the main cache but must not write shared compiler artifacts"
    );
    assert!(
        pr_check.contains(
            "if: github.event.action != 'edited' || github.event.changes.body || github.event.changes.base"
        ),
        "a retargeted PR is judged on a new diff, so a base change must rerun the check"
    );
    assert!(pr_check.contains("fetch-depth: 0"));
    assert!(pr_check.contains("ref: ${{ github.event.pull_request.head.sha }}"));
    assert!(pr_check.contains("BASE_REF: ${{ github.base_ref }}"));
    assert!(pr_check.contains("PR_HEAD_SHA: ${{ github.event.pull_request.head.sha }}"));
    assert!(pr_check.contains("git merge-base \"origin/${BASE_REF}\" \"${PR_HEAD_SHA}\""));
    assert!(pr_check.contains("--base \"$CONTRACT_BASE\""));
    let pr_check_step = pr_check
        .split("      - name: Run cross-process contract check")
        .nth(1)
        .unwrap();
    let run_script = block(pr_check_step, "        run: |").join("\n");
    assert!(
        !run_script.contains("${{"),
        "GitHub context values must enter the shell through env:"
    );
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

#[test]
fn local_nextest_profile_kills_hung_tests_after_a_finite_deadline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let source =
        std::fs::read_to_string(root.join("packages/aethyme/rust/.config/nextest.toml")).unwrap();
    let config: toml::Value = toml::from_str(&source).unwrap();
    let profiles = config["profile"].as_table().unwrap();

    let local = &profiles["local"];
    assert_eq!(local["test-threads"].as_integer(), Some(2));
    assert_eq!(local["retries"].as_integer(), Some(0));
    assert_eq!(local["fail-fast"].as_bool(), Some(true));
    assert_eq!(local["success-output"].as_str(), Some("never"));
    assert_eq!(local["failure-output"].as_str(), Some("final"));
    assert_eq!(local["slow-timeout"]["period"].as_str(), Some("60s"));
    assert_eq!(
        local["slow-timeout"]["terminate-after"].as_integer(),
        Some(5)
    );

    // Keep the CI profile's existing per-test bound while fixing the local
    // profile selected by the main gate.
    let ci = &profiles["ci"];
    assert_eq!(ci["retries"].as_integer(), Some(1));
    assert_eq!(ci["slow-timeout"]["period"].as_str(), Some("60s"));
    assert_eq!(ci["slow-timeout"]["terminate-after"].as_integer(), Some(5));
}
