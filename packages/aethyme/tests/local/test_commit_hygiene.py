"""Implementation-blind CLI checks for the commit-hygiene contract.

The function-level template/linter cases that imported
`src.indexing.commit_hygiene` moved to Rust unit tests in
`aethyme-enhance/src/hygiene.rs` when the module was ported (Phase 3);
what remains here drives the frozen CLI surface through the router.
"""

from __future__ import annotations

import json
from pathlib import Path

from tests.support.cli_invoke import invoke_aethyme


def test_repo_commit_message_template_command() -> None:
    result = invoke_aethyme(
        ["repo", "commit-message-template", "--type", "feat", "--scope", "repo-memory"])

    assert result.exit_code == 0, result.output
    assert result.output.startswith("feat(repo-memory): short summary\n")
    assert "Validation:\n- ...\n" in result.output


def test_repo_commit_message_template_docs_skips_rationale() -> None:
    result = invoke_aethyme(["repo", "commit-message-template", "--type", "docs"])

    assert result.exit_code == 0, result.output
    assert result.output.startswith("docs(scope): short summary\n")
    assert "Rationale:" not in result.output


def test_repo_lint_commit_message_command_supports_file_and_json(tmp_path: Path) -> None:
    message_path = tmp_path / "COMMIT_EDITMSG"
    message_path.write_text(
        """docs: clarify onboarding

Problem:
Onboarding commands were unclear.

Decision:
Clarify the operator-facing examples.

Validation:
- Reviewed generated docs output.
""",
        encoding="utf-8",
    )

    result = invoke_aethyme(
        ["repo", "lint-commit-message", str(message_path), "--json-output"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["ok"] is True
    assert payload["subject"]["type"] == "docs"
    assert payload["body_required"] is False


def test_repo_lint_commit_message_accepts_structured_fix() -> None:
    message = """fix(watchlist): mark only viewed revision as seen

Problem:
Viewing a diff marked every revision as seen.

Decision:
Use the viewed revision id for seen-marking.

Rationale:
Seen state is revision-scoped.

Validation:
- Added regression coverage.

Memory:
Watchlist seen-marking must remain revision-scoped.
"""
    result = invoke_aethyme(
        ["repo", "lint-commit-message", "--message", message, "--json-output"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["ok"] is True
    assert payload["subject"] == {
        "type": "fix",
        "scope": "watchlist",
        "summary": "mark only viewed revision as seen",
    }
    assert "Decision" in payload["recognized_sections"]
    assert any(
        candidate["type"] == "decision" for candidate in payload["memory_candidates"]
    )


def test_repo_lint_commit_message_command_fails_invalid_message() -> None:
    result = invoke_aethyme(
        [
            "repo",
            "lint-commit-message",
            "--message",
            "fix: short\n\nProblem:\n...\nDecision:\n...\nValidation:\n- tests\n",
        ])

    assert result.exit_code == 1, result.output
    assert "Valid: no" in result.output
    assert "Missing required section: Rationale." in result.output
