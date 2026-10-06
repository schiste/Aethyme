//! Gate policy asserted against the live `.aethyme/gates.toml`.
//!
//! Every other gates suite in this crate writes its own fixture into a
//! temporary repository, so until this file existed nothing read the policy
//! the repository actually ships. That is the permissive shape `gates.toml`
//! already documents three times over: a gate that did not run is
//! indistinguishable from a gate that passed, and trigger lists are a
//! hand-maintained mirror of what the suites read, so they drift in one
//! direction only.
//!
//! These tests drive the broker's own loader and selector rather than a
//! re-implementation, because a second copy of the matcher would be one more
//! mirror to keep in sync. `parse_gates` compiles each trigger with a bare
//! `globset::Glob::new` and no normalization (`gates.rs`), so the
//! single-glob matcher built below is byte-identical to one member of the
//! set the broker compiles.

use aethyme_broker::{GATES_CONFIG_RELPATH, load_gates, select_gates};
use aethyme_testkit::repo_root;
use globset::Glob;
use std::path::Path;
use std::process::Command;

/// Repository-relative paths Git tracks in the checkout under test.
///
/// `ls-files` is one of the subcommands the local `cto_bin` wrapper passes
/// straight through, so its bytes are the real Git's.
fn tracked_files(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git ls-files runs");
    assert!(
        out.status.success(),
        "git ls-files failed in {}: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("tracked paths are utf-8")
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The gate config is itself source: editing it changes which gates run.
///
/// With no trigger naming it, an entry that rewrites gate policy selects
/// only the always-on gates — so the very gate being weakened never runs to
/// object, and the entry auto-promotes. Found 2026-09-13, one day after the
/// same shape was found for `.github/workflows/**`.
#[test]
fn installer_and_pilot_scripts_select_their_narrow_contract_gate() {
    let gates = load_gates(&repo_root()).unwrap();
    for path in [
        "install.sh",
        "scripts/pilot-report.jq",
        "scripts/pilot-compare.jq",
    ] {
        let selected = select_gates(&gates, &[path.to_string()]);
        let contract = selected
            .iter()
            .find(|selection| selection.gate.name == "script-contract")
            .unwrap_or_else(|| panic!("{path} has no script contract coverage"));
        assert_eq!(contract.gate.cost, 1);
        assert!(
            contract
                .gate
                .command
                .contains("--test release_contract installer_")
        );
        assert!(contract.gate.command.contains("--test pilot_report"));
        assert!(
            !selected
                .iter()
                .any(|selection| selection.gate.name == "cargo-test"),
            "script-only changes should not need the full workspace suite"
        );
    }
}

#[test]
fn gate_policy_changes_select_a_triggered_gate() {
    let root = repo_root();
    let gates = load_gates(&root).expect("the shipped gates.toml parses");
    let selected = select_gates(&gates, &[GATES_CONFIG_RELPATH.to_string()]);

    // An always-on gate reports `triggered_by: None`. It selects on every
    // diff and therefore proves nothing about this path in particular.
    let triggered: Vec<&str> = selected
        .iter()
        .filter(|selection| selection.triggered_by.is_some())
        .map(|selection| selection.gate.name.as_str())
        .collect();

    assert!(
        !triggered.is_empty(),
        "{GATES_CONFIG_RELPATH} matches no gate trigger, so an entry that edits gate \
         policy runs only the always-on gates. Add it to the triggers of a gate whose \
         suites read it."
    );
}

#[test]
fn cross_process_contract_uses_load_admission_despite_its_low_cost() {
    let gates = load_gates(&repo_root()).expect("the shipped gates.toml parses");
    let gate = gates
        .iter()
        .find(|gate| gate.name == "cross-process-contract")
        .expect("the cross-process contract gate is configured");

    assert_eq!(
        gate.cost, 1,
        "the gate's cost should not be raised just to admit by load"
    );
    assert_eq!(
        gate.max_load_per_cpu,
        Some(3.0),
        "the gate should wait for load to fall below 3 per CPU before starting"
    );
}

/// The guard suites `fast-guards` runs. Each one also runs under
/// `cargo-test`; the gate exists so their failure arrives in seconds rather
/// than after the whole workspace suite.
const FAST_GUARD_SUITES: &[&str] = &[
    "help_snapshots",
    "help_surface",
    "help_everywhere",
    "broker_discarded_results",
    "deprecated_spelling_callers",
    "docs_hygiene",
    "no_eval_tuning",
    "pr_template",
    "grammar_provenance",
    "gate_policy",
];

/// `fast-guards` must run, and run first, on every diff that runs
/// `cargo-test`. Gates run cheap-first and stop at the first failure, so a
/// lower cost is what puts the seconds-long guards ahead of the ~10-minute
/// workspace suite; a trigger `cargo-test` has and `fast-guards` lacks is a
/// path on which a guard failure is again found only at the end.
///
/// Its cost must also stay above 1: the pre-commit hook runs every gate of
/// cost 1 or less, and a Cargo build does not belong at commit time.
#[test]
fn fast_guards_precede_cargo_test_on_every_path_it_covers() {
    let gates = load_gates(&repo_root()).expect("the shipped gates.toml parses");
    let find = |name: &str| {
        gates
            .iter()
            .find(|gate| gate.name == name)
            .unwrap_or_else(|| panic!("gates.toml has no {name:?} gate"))
    };
    let fast = find("fast-guards");
    let full = find("cargo-test");

    assert!(
        fast.cost > 1 && fast.cost < full.cost,
        "fast-guards cost {} must be above the pre-commit ceiling (1) and below \
         cargo-test's {}",
        fast.cost,
        full.cost
    );
    let missing: Vec<&str> = full
        .triggers
        .iter()
        .filter(|trigger| !fast.triggers.contains(trigger))
        .map(String::as_str)
        .collect();
    assert!(
        missing.is_empty(),
        "cargo-test triggers missing from fast-guards: {missing:?}"
    );
    for suite in FAST_GUARD_SUITES {
        assert!(
            fast.command.contains(&format!("--test {suite} "))
                || fast.command.ends_with(&format!("--test {suite}")),
            "fast-guards does not run {suite}"
        );
    }
    // Same cache and build flags as cargo-test, so the artifacts fast-guards
    // builds are the ones cargo-test reuses instead of a second build.
    let cache = |gate: &aethyme_broker::Gate| {
        gate.managed_cache
            .as_ref()
            .map(|cache| (cache.key.clone(), cache.max_bytes))
    };
    assert_eq!(
        cache(fast),
        cache(full),
        "fast-guards must share cargo-test's cache"
    );
    let prefix = |command: &str| command.split(" && ").next().unwrap_or_default().to_string();
    assert_eq!(
        prefix(&fast.command),
        prefix(&full.command),
        "fast-guards must build with cargo-test's environment"
    );
}

/// A trigger glob that matches nothing fails silently: selection keeps
/// working and one gate simply stops being reachable by the path it was
/// written for. The retired `pytest-local` gate's `src/**` outlived `src/`
/// exactly this way, which is why the surviving triggers each name the suite
/// that reads them.
#[test]
fn every_trigger_glob_matches_a_tracked_file() {
    let root = repo_root();
    let gates = load_gates(&root).expect("the shipped gates.toml parses");
    let tracked = tracked_files(&root);
    assert!(
        !tracked.is_empty(),
        "no tracked files under {}",
        root.display()
    );

    let mut dead = Vec::new();
    for gate in &gates {
        for trigger in &gate.triggers {
            let matcher = Glob::new(trigger)
                .unwrap_or_else(|err| panic!("gate {}: bad glob {trigger:?}: {err}", gate.name))
                .compile_matcher();
            if !tracked.iter().any(|path| matcher.is_match(path)) {
                dead.push(format!("{} -> {trigger}", gate.name));
            }
        }
    }

    assert!(
        dead.is_empty(),
        "trigger glob(s) match no tracked file, so the gate is unreachable by the path \
         it was written for: {dead:?}"
    );
}
