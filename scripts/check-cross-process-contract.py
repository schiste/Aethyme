#!/usr/bin/env python3
"""Heuristic check for unannounced contract changes.

A "contract change" is a diff that touches a symbol named in
`packages/aethyme/docs/architecture/cross-process-consumers.md` —
the canonical inventory of cross-process Aethyme entry points.
The 2026-05-08 hard-delete of `python -m src.cli explore` broke
the deployed `aethyme-explore` wrapper because the consumer wasn't
listed; this script is the friction layer that catches the next
miss.

Logic:

1. Parse `cross-process-consumers.md` for inline-code symbols
   (backtick-wrapped tokens). These are the names whose removal
   from the codebase has cross-process blast radius.
2. Read the PR diff (against `--base` or origin/main).
3. For each *removed* line in the diff, check if it contains a
   tracked symbol. Removed lines are the dangerous direction —
   added lines mean someone is *introducing* something, which is
   fine on its own.
4. If any tracked symbols appear on removed lines:
   - Look for a `Contract` decision in the PR body (passed via
     `--pr-body` or stdin).
   - If the Contract is missing or set to `none`, exit non-zero
     with a list of touched symbols.
   - If the Contract is `soft-retire`, `hard-delete`, or
     `introduce`, exit zero with an informational note.

This is intentionally heuristic. False positives are acceptable —
they prompt a human to confirm the contract decision. False
negatives (silently dropping a tracked symbol) are the failure
mode this script exists to prevent.

Usage::

    # Locally, against the merge base of HEAD:
    scripts/check-cross-process-contract.py

    # In CI, against an explicit base ref and PR body file:
    scripts/check-cross-process-contract.py \\
        --base origin/main \\
        --pr-body /tmp/pr-body.md

Exit codes:
  0 — clean, or contract decision documented.
  1 — contract change detected without a documented decision.
  2 — script invocation error (bad args, missing files).
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CONSUMERS_DOC = (
    REPO_ROOT
    / "packages" / "aethyme" / "docs" / "architecture"
    / "cross-process-consumers.md"
)
# Tokens shorter than this are too noisy to track (`a`, `it`, `of`).
MIN_SYMBOL_LEN = 4
# Symbols matching these patterns are too generic to track usefully.
EXCLUDED_PATTERNS = (
    re.compile(r"^[A-Z]+\s*$"),  # bare ALLCAPS like "TODO"
    re.compile(r"^\d+$"),         # bare numbers
)
# python-retirement Phase 5.5 (2026-08-01): deployed templates project
# Explore with `aethyme explore-summary --from <json>`. Any reference to
# the Aethyme venv interpreter is stale — the product path must work with
# no Python on PATH (Phase 6 exit criterion).
_STALE_PYTHON_INVOCATIONS = (
    "python -m src.cli explore",
    ".venv/bin/python -m src.cli explore",
    '"$AETHYME_PY" -m src.cli explore',
    '"$AETHYME_ROOT/.venv/bin/python" -m src.cli explore',
    ".venv/bin/python",
    "$AETHYME_PY",
)
TEXT_CONSUMER_CHECKS = (
    (
        REPO_ROOT / "packages" / "aethyme" / "skills" / "aethyme" / "SKILL.md",
        _STALE_PYTHON_INVOCATIONS,
    ),
    (
        REPO_ROOT / "packages" / "aethyme" / "skills" / "aethyme" / "AGENTS.md",
        _STALE_PYTHON_INVOCATIONS,
    ),
    (
        REPO_ROOT
        / "packages" / "aethyme" / "skills" / "aethyme"
        / "references" / "explore.md",
        _STALE_PYTHON_INVOCATIONS,
    ),
)


def extract_tracked_symbols(doc_text: str) -> set[str]:
    """Pull every backtick-wrapped symbol out of the consumers doc.

    The doc uses `code` for everything that has cross-process
    blast radius: file paths, CLI subcommand names, intent names,
    schema field names, env vars. Treat the whole set as tracked.
    """
    raw = re.findall(r"`([^`\n]+)`", doc_text)
    out: set[str] = set()
    for token in raw:
        token = token.strip()
        if len(token) < MIN_SYMBOL_LEN:
            continue
        if any(p.match(token) for p in EXCLUDED_PATTERNS):
            continue
        out.add(token)
    return out


def read_diff(base: str) -> list[str]:
    """Return the unified diff against `base`. Raises on git error."""
    result = subprocess.run(
        ["git", "diff", "--unified=0", base, "--", "."],
        capture_output=True,
        text=True,
        cwd=REPO_ROOT,
        check=False,
    )
    if result.returncode != 0:
        msg = result.stderr.strip() or "git diff failed"
        print(f"ERROR: {msg}", file=sys.stderr)
        sys.exit(2)
    return result.stdout.splitlines()


def find_touched_symbols(diff_lines: list[str], tracked: set[str]) -> dict[str, list[str]]:
    """Return `{symbol: [diff_line, …]}` for tracked symbols whose
    removal appears in the diff. We focus on removals because:

    - Added lines that mention a tracked symbol are usually new
      consumers — those should be added to the consumers doc, but
      they don't break anything by being added.
    - Removed lines are the operation that breaks downstream
      consumers (the entry point is gone; callers crash).
    """
    findings: dict[str, list[str]] = {}
    for line in diff_lines:
        # `--- a/path` and `+++ b/path` appear at hunk boundaries.
        # Skip them. Real removals are `-text`, real additions `+text`.
        if line.startswith("---") or line.startswith("+++"):
            continue
        if not line.startswith("-"):
            continue
        body = line[1:]
        for symbol in tracked:
            if symbol in body:
                findings.setdefault(symbol, []).append(line)
    return findings


def parse_contract_decision(pr_body: str) -> str | None:
    """Find the contract decision the author declared.

    Two accepted spellings:
    - PR template checkbox: `- [x] **<label>**` (the GitHub PR path).
    - Commit-message line: `Contract decision: <label>` (the broker
      path, where submissions have no PR body and the decision lives
      in commit messages — added 2026-07-27 when the checker became a
      broker gate; the 12-day `query deps` break shipped through a
      broker submission the PR-only checker never saw).

    If neither appears, the contract is undeclared.
    """
    checkbox = re.compile(
        r"-\s*\[\s*[xX]\s*\]\s*\*\*(none|introduce|soft-retire|hard-delete)\*\*",
        re.IGNORECASE,
    )
    message_line = re.compile(
        r"^\s*Contract decision:\s*(none|introduce|soft-retire|hard-delete)\b",
        re.IGNORECASE | re.MULTILINE,
    )
    matches = checkbox.findall(pr_body or "") + message_line.findall(pr_body or "")
    if not matches:
        return None
    # Prefer the most-restrictive label if multiple are declared.
    order = ("hard-delete", "soft-retire", "introduce", "none")
    for tier in order:
        if any(m.lower() == tier for m in matches):
            return tier
    return matches[0].lower()


def find_text_consumer_violations(
    checks: tuple[tuple[Path, tuple[str, ...]], ...] = TEXT_CONSUMER_CHECKS,
) -> dict[str, list[str]]:
    """Return forbidden command references in canonical text consumers."""
    violations: dict[str, list[str]] = {}
    for path, forbidden_patterns in checks:
        if not path.exists():
            continue
        hits: list[str] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            normalized = line.strip().lower()
            for pattern in forbidden_patterns:
                if pattern not in line:
                    continue
                if any(
                    marker in normalized
                    for marker in ("do not run", "not a valid command", "was removed")
                ):
                    continue
                hits.append(pattern)
        if hits:
            try:
                display_path = str(path.relative_to(REPO_ROOT))
            except ValueError:
                display_path = str(path)
            violations[display_path] = hits
    return violations


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument(
        "--base",
        default="origin/main",
        help="Base ref to diff against (default: origin/main).",
    )
    parser.add_argument(
        "--pr-body",
        type=Path,
        help="Path to a file containing the PR body (for contract parsing).",
    )
    args = parser.parse_args()

    if not CONSUMERS_DOC.exists():
        print(
            f"ERROR: {CONSUMERS_DOC} not found — cannot determine "
            "tracked symbols.",
            file=sys.stderr,
        )
        return 2
    tracked = extract_tracked_symbols(CONSUMERS_DOC.read_text())
    if not tracked:
        print(
            "ERROR: no tracked symbols extracted from consumers doc — "
            "the doc may be empty or malformed.",
            file=sys.stderr,
        )
        return 2

    text_violations = find_text_consumer_violations()
    if text_violations:
        print("ERROR: forbidden removed command references found in text consumers:", file=sys.stderr)
        for path, patterns in sorted(text_violations.items()):
            print(f"  - {path}", file=sys.stderr)
            for pattern in patterns:
                print(f"    contains: {pattern}", file=sys.stderr)
        print(
            "Use the native Explore binary in skills/agent instructions: "
            "`$AETHYME_ROOT/rust/target/release/aethyme explore ...`.",
            file=sys.stderr,
        )
        return 1

    diff_lines = read_diff(args.base)
    if not diff_lines:
        print("clean: empty diff against base, nothing to check.")
        return 0

    findings = find_touched_symbols(diff_lines, tracked)
    if not findings:
        print(
            f"clean: no tracked cross-process symbols touched on "
            f"removed lines (checked {len(tracked)} symbols)."
        )
        return 0

    pr_body = ""
    if args.pr_body and args.pr_body.exists():
        pr_body = args.pr_body.read_text()
    decision = parse_contract_decision(pr_body)

    print("Cross-process symbols touched on removed lines:")
    for symbol, lines in sorted(findings.items()):
        print(f"  - `{symbol}` ({len(lines)} occurrence(s))")
    print()
    if decision in ("introduce", "soft-retire", "hard-delete"):
        print(
            f"Contract decision in PR body: **{decision}** — "
            "treating as deliberate."
        )
        return 0
    if decision == "none":
        print(
            "ERROR: PR contract is **none**, but the diff removes "
            "tracked cross-process symbols. Either pick a different "
            "contract label (introduce / soft-retire / hard-delete) "
            "or restore the symbols.",
            file=sys.stderr,
        )
        return 1
    print(
        "ERROR: PR body does not declare a contract decision "
        "(`none` / `introduce` / `soft-retire` / `hard-delete`). "
        "See `.github/pull_request_template.md`. The 2026-05-08 "
        "playground breakage came from a missing decision here.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
