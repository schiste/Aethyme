//! Implementation-blind CLI checks for the commit-hygiene contract.
//!
//! Ported from `tests/local/test_commit_hygiene.py` (python-retirement
//! Phase 7). The function-level template/linter cases that imported
//! `src.indexing.commit_hygiene` moved to Rust unit tests in
//! `aethyme-enhance/src/hygiene.rs` when the module was ported (Phase 3);
//! what remains here drives the frozen CLI surface through the router.

use aethyme_testkit::repos::write;
use aethyme_testkit::{invoke_aethyme, tmp_dir};

#[test]
fn repo_commit_message_template_command() {
    let result = invoke_aethyme([
        "repo",
        "commit-message-template",
        "--type",
        "feat",
        "--scope",
        "repo-memory",
    ]);
    result.ok();
    assert!(
        result.output.starts_with("feat(repo-memory): short summary\n"),
        "{}",
        result.output
    );
    result.assert_contains("Validation:\n- ...\n");
}

#[test]
fn repo_commit_message_templates_are_subject_only_for_non_substantive_types() {
    for commit_type in ["docs", "chore", "test", "build", "revert"] {
        let result = invoke_aethyme(["repo", "commit-message-template", "--type", commit_type]);
        result.ok();
        assert_eq!(
            result.output,
            format!("{commit_type}(scope): short summary\n")
        );
    }
}

#[test]
fn repo_lint_commit_message_command_supports_file_and_json() {
    let tmp = tmp_dir();
    let message_path = tmp.path().join("COMMIT_EDITMSG");
    write(
        &message_path,
        "docs: clarify onboarding\n\nProblem:\nOnboarding commands were unclear.\n\n\
         Decision:\nClarify the operator-facing examples.\n\n\
         Validation:\n- Reviewed generated docs output.\n",
    );

    let result = invoke_aethyme([
        "repo",
        "lint-commit-message",
        &message_path.display().to_string(),
        "--json-output",
    ]);
    result.ok();
    let payload = result.json();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["subject"]["type"], "docs");
    assert_eq!(payload["body_required"], false);
}

#[test]
fn repo_lint_commit_message_accepts_structured_fix() {
    let message = "fix(watchlist): mark only viewed revision as seen\n\n\
         Problem:\nViewing a diff marked every revision as seen.\n\n\
         Decision:\nUse the viewed revision id for seen-marking.\n\n\
         Rationale:\nSeen state is revision-scoped.\n\n\
         Validation:\n- Added regression coverage.\n\n\
         Memory:\nWatchlist seen-marking must remain revision-scoped.\n";

    let result = invoke_aethyme([
        "repo",
        "lint-commit-message",
        "--message",
        message,
        "--json-output",
    ]);
    result.ok();
    let payload = result.json();
    assert_eq!(payload["ok"], true);
    assert_eq!(
        payload["subject"],
        serde_json::json!({
            "type": "fix",
            "scope": "watchlist",
            "summary": "mark only viewed revision as seen",
        })
    );
    assert!(
        payload["recognized_sections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|section| section == "Decision")
    );
    assert!(
        payload["memory_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["type"] == "decision")
    );
}

#[test]
fn repo_lint_commit_message_accepts_inline_sections() {
    let message = "fix(hygiene): accept inline sections\n\n\
         Problem: Standalone headers were the only accepted form.\n\
         Decision: Parse initial content after a known header.\n\
         Rationale: Both documented structured forms should be valid.\n\
         Validation: Added inline and multiline coverage.\n";

    let result = invoke_aethyme([
        "repo",
        "lint-commit-message",
        "--message",
        message,
        "--json-output",
    ]);
    result.ok();
    let payload = result.json();
    assert_eq!(payload["ok"], true);
    assert_eq!(
        payload["sections"]["Problem"],
        "Standalone headers were the only accepted form."
    );
    assert_eq!(
        payload["sections"]["Rationale"],
        "Both documented structured forms should be valid."
    );
}

#[test]
fn repo_lint_commit_message_command_fails_invalid_message() {
    let result = invoke_aethyme([
        "repo",
        "lint-commit-message",
        "--message",
        "fix: short\n\nProblem:\n...\nDecision:\n...\nValidation:\n- tests\n",
    ]);
    result.expect_code(1);
    result.assert_contains("Valid: no");
    result.assert_contains("Missing required section: Rationale.");
}
