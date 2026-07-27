from __future__ import annotations

import json
from pathlib import Path

from src.indexing.commit_hygiene import default_template, lint_commit_message
from tests.support.cli_invoke import invoke_aethyme


def test_default_template_includes_required_sections_for_fix() -> None:
    template = default_template(commit_type="fix", scope="watchlist")

    assert template.startswith("fix(watchlist): short summary\n")
    assert "Problem:\n...\n" in template
    assert "Decision:\n...\n" in template
    assert "Rationale:\n...\n" in template
    assert "Validation:\n- ...\n" in template
    assert "Memory:\n...\n" in template


def test_lint_commit_message_accepts_structured_fix() -> None:
    message = """fix(watchlist): mark only viewed revision as seen

Problem:
Viewing a diff marked every revision as seen.

Decision:
Use the viewed revision id for seen-marking.

Rationale:
Seen state is revision-scoped.

Validation:
- Added regression coverage.
- Ran watchlist tests.

Memory:
Watchlist seen-marking must remain revision-scoped.
"""

    result = lint_commit_message(message)

    assert result["ok"] is True
    assert result["subject"] == {
        "type": "fix",
        "scope": "watchlist",
        "summary": "mark only viewed revision as seen",
    }
    assert "Decision" in result["recognized_sections"]
    assert any(
        candidate["type"] == "decision"
        for candidate in result["memory_candidates"]
    )


def test_lint_commit_message_rejects_missing_required_section() -> None:
    message = """feat(repo-memory): add commit hygiene tool

Problem:
Agents need a consistent commit format.

Decision:
Add a linter and template generator.

Validation:
- Added tests.
"""

    result = lint_commit_message(message)

    assert result["ok"] is False
    assert "Missing required section: Rationale." in result["errors"]


def test_repo_commit_message_template_command() -> None:
    result = invoke_aethyme(
        ["repo", "commit-message-template", "--type", "feat", "--scope", "repo-memory"])

    assert result.exit_code == 0, result.output
    assert result.output.startswith("feat(repo-memory): short summary\n")
    assert "Validation:\n- ...\n" in result.output


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
