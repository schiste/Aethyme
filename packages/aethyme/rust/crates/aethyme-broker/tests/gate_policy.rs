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
    assert!(!tracked.is_empty(), "no tracked files under {}", root.display());

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
