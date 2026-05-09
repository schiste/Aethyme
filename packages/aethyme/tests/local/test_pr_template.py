"""The repo's PR template carries cardinal-rule guard rails.

`.github/pull_request_template.md` includes:

- A `## Contract` section with four mutually-exclusive labels:
  `none`, `introduce`, `soft-retire`, `hard-delete`. CI
  (task #87) reads this line to decide whether a diff that touches
  cross-process-consumed entry points was approved deliberately.
- An `## Eval impact` block re-stating the cardinal-rule test ("would
  I make this change if the eval didn't exist?").

The template is the friction layer that keeps the cross-process
boundary maintained. If it goes missing, contributors will quietly
hard-delete entry points again (we did this once on 2026-05-08 with
`python -m src.cli explore`; the playground broke).
"""

from __future__ import annotations

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
PR_TEMPLATE = REPO_ROOT / ".github" / "pull_request_template.md"


def test_pr_template_exists():
    assert PR_TEMPLATE.exists(), (
        f"{PR_TEMPLATE} must exist — see task #86 / cross-process-consumers"
    )


def test_pr_template_declares_all_four_contract_labels():
    """`none / introduce / soft-retire / hard-delete` cover the
    full lifecycle of a cross-process entry point. Missing any one
    means a PR can avoid declaring the change category."""
    text = PR_TEMPLATE.read_text()
    for label in ("none", "introduce", "soft-retire", "hard-delete"):
        # Match the bold form to avoid false positives on stray
        # mentions in surrounding prose (e.g. "introduce" as a verb).
        assert f"**{label}**" in text, (
            f"PR template missing contract label {label!r}"
        )


def test_pr_template_links_to_cross_process_consumers_doc():
    """The contract section's whole point is to keep the
    cross-process-consumers inventory current. The template must
    point readers at it."""
    text = PR_TEMPLATE.read_text()
    assert "cross-process-consumers.md" in text


def test_pr_template_includes_cardinal_rule_check():
    """The `Eval impact` block must restate the cardinal-rule test.
    PR authors who don't think about it before submitting are the
    failure mode this section exists to prevent."""
    text = PR_TEMPLATE.read_text()
    assert "if the eval didn" in text or "if the evals didn" in text, (
        "PR template must include the cardinal-rule self-check question"
    )


def test_pr_template_links_to_rejected_tunings_doc():
    """When a contributor's first instinct fails the cardinal-rule
    test, the template should send them to the rejected-tunings
    doc to record the rejection rather than to silently rewrite
    the change."""
    text = PR_TEMPLATE.read_text()
    assert "eval-tuning-rejected.md" in text
