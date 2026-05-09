"""The rejected eval-tunings registry must exist and stay non-trivial.

`docs/architecture/eval-tuning-rejected.md` is the audit trail for
the cardinal rule "no eval-overfitting." It must exist (so the
eval-protocol link doesn't dangle) and contain at least the seeded
entries (so a contributor opening it sees real examples of rejected
tuning, not an empty stub that invites adding one).

We don't pin exact dates or wording — those evolve. We pin:
1. The file exists.
2. The file is linked from the eval protocol.
3. At least four reasoned rejection entries exist (the seed set
   covers the four pattern families: prompt-tightening, schema
   scaffolding, reference matching, and behavior switching by
   eval detection).
"""

from __future__ import annotations

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
REJECTED_DOC = REPO_ROOT / "docs" / "architecture" / "eval-tuning-rejected.md"
PROTOCOL_DOC = REPO_ROOT / "docs" / "guides" / "eval-protocol.md"


def test_rejected_doc_exists():
    assert REJECTED_DOC.exists(), (
        f"{REJECTED_DOC} must exist — see eval-protocol.md cardinal rule"
    )


def test_rejected_doc_is_linked_from_eval_protocol():
    """A reader following the eval protocol should be able to find
    the audit trail. If this link breaks the doc becomes invisible."""
    text = PROTOCOL_DOC.read_text()
    assert "eval-tuning-rejected" in text, (
        "eval-protocol.md must link to eval-tuning-rejected.md"
    )


def test_rejected_doc_has_at_least_four_seeded_entries():
    """Each top-level entry is preceded by `## ` and has the year in
    the title (so the entry shape stays consistent with the seed
    set). Counting `## ` headings excluding the document's own
    structure (Purpose, How to add an entry)."""
    text = REJECTED_DOC.read_text()
    # Entries are dated; structural sections aren't.
    entry_headings = [
        line for line in text.splitlines()
        if line.startswith("## ") and any(
            year in line for year in ("2026-", "2025-", "2024-")
        )
    ]
    assert len(entry_headings) >= 4, (
        f"Expected at least 4 dated entries; got {len(entry_headings)}: "
        f"{entry_headings}"
    )


def test_each_entry_states_proposal_and_rejection_reason():
    """Each entry must follow the documented shape: a Proposal block
    and a rejection-reason block. Without those, the entry is just a
    note and won't help a future contributor recognize the pattern."""
    text = REJECTED_DOC.read_text()
    # Coarse check: the body must use the bold field labels we
    # prescribed in "How to add an entry."
    assert "**Proposal:**" in text or "**Proposal.**" in text, (
        "entries must include a Proposal block"
    )
    assert (
        "**Why we rejected it:**" in text
        or "**Why we rejected it.**" in text
    ), "entries must include a rejection-reason block"
