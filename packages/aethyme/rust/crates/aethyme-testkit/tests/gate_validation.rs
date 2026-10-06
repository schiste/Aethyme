//! Pins what the repository's own `cargo-test` gate runs.
//!
//! From 2026-09-30 to 2026-10-01 the gate ran `cargo test --workspace
//! --examples`, which restricts Cargo to example targets: three example
//! binaries ran and no library, binary or integration test did. Nothing
//! failed, so nothing noticed. Aethyme Gates on `main` and every local
//! `broker submit` use this gate, and a release requires it to pass.

use std::path::PathBuf;

fn gates_toml() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    std::fs::read_to_string(root.join(".aethyme/gates.toml")).expect("read .aethyme/gates.toml")
}

fn cargo_test_command() -> String {
    let text = gates_toml();
    let gates: toml::Value = text.parse().expect("gates.toml parses");
    gates["gate"]
        .as_array()
        .expect("[[gate]] entries")
        .iter()
        .find(|gate| gate["name"].as_str() == Some("cargo-test"))
        .and_then(|gate| gate["command"].as_str())
        .expect("a cargo-test gate with a command")
        .to_owned()
}

/// Every `cargo test` invocation in the gate, split on shell separators.
fn cargo_test_runs(command: &str) -> Vec<String> {
    command
        .split(['&', ';'])
        .map(str::trim)
        .filter(|part| part.starts_with("cargo test"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_cargo_test_gate_runs_the_whole_workspace_suite() {
    let command = cargo_test_command();
    let runs = cargo_test_runs(&command);
    assert!(
        runs.iter().any(|run| run.contains("--workspace")
            && !run.contains("--examples")
            && !run.contains("--lib")
            && !run.contains("--bins")
            && !run.contains("--tests")),
        "no unrestricted `cargo test --workspace` run in the cargo-test gate: {command}"
    );
}

#[test]
fn the_cargo_test_gate_still_runs_the_example_tests() {
    let command = cargo_test_command();
    assert!(
        cargo_test_runs(&command)
            .iter()
            .any(|run| run.contains("--examples")),
        "the release-manifest example tests (#435) no longer run: {command}"
    );
}

#[test]
fn the_cargo_test_gate_fails_when_any_run_fails() {
    let command = cargo_test_command();
    assert!(
        command.contains("suite=$?")
            && command.contains("doc=$?")
            && command.contains("examples=$?"),
        "the gate must record every run's exit status: {command}"
    );
    assert!(
        command
            .trim_end_matches('\'')
            .trim_end()
            .ends_with(r#"[ "$suite" -eq 0 ] && [ "$doc" -eq 0 ] && [ "$examples" -eq 0 ]"#),
        "the gate's exit status must require every run to pass: {command}"
    );
}

/// Where nextest is installed the gate runs the suite under it (2026-10-06).
/// That path must still cover the whole workspace, use the prebuilt product
/// binaries, and run the doctests nextest skips.
#[test]
fn the_nextest_path_runs_the_whole_workspace_and_the_doctests() {
    let command = cargo_test_command();
    let nextest = command
        .split(['&', ';'])
        .map(str::trim)
        .find(|part| part.contains("cargo nextest run"))
        .unwrap_or_else(|| panic!("no nextest run in the cargo-test gate: {command}"));
    assert!(
        nextest.contains("--workspace")
            && nextest.contains("--no-fail-fast")
            && ![
                "--lib",
                "--bins",
                "--tests",
                "--examples",
                "--profile",
                "-E "
            ]
            .iter()
            .any(|flag| nextest.contains(flag)),
        "the nextest run must be an unrestricted, no-retry workspace run: {nextest}"
    );
    assert!(
        command.contains("AETHYME_TESTKIT_PREBUILT_BINS=1 cargo nextest run")
            && command.contains("cargo build --locked --workspace --bins"),
        "nextest needs the product binaries built first: {command}"
    );
    assert!(
        cargo_test_runs(&command)
            .iter()
            .any(|run| run.contains("--doc")),
        "nextest runs no doctests, so the gate must: {command}"
    );
}
