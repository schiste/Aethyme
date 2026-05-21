"""Telemetry helpers for Claude Code session-JSONL collection.

When the eval orchestrator launches 5 conditions in 5 Chau7 tabs, each
agent run produces a `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`
transcript. We need to map these JSONL files back to their conditions
to extract per-condition cost/duration/tool-call counts.

The naive approach is to read chau7's `tab_status.ai_session_id` for
each tab and look up that session-id JSONL. This works most of the
time but is unreliable: chau7 can report a stale or duplicated
`ai_session_id` after a tab is reused. Today (2026-05-08) we observed
two tabs reporting the SAME `ai_session_id` despite running in
different cwds; the JSONL collection mapped to the wrong condition.

The robust pattern, used by today's eval-collection code, is to
content-match: for each candidate JSONL, read the first user message,
regex out the output-file path mentioned in the prompt, and use that
as the condition key. This works because every eval prompt template
embeds an output-file path with the condition name (e.g.
`.aethyme-eval-output-leverage.json`).

This module codifies that pattern so future eval-collection code
doesn't reinvent it (and inevitably get the off-by-one wrong on
"latest mtime" deduplication).
"""

from __future__ import annotations

import json
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

# The default output-path regex matches the canonical
# `.aethyme-eval-output-<condition>.json` filename used by every eval
# type's prompt template. Override via `output_path_pattern` if a
# future eval type uses a different shape.
DEFAULT_OUTPUT_PATH_PATTERN = re.compile(
    r"\.aethyme-eval-output-([\w-]+)\.json"
)


def project_dir_for_repo(repo_cwd: str | Path) -> Path:
    """Compute the `~/.claude/projects/<encoded>/` dir for a given repo cwd.

    Claude Code's encoding rule:
      - leading `/` → leading `-`
      - subsequent `/` → `-`
      - spaces in path components → `-` per space (so `Foo - Bar`
        becomes `Foo---Bar`, three dashes)

    The resulting dir lives under `~/.claude/projects/`.
    """
    cwd = str(Path(repo_cwd).resolve())
    # Replace `/` with `-` first; spaces stay (will become `-` next)
    encoded = cwd.replace("/", "-").replace(" ", "-")
    return Path.home() / ".claude" / "projects" / encoded


def map_sessions_by_prompt(
    candidate_jsonls: list[Path],
    *,
    expected_conditions: list[str] | None = None,
    output_path_pattern: re.Pattern[str] = DEFAULT_OUTPUT_PATH_PATTERN,
) -> dict[str, Path]:
    """Map condition name → most-recent JSONL by content matching.

    For each candidate JSONL: read the first ``type=user`` message, regex
    the output-file path out of the prompt, derive the condition name.
    When multiple JSONLs match the same condition, keep the one with
    the latest mtime (so re-runs win over leftover from prior days).

    Parameters
    ----------
    candidate_jsonls:
        JSONL files to consider. Typically `list(project_dir.glob("*.jsonl"))`
        for one or both Control/Aethyme dirs.
    expected_conditions:
        If supplied, the result is filtered to only these keys. Useful
        as an assertion check that all 5 conditions were found.
    output_path_pattern:
        Regex with one group capturing the condition name. Default
        matches `.aethyme-eval-output-<cond>.json`.

    Returns
    -------
    Dict mapping condition name → most-recent JSONL path. Conditions
    with no matching JSONL are absent from the result (use
    ``set(expected_conditions) - result.keys()`` to detect missing).
    """
    by_condition: dict[str, tuple[float, Path]] = {}

    for jsonl in candidate_jsonls:
        try:
            marker = _extract_condition_marker_from_jsonl(jsonl, output_path_pattern)
        except (OSError, json.JSONDecodeError):
            # Unreadable or corrupted JSONL — skip silently. The caller's
            # missing-conditions check will surface the gap.
            continue
        if marker is None:
            continue
        cond = marker["condition"]
        mtime = jsonl.stat().st_mtime
        existing = by_condition.get(cond)
        if existing is None or mtime > existing[0]:
            by_condition[cond] = (mtime, jsonl)

    result = {cond: path for cond, (_mtime, path) in by_condition.items()}
    if expected_conditions is not None:
        result = {cond: path for cond, path in result.items() if cond in expected_conditions}
    return result


