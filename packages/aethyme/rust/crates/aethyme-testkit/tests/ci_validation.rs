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

/// CI runs on merged main. The contract check is the one exception, because its
/// verdict is only useful before the merge.
#[test]
fn pull_requests_trigger_only_the_contract_check() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let mut triggered = Vec::new();
    for entry in std::fs::read_dir(root.join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let on = block(&text, "on:");
        if on.iter().any(|line| {
            line.trim_start().starts_with("pull_request")
                || line.trim_start().starts_with("merge_group")
        }) {
            triggered.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    assert_eq!(triggered, ["cross-process-contract.yml"]);

    // Each workflow that left the pull-request lane can still be run by hand on
    // a branch, and on main. The tap's dispatch publishes, so its branch-free
    // rehearsal stays on push.
    for name in ["oss-ci.yml", "windows-release.yml", "release-targets.yml"] {
        let text = workflow(name);
        let on = block(&text, "on:");
        assert!(on.contains(&"  workflow_dispatch:"), "{name}");
        assert!(on.contains(&"  push:"), "{name}");
    }
    let tap = workflow("homebrew-tap.yml");
    assert!(block(&tap, "on:").contains(&"  push:"));
    // A release calls the tap workflow from a tag push, so the event name cannot
    // tell a rehearsal from a publication; only the absent tag can.
    assert!(tap.contains("if [ -z \"$INPUT_TAG\" ]; then"));
    assert!(tap.contains("            mode=rehearse"));
    assert!(!tap.contains("\"$EVENT_NAME\" = push"));
    assert!(!tap.contains("github.event_name == 'push'"));
    assert!(workflow("release.yml").contains("uses: ./.github/workflows/homebrew-tap.yml"));

    // Main runs what the pull-request lane used to be the only run of.
    let oss = workflow("oss-ci.yml");
    let controls = block(&oss, "  broker-performance-controls:");
    assert!(
        !controls
            .iter()
            .any(|line| line.contains("github.event_name"))
    );
}

#[test]
fn main_has_one_automatic_full_workspace_owner() {
    let oss = workflow("oss-ci.yml");
    assert_eq!(
        block(&oss, "on:"),
        [
            "  push:",
            "    branches: [\"main\", \"master\"]",
            "  workflow_dispatch:"
        ]
    );
    // `rust-tests` keeps the required check name. The suite itself is built
    // once into a nextest archive and run in ten hash partitions; the verdict
    // job requires the build, every shard and the performance controls.
    let rust = block(&oss, "  rust-tests:");
    assert!(rust.contains(&"    if: ${{ !cancelled() && github.event_name != 'push' }}"));
    assert!(rust.contains(&"    needs: [classify_changes, rust-tests-build, rust-tests-shard, broker-performance-controls]"));
    let build = block(&oss, "  rust-tests-build:");
    assert!(build.contains(&"    needs: classify_changes"));
    assert!(build.contains(&"    if: ${{ !cancelled() && github.event_name != 'push' && (needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));
    // The workspace suite runs under nextest. `cargo test --workspace
    // --examples` once stood in for it, but a target flag narrows cargo's
    // selection to examples only, so it ran three binaries.
    assert!(build.contains(
        &"        run: ../scripts/test-like-ci.sh --archive \"$RUNNER_TEMP/rust-tests.tar.zst\""
    ));
    assert!(
        !build
            .iter()
            .any(|line| line.contains("cargo build --locked --workspace --bins")),
        "the test build already produces the product binaries"
    );
    let shard = block(&oss, "  rust-tests-shard:");
    assert!(shard.contains(&"    needs: [classify_changes, rust-tests-build]"));
    assert!(
        shard
            .contains(&"    if: ${{ !cancelled() && needs.rust-tests-build.result == 'success' }}")
    );
    assert!(shard.contains(&"      fail-fast: false"));
    assert!(shard.contains(&"        shard: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]"));
    assert!(shard.contains(&"        run: ../scripts/test-like-ci.sh --from-archive \"$RUNNER_TEMP/rust-tests-archive/rust-tests.tar.zst\" --partition \"hash:${{ matrix.shard }}/10\""));
    assert!(shard.contains(&"          path: packages/aethyme/rust/target/nextest/ci/junit.xml"));
    // nextest runs neither example targets nor doctests, and the
    // release-manifest assertions in
    // `crates/aethyme-broker/examples/release_manifest.rs` live in an example.
    // They run in a job of their own, beside the workspace suite, whenever the
    // suite is dispatched.
    let examples = block(&oss, "  rust-examples-doctests:");
    assert!(examples.contains(&"    needs: classify_changes"));
    assert!(examples.contains(&"    if: ${{ !cancelled() && github.event_name != 'push' && (needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));
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
        rust_conditions,
        [
            "if: ${{ !cancelled() && github.event_name != 'push' }}",
            "if: ${{ needs.classify_changes.result == 'success' && (needs.classify_changes.outputs.docs_fast == 'true' || needs.classify_changes.outputs.performance_fast == 'true') }}",
            "if: ${{ needs.classify_changes.result == 'success' && (needs.classify_changes.outputs.docs_fast == 'true' || needs.classify_changes.outputs.performance_fast == 'true') }}",
            "if: ${{ needs.classify_changes.result == 'success' && needs.classify_changes.outputs.performance_fast == 'true' }}",
            "if: ${{ needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true') }}",
            "if: ${{ always() && needs.rust-tests-build.result == 'success' }}",
            "if: ${{ always() && needs.rust-tests-build.result == 'success' }}",
            "if: ${{ always() && needs.rust-tests-build.result == 'success' }}",
            "if: ${{ always() && needs.rust-tests-build.result == 'success' }}",
        ],
        "Not on push; parity toolchain and parity on either fast path; performance validation; the full-suite verdict; four telemetry steps"
    );
    assert!(rust.contains(&"        run: bash .github/scripts/validate-performance-fast-lane.sh"));
    // A skipped or failed build, shard or control job fails the verdict.
    assert!(rust.contains(&"          [[ \"$BUILD\" == success && \"$SHARDS\" == success && \"$CONTROLS\" == success ]]"));
    assert!(rust.contains(&"          BUILD: ${{ needs.rust-tests-build.result }}"));
    assert!(rust.contains(&"          SHARDS: ${{ needs.rust-tests-shard.result }}"));
    assert!(rust.contains(&"          CONTROLS: ${{ needs.broker-performance-controls.result }}"));

    // The lightweight Python coverage belongs to the full lane too, in a job
    // of its own beside the build. Pin its condition and exact suites.
    let controls = block(&oss, "  broker-performance-controls:");
    assert!(controls.contains(&"    needs: classify_changes"));
    assert!(controls.contains(&"    if: ${{ !cancelled() && (needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')) }}"));
    assert_eq!(
        controls
            .iter()
            .filter(|line| line.trim() == "-s packages/aethyme-eval/benchmarks/performance \\")
            .count(),
        4
    );
    assert_eq!(
        controls
            .iter()
            .filter(|line| line.trim().starts_with("python3 -m unittest discover"))
            .count(),
        4
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
            "'test_git_worktree_options.py' -v",
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
    // runs (`main_skips_only_the_gates_a_pr_run_tested_on_the_same_tree`), and
    // cargo-test runs in shards
    // (`the_cargo_test_jobs_run_the_pull_request_suite`). The job always runs;
    // only the tree lookup is limited to pushes.
    assert!(!job.iter().any(|line| line.starts_with("    if:")));
    assert!(block(&gates, "  tested-tree:").contains(&"        if: github.event_name == 'push'"));
    assert_eq!(
        job.iter()
            .filter(|line| line.trim_start().starts_with("if:"))
            .copied()
            .collect::<Vec<_>>(),
        [
            "        if: failure()",
            "        if: ${{ !cancelled() }}",
            "        if: ${{ !cancelled() }}"
        ]
    );
}

/// The cargo-test gate timed out on main with the runner almost idle, and a gate
/// refused to start on a runner short of disk. The workflow owns both answers.
#[test]
fn main_gates_use_the_whole_runner_and_keep_their_storage_reserve() {
    let gates = workflow("aethyme-gates.yml");
    let run = block(&gates, "      - name: Run all gates");
    assert!(run.contains(&"          AETHYME_GATE_BUILD_JOBS: \"4\""));
    assert!(run.contains(&"          AETHYME_GATE_TEST_THREADS: \"4\""));

    // Passing gates print no detail, so the workflow records it. A summary
    // problem must not change the verdict the gates reached.
    let summary = block(&gates, "      - name: Summarize the gates");
    assert!(summary.contains(&"          set -uo pipefail"));
    assert!(
        summary.contains(
            &"            echo \"::warning::The gate telemetry could not be summarized.\""
        )
    );
    assert!(gates.contains("      - name: Upload the gate telemetry"));

    let job = &gates[gates.find("\n  gates:\n").unwrap()..];
    let free = job
        .find("      - name: Free runner disk when it is low")
        .unwrap();
    assert!(free < job.find("      - name: Checkout").unwrap());
    let step = block(&gates, "      - name: Free runner disk when it is low");
    assert!(step.contains(&"          if [ \"$free\" -lt 30 ]; then"));
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
    let run = gates.find("      - name: Run all gates").unwrap();
    assert!(locate_cache < cache && cache < build && build < run);
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
        "CARGO_BUILD_JOBS: \"4\"",
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

/// Pull requests run no suite, so this skip finds nothing today; it stays for a
/// restored trigger. Main's Aethyme Gates skip a gate only when a pull-request run of this
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
    let lookup = block(&gates, "  tested-tree:");
    assert!(lookup.contains(&"          tree=\"$(git rev-parse 'HEAD^{tree}')\""));
    // An artifact uploaded by a fork's run never counts.
    assert!(
        lookup
            .iter()
            .any(|line| line.contains(".workflow_run.head_repository_id == $repo_id"))
    );
    let job = block(&gates, "  gates:");
    assert!(job.contains(&"          TESTED_BY_RUN: ${{ needs.tested-tree.outputs.run }}"));
    assert!(job.contains(
        &"          COVERED_BY_WORKSPACE_SUITE: \"cargo-test fast-guards workflow-contract gate-policy\""
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
    assert!(
        job.contains(
            &"              \"$aethyme\" broker advanced gates run --all --only \"$gate\""
        )
    );

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
    assert!(
        !job.iter()
            .any(|line| line.contains("COVERED_BY_WORKSPACE_SUITE")
                && line.contains("cross-process-contract"))
    );
}

/// On main the cargo-test gate runs as jobs, because one runner spent 19m49 on
/// it with its CPUs saturated. A tree the pull-request suite tested already
/// counts as covered, so these jobs must be that suite: same commands, every
/// partition, and a verdict that fails when any part fails or is skipped. A
/// gate other than cargo-test still runs in the gates job.
#[test]
fn every_cargo_test_keeps_fixtures_out_of_the_hosts_worktree_root() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap();
    assert!(config.contains("AETHYME_TEST_CONTAIN_WORKTREES = \"1\""));
}

#[test]
fn test_shards_reuse_the_build_jobs_verified_nextest() {
    for (file, build_name, shard_name) in [
        ("oss-ci.yml", "  rust-tests-build:", "  rust-tests-shard:"),
        (
            "aethyme-gates.yml",
            "  cargo-test-build:",
            "  cargo-test-shard:",
        ),
    ] {
        let text = workflow(file);
        let build = block(&text, build_name).join("\n");
        let shard = block(&text, shard_name).join("\n");
        assert!(build.contains("timeout-minutes: 3"), "{file}");
        assert!(
            build.contains("steps.nextest_install.outcome == 'failure'"),
            "{file}"
        );
        assert!(build.contains("name: rust-tests-nextest"), "{file}");
        assert!(
            build.contains("sha256sum cargo-nextest > SHA256SUMS"),
            "{file}"
        );
        assert!(shard.contains("name: rust-tests-nextest"), "{file}");
        assert!(shard.contains("sha256sum --check SHA256SUMS"), "{file}");
        assert!(!shard.contains("taiki-e/install-action@"), "{file}");
    }
}

#[test]
fn the_cargo_test_jobs_run_the_pull_request_suite() {
    let run_lines = |text: &str, job: &str| -> Vec<String> {
        block(text, job)
            .iter()
            .filter(|line| line.trim_start().starts_with("run:"))
            .map(|line| line.trim().to_owned())
            .collect()
    };
    let oss = workflow("oss-ci.yml");
    let gates = workflow("aethyme-gates.yml");

    let oss_build = run_lines(&oss, "  rust-tests-build:");
    assert!(oss_build.iter().any(|line| line.contains("--archive")));
    assert_eq!(
        run_lines(&gates, "  cargo-test-build:")
            .iter()
            .filter(|line| !line.starts_with("run: |"))
            .collect::<Vec<_>>(),
        oss_build
            .iter()
            .filter(|line| !line.starts_with("run: |") && !line.contains("cargo fmt"))
            .collect::<Vec<_>>(),
    );
    let examples = run_lines(&gates, "  cargo-test-examples-doctests:");
    assert_eq!(
        examples.iter().collect::<Vec<_>>(),
        run_lines(&oss, "  rust-examples-doctests:")
            .iter()
            .filter(|line| !line.contains("cargo fmt"))
            .collect::<Vec<_>>(),
    );

    // The matrix must name every partition its command divides the suite into.
    let shard = block(&gates, "  cargo-test-shard:");
    assert!(shard.contains(&"    needs: [tested-tree, cargo-test-build]"));
    assert!(shard.contains(&"      fail-fast: false"));
    assert!(shard.contains(&"        shard: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]"));
    // The pull-request command, and the same limited to the push scope's
    // selection (`main_tests_the_changed_scope_and_records_only_full_suites`).
    let oss_shard: Vec<String> = run_lines(&oss, "  rust-tests-shard:")
        .into_iter()
        .filter(|line| line.contains("--from-archive"))
        .collect();
    assert_eq!(oss_shard.len(), 1);
    let command = oss_shard[0]
        .strip_prefix("run: ")
        .unwrap()
        .replace("/10\"", "/12\"");
    let shard_lines: Vec<&str> = shard.iter().map(|line| line.trim()).collect();
    assert!(shard_lines.contains(&command.as_str()), "{command}");
    let selected = format!("{command} --selection \"$SELECTION\"");
    assert!(shard_lines.contains(&selected.as_str()), "{selected}");

    // Skipped when a pull-request run tested the tree or nothing changed.
    for job in ["  cargo-test-build:", "  cargo-test-examples-doctests:"] {
        assert!(
            block(&gates, job).contains(&"    if: ${{ needs.tested-tree.outputs.run == '' && needs.tested-tree.outputs.scope != 'none' }}"),
            "{job}"
        );
    }
    let verdict = block(&gates, "  cargo-test:");
    assert!(verdict.contains(&"    needs: [tested-tree, cargo-test-build, cargo-test-shard, cargo-test-examples-doctests]"));
    assert!(verdict.contains(&"    if: ${{ !cancelled() && needs.tested-tree.result == 'success' && needs.tested-tree.outputs.run == '' }}"));
    assert!(verdict.contains(&"            full|changed) [[ \"$BUILD\" == success && \"$SHARDS\" == success && \"$EXAMPLES\" == success ]] ;;"));

    // The gates job leaves out only the gates this suite runs in full, and
    // each one's test targets must be in that suite: none selects a package or
    // feature the workspace run would miss.
    let job = block(&gates, "  gates:");
    assert!(job.contains(
        &"            if [[ \" $COVERED_BY_WORKSPACE_SUITE \" != *\" $gate \"* ]]; then"
    ));
    let policy: toml::Value = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(5)
            .unwrap()
            .join(".aethyme/gates.toml"),
    )
    .unwrap()
    .parse()
    .unwrap();
    for name in ["fast-guards", "workflow-contract", "gate-policy"] {
        let command = policy["gate"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gate| gate["name"].as_str() == Some(name))
            .and_then(|gate| gate["command"].as_str())
            .unwrap();
        assert!(command.contains("cargo test --locked"), "{name}: {command}");
        // `--profile local` names fast-guards' nextest profile (retries and
        // workers), not a Cargo build profile.
        let command = command.replace("--profile local", "");
        for excluded in [
            "--exclude",
            "--features",
            "--all-features",
            "--release",
            "--profile",
        ] {
            assert!(
                !command.contains(excluded),
                "{name} runs more than the suite: {command}"
            );
        }
    }

    // A required check matches by name, so a manual dispatch here must not be
    // able to satisfy one on the same commit.
    for name in block(&gates, "jobs:")
        .iter()
        .filter_map(|line| line.strip_prefix("    name: "))
    {
        assert!(
            ![
                "Rust lint (fmt, clippy)",
                "Rust workspace tests",
                "Rust examples and doctests",
                "Cross-process contract check",
            ]
            .contains(&name),
            "{name} is a required check's name"
        );
    }
}

/// The removal has never run on GitHub: every runner so far had 86 GiB free. Run
/// the step's own script against stand-ins for `df` and `sudo`.
#[cfg(unix)]
#[test]
fn the_disk_step_removes_toolchains_only_below_its_threshold() {
    use std::os::unix::fs::PermissionsExt;

    let gates = workflow("aethyme-gates.yml");
    let step = gates
        .split("      - name: Free runner disk when it is low")
        .nth(1)
        .unwrap();
    let script = block(step, "        run: |")
        .into_iter()
        .map(|line| line.strip_prefix("          ").unwrap())
        .collect::<Vec<_>>()
        .join("\n");

    let run = |free: &str| {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let log = root.path().join("sudo.log");
        for (name, body) in [
            ("df", format!("#!/bin/sh\nprintf 'Avail\\n {free}G\\n'\n")),
            (
                "sudo",
                format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = std::process::Command::new("bash")
            .args(["-c", &script])
            .env("PATH", path)
            .env("GITHUB_WORKSPACE", root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        std::fs::read_to_string(&log).unwrap_or_default()
    };

    assert_eq!(run("86"), "");
    assert_eq!(run("30"), "");
    let removed = run("29");
    for path in ["/usr/local/lib/android", "/opt/ghc", "/usr/share/dotnet"] {
        assert!(removed.contains(path), "{removed}");
    }
    assert!(removed.starts_with("rm -rf "), "{removed}");
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
    let shard = block(&oss, "  rust-tests-shard:");
    assert!(shard.contains(&"      - name: Run workspace tests"));

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

/// `--print-selection` reports what `--changed` would test, which is what the
/// broker's local cargo-test gate runs. Drive the real script against a
/// throwaway repository so the selection is checked, not its spelling.
#[test]
#[cfg(unix)]
fn print_selection_reports_the_changed_selection_workspace_or_none() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let script = root.join("packages/aethyme/scripts/test-like-ci.sh");
    let fixture = tempfile::tempdir().unwrap();
    let repo = fixture.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    };
    let writes = std::cell::Cell::new(0);
    let write = |path: &str| {
        writes.set(writes.get() + 1);
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("revision {}\n", writes.get())).unwrap();
    };
    git(&["init", "-q", "-b", "main"]);
    write("packages/aethyme/rust/crates/aethyme-graph-schema/src/lib.rs");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "base"]);
    let select = |base: &str| {
        let output = std::process::Command::new("bash")
            .arg(&script)
            .args(["--base", base, "--print-selection"])
            .current_dir(repo)
            .env_remove("CI")
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };
    let repository_files = "package(aethyme-testkit) | package(aethyme-cli) | binary_id(aethyme-broker::gate_policy) | (binary_id(aethyme-broker::it) & test(/^(certify_scaffold|config_formats|plugin_floor|review_routing)::/))";

    assert_eq!(select("base"), (Some(0), "none\n".to_string()));

    write("packages/aethyme/rust/crates/aethyme-graph-schema/src/lib.rs");
    git(&["commit", "-qam", "crate"]);
    assert_eq!(
        select("base"),
        (Some(0), "rdeps(aethyme-graph-schema)\n".to_string())
    );

    // Uncommitted and untracked repository files count too, once each, in
    // sorted path order.
    write("packages/aethyme/docs/guide.md");
    write("packages/aethyme/skills/aethyme/SKILL.md");
    assert_eq!(
        select("base"),
        (
            Some(0),
            format!("{repository_files} | rdeps(aethyme-graph-schema)\n")
        )
    );

    write("packages/aethyme/rust/Cargo.lock");
    assert_eq!(select("base"), (Some(0), "workspace\n".to_string()));

    let (status, stdout) = select("no-such-ref");
    assert_eq!(status, Some(2));
    assert_eq!(stdout, "");
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
    // CI splits --full across jobs: one archive build, then partitions of it
    // extracted over the same tree, so baked binary paths stay valid.
    assert!(script.contains(
        "cargo nextest archive --locked --workspace --archive-file \"$archive_out\" --timings"
    ));
    assert!(script.contains(
        "--archive-file \"$archive_in\" --extract-to \"$rust\" --extract-overwrite --workspace-remap \"$rust\" --profile ci --no-tests fail"
    ));
    assert!(script.contains("test_command+=(--partition \"$partition\")"));
    // The full-coverage paths take product binaries from the test build; only
    // the bounded local profile still warms them separately.
    assert!(!script.contains("    cargo build --locked --workspace --bins\n"));
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

/// The pre-push tier runs the always-run suites, then the test modules the
/// change touches, cheapest first, within the budget; what the budget leaves
/// out is named for the push tier. A module with no recorded time is new and
/// always runs.
#[test]
fn prepush_selection_adds_touched_modules_cheapest_first_within_the_budget() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let script = root.join("packages/aethyme/scripts/prepush-selection.sh");
    let fixture = tempfile::tempdir().unwrap();
    let repo = fixture.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    };
    let write = |path: &str| {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{}\n", path.display())).unwrap();
    };
    let data = repo.join("data.json");
    std::fs::write(
        &data,
        r#"{
  "budget_seconds": 10,
  "always_binaries": ["k::lint"],
  "always_modules": [{"binary": "c", "module": "hot", "failing_runs": 9}],
  "modules": {"k::lint a": 1, "c hot": 1, "c small": 2, "c big": 20, "c::it suite": 3, "c::own x": 1}
}"#,
    )
    .unwrap();
    git(&["init", "-q", "-b", "main"]);
    write("README.md");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "base"]);
    let select = |base: &str| {
        let output = std::process::Command::new("bash")
            .arg(&script)
            .args(["--base", base, "--data"])
            .arg(&data)
            .current_dir(repo)
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let always = "binary_id(k::lint) | (binary_id(c) & test(/^hot(::|$)/))";

    // No change: the always-run suites alone.
    let (status, stdout, _) = select("base");
    assert_eq!(status, Some(0));
    assert_eq!(stdout, format!("{always}\n"));

    for path in [
        "src/small.rs",
        "src/big/mod.rs",
        "src/hot.rs",
        "src/lib.rs",
        "src/newmod.rs",
        "tests/suite.rs",
        "tests/own.rs",
        "tests/common/mod.rs",
    ] {
        write(&format!("packages/aethyme/rust/crates/c/{path}"));
    }
    write("docs/guide.md");
    let (status, stdout, stderr) = select("base");
    assert_eq!(status, Some(0), "{stderr}");
    // 2 s always-run + 0 (new) + 1 (own binary) + 2 (small) + 3 (suite) = 8 of
    // 10; big (20 s) does not fit. The crate root, shared test support and the
    // doc select nothing of their own, and hot already runs.
    assert_eq!(
        stdout,
        format!(
            "{always} | (binary_id(c) & test(/^newmod(::|$)/)) | binary_id(c::own) | (binary_id(c) & test(/^small(::|$)/)) | (binary_id(c::it) & test(/^suite(::|$)/))\n"
        )
    );
    assert!(
        stderr.contains("left to the push tier: c big (20 s)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("selected 8 s of a 10 s budget, 4 changed module(s)"),
        "{stderr}"
    );

    // Without a merge base the always-run suites still run.
    let (status, stdout, stderr) = select("no-such-ref");
    assert_eq!(status, Some(0));
    assert_eq!(stdout, format!("{always}\n"));
    assert!(stderr.contains("no merge base"), "{stderr}");
}

/// A push to main tests the changed crates, their reverse dependencies and the
/// guard suites; a version change, a repository-wide input or a dispatch runs
/// the whole workspace, and only such a run records the `full-suite` artifact
/// a release tag requires.
#[test]
fn main_tests_the_changed_scope_and_records_only_full_suites() {
    let gates = workflow("aethyme-gates.yml");
    let tree = block(&gates, "  tested-tree:");
    assert!(tree.contains(&"      scope: ${{ steps.scope.outputs.scope }}"));
    assert!(tree.contains(&"      selection: ${{ steps.scope.outputs.selection }}"));
    assert!(tree.contains(&"          fetch-depth: 2"));
    assert!(tree.iter().any(|line| line.contains(
        "changed=\"$(packages/aethyme/scripts/test-like-ci.sh --base HEAD^1 --print-selection)\""
    )));
    assert!(tree.iter().any(|line| line.contains(
        "selection=\"($changed) | package(aethyme-testkit) | package(aethyme-cli) | binary_id(aethyme-broker::gate_policy)\""
    )));
    assert!(tree.contains(&"          scope=full"));
    assert!(tree.contains(&"          if [ \"$EVENT_NAME\" = push ] && git rev-parse --verify --quiet HEAD^1 >/dev/null; then"));
    assert!(
        tree.contains(&"            if [ \"$(version HEAD)\" != \"$(version HEAD^1)\" ]; then")
    );

    let shard = block(&gates, "  cargo-test-shard:");
    assert!(shard.contains(&"    needs: [tested-tree, cargo-test-build]"));
    assert!(
        shard
            .iter()
            .any(|line| line.contains("--selection \"$SELECTION\""))
    );
    for job in ["  cargo-test-build:", "  cargo-test-examples-doctests:"] {
        assert!(
            block(&gates, job)
                .contains(&"    if: ${{ needs.tested-tree.outputs.run == '' && needs.tested-tree.outputs.scope != 'none' }}"),
            "{job}"
        );
    }
    let verdict = block(&gates, "  cargo-test:");
    assert!(verdict.contains(&"            full|changed) [[ \"$BUILD\" == success && \"$SHARDS\" == success && \"$EXAMPLES\" == success ]] ;;"));
    assert_eq!(
        verdict
            .iter()
            .filter(|line| line.trim() == "if: ${{ needs.tested-tree.outputs.scope == 'full' }}")
            .count(),
        2,
        "only a full scope records the full-suite artifact"
    );
    assert!(verdict.contains(&"          name: full-suite"));
}

/// Every always-run entry names timed tests, so a renamed suite cannot drop
/// out of the pre-push tier unnoticed, and the guard suites the `fast-guards`
/// fallback runs stay in it.
#[test]
fn prepush_always_run_entries_name_timed_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap();
    let data: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("packages/aethyme/rust/.config/prepush-tests.json"))
            .unwrap(),
    )
    .unwrap();
    let modules = data["modules"].as_object().unwrap();
    let suite: f64 = modules.values().map(|v| v.as_f64().unwrap()).sum();
    let budget = data["budget_seconds"].as_f64().unwrap();
    assert!(
        budget > 0.0 && budget <= 0.2 * suite,
        "budget {budget} s of {suite} s"
    );
    let binaries: Vec<&str> = data["always_binaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_str().unwrap())
        .collect();
    for binary in &binaries {
        assert!(
            modules
                .keys()
                .any(|key| key.starts_with(&format!("{binary} "))),
            "always-run binary {binary} has no timed tests"
        );
    }
    for entry in data["always_modules"].as_array().unwrap() {
        let key = format!(
            "{} {}",
            entry["binary"].as_str().unwrap(),
            entry["module"].as_str().unwrap()
        );
        assert!(
            modules.contains_key(&key),
            "always-run module {key} has no timed tests"
        );
    }
    for guard in [
        "aethyme-cli::help_snapshots",
        "aethyme-cli::help_surface",
        "aethyme-cli::help_everywhere",
        "aethyme-testkit::broker_discarded_results",
        "aethyme-testkit::deprecated_spelling_callers",
        "aethyme-testkit::docs_hygiene",
        "aethyme-testkit::no_eval_tuning",
        "aethyme-testkit::pr_template",
        "aethyme-testkit::grammar_provenance",
        "aethyme-broker::gate_policy",
    ] {
        assert!(binaries.contains(&guard), "{guard} is not always run");
    }
}
