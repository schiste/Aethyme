//! The repo's PR template carries cardinal-rule guard rails.
//!
//! Ported from `tests/local/test_pr_template.py` (python-retirement
//! Phase 7). It inspects the monorepo's `.github/`, not any product
//! crate, so it lives with the workspace's dev-support crate.
//!
//! `.github/pull_request_template.md` includes:
//!
//! - A `## Contract` section with four mutually-exclusive labels:
//!   `none`, `introduce`, `soft-retire`, `hard-delete`. CI (task #87)
//!   reads this line to decide whether a diff that touches
//!   cross-process-consumed entry points was approved deliberately.
//! - An `## Eval impact` block re-stating the cardinal-rule test ("would
//!   I make this change if the eval didn't exist?").
//!
//! The template is the friction layer that keeps the cross-process
//! boundary maintained. If it goes missing, contributors will quietly
//! hard-delete entry points again (we did this once on 2026-05-08 with
//! `python -m src.cli explore`; the playground broke).

use std::path::PathBuf;

use aethyme_testkit::repo_root;

fn pr_template() -> PathBuf {
    repo_root().join(".github/pull_request_template.md")
}

fn text() -> String {
    std::fs::read_to_string(pr_template())
        .unwrap_or_else(|error| panic!("read {}: {error}", pr_template().display()))
}

#[test]
fn pr_template_exists() {
    assert!(
        pr_template().exists(),
        "{} must exist — see task #86 / cross-process-consumers",
        pr_template().display()
    );
}

/// `none / introduce / soft-retire / hard-delete` cover the full
/// lifecycle of a cross-process entry point. Missing any one means a PR
/// can avoid declaring the change category.
#[test]
fn pr_template_declares_all_four_contract_labels() {
    let text = text();
    for label in ["none", "introduce", "soft-retire", "hard-delete"] {
        // Match the bold form to avoid false positives on stray mentions
        // in surrounding prose (e.g. "introduce" as a verb).
        assert!(
            text.contains(&format!("**{label}**")),
            "PR template missing contract label {label:?}"
        );
    }
}

/// The contract section's whole point is to keep the
/// cross-process-consumers inventory current. The template must point
/// readers at it.
#[test]
fn pr_template_links_to_cross_process_consumers_doc() {
    assert!(text().contains("cross-process-consumers.md"));
}

/// The `Eval impact` block must restate the cardinal-rule test. PR
/// authors who don't think about it before submitting are the failure
/// mode this section exists to prevent.
#[test]
fn pr_template_includes_cardinal_rule_check() {
    let text = text();
    assert!(
        text.contains("if the eval didn") || text.contains("if the evals didn"),
        "PR template must include the cardinal-rule self-check question"
    );
}

/// When a contributor's first instinct fails the cardinal-rule test, the
/// template should send them to the rejected-tunings doc to record the
/// rejection rather than to silently rewrite the change.
#[test]
fn pr_template_links_to_rejected_tunings_doc() {
    assert!(text().contains("eval-tuning-rejected.md"));
}