def map_session_attributions_by_prompt(
    candidate_jsonls: list[Path],
    *,
    expected_conditions: list[str] | None = None,
    reported_session_ids: dict[str, str | None] | None = None,
    reported_jsonl_paths: dict[str, str | Path | None] | None = None,
    expected_session_ids: dict[str, str | None] | None = None,
    output_path_pattern: re.Pattern[str] = DEFAULT_OUTPUT_PATH_PATTERN,
    matched_at: str | None = None,
) -> dict[str, dict[str, Any]]:
    """Return per-condition transcript attribution records.

    This is the auditable companion to :func:`map_sessions_by_prompt`.
    The mapping helper intentionally hides Chau7's stale ``ai_session_id``
    problem by returning the content-matched JSONL. This helper exposes the
    evidence trail: what Chau7 reported, which JSONL matched the prompt marker,
    and whether the two disagree.
    """
    matched_at = matched_at or datetime.now(UTC).isoformat()
    reported_session_ids = reported_session_ids or {}
    reported_jsonl_paths = reported_jsonl_paths or {}
    expected_session_ids = expected_session_ids or {}

    by_condition: dict[str, tuple[float, Path, dict[str, str]]] = {}

    for jsonl in candidate_jsonls:
        try:
            marker = _extract_condition_marker_from_jsonl(jsonl, output_path_pattern)
        except (OSError, json.JSONDecodeError):
            continue
        if marker is None:
            continue
        cond = marker["condition"]
        mtime = jsonl.stat().st_mtime
        existing = by_condition.get(cond)
        if existing is None or mtime > existing[0]:
            by_condition[cond] = (mtime, jsonl, marker)

    conditions = (
        list(expected_conditions)
        if expected_conditions is not None
        else sorted(by_condition)
    )
    records: dict[str, dict[str, Any]] = {}
    for cond in conditions:
        item = by_condition.get(cond)
        matched_path = item[1] if item else None
        marker = item[2] if item else None
        records[cond] = build_attribution_confidence(
            condition=cond,
            reported_chau7_session_id=reported_session_ids.get(cond),
            reported_jsonl_path=reported_jsonl_paths.get(cond),
            expected_session_id=expected_session_ids.get(cond),
            content_matched_jsonl_path=matched_path,
            matched_marker=marker.get("marker") if marker else None,
            matched_condition=marker.get("condition") if marker else None,
            matched_at=matched_at if marker else None,
        )
    return records


def build_attribution_confidence(
    *,
    condition: str,
    reported_chau7_session_id: str | None = None,
    reported_jsonl_path: str | Path | None = None,
    expected_session_id: str | None = None,
    content_matched_jsonl_path: str | Path | None = None,
    matched_marker: str | None = None,
    matched_condition: str | None = None,
    matched_at: str | None = None,
) -> dict[str, Any]:
    """Build the first-class per-condition attribution-confidence artifact."""
    matched_path_str = (
        str(content_matched_jsonl_path)
        if content_matched_jsonl_path is not None
        else None
    )
    reported_path_str = str(reported_jsonl_path) if reported_jsonl_path is not None else None
    matched_session_id = (
        Path(matched_path_str).stem
        if matched_path_str
        else None
    )
    reported = reported_chau7_session_id or None
    mismatch = bool(reported and matched_session_id and reported != matched_session_id)

    if matched_path_str and mismatch:
        confidence = "content-match-overrode-reported-session"
    elif matched_path_str and reported:
        confidence = "high"
    elif matched_path_str:
        confidence = "content-matched-no-reported-session"
    else:
        confidence = "missing-transcript"

    return {
        "schema_version": "aethyme-attribution-confidence-v1",
        "condition": condition,
        "reported_chau7_session_id": reported,
        "reported_jsonl_path": reported_path_str,
        "expected_session_id": expected_session_id,
        "content_matched_jsonl_path": matched_path_str,
        "content_matched_session_id": matched_session_id,
        "matched_marker": matched_marker,
        "matched_condition": matched_condition,
        "matched_at": matched_at,
        "attribution_mismatch": mismatch,
        "confidence": confidence,
    }


def _extract_condition_from_jsonl(
    jsonl: Path,
    output_path_pattern: re.Pattern[str],
) -> str | None:
    """Read the first user message from `jsonl` and regex out the condition."""
    marker = _extract_condition_marker_from_jsonl(jsonl, output_path_pattern)
    return marker["condition"] if marker else None


def _extract_condition_marker_from_jsonl(
    jsonl: Path,
    output_path_pattern: re.Pattern[str],
) -> dict[str, str] | None:
    """Return condition + exact prompt marker from the first user message."""
    with jsonl.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                # Some session files have transient malformed lines at
                # the start. Skip until we hit a parseable one.
                continue
            if msg.get("type") != "user":
                continue
            text = msg.get("message", {}).get("content", "")
            if isinstance(text, list):
                text = " ".join(
                    c.get("text", "")
                    for c in text
                    if isinstance(c, dict)
                )
            m = output_path_pattern.search(text or "")
            if m:
                return {"condition": m.group(1), "marker": m.group(0)}
            # First user message had no output-file path — this isn't
            # an eval session. Don't keep scanning.
            return None
    return None


def missing_conditions(
    mapping: dict[str, Path],
    expected: list[str],
) -> list[str]:
    """Return the conditions in *expected* that are absent from *mapping*."""
    return [c for c in expected if c not in mapping]
