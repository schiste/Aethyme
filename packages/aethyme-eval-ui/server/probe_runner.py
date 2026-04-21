"""Runs capability probes on an eval row by parsing its Claude session JSONL.

Piggybacks on completed eval runs: takes the session file path + optional
sub-agents directory, extracts an ordered ToolCall sequence, loads the
ground-truth probe_targets() from schemas, runs every probe, and stores
each result in eval_probes.

Scope
-----
Only handles Claude Code JSONL sessions for now. Codex-driven eval runs
use a different transcript format (Chau7's `run_tool_calls` MCP) and are
left as a future extension. When probe_runner is called on a codex row,
the probe results are marked {"skipped": True, "reason": "codex
transcript not yet supported"}.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


# We need src.eval.probes from the aethyme package. server/main.py sets
# up sys.path for this already; doing it here too makes the module
# independently importable (useful in tests).
_AETHYME_PKG = Path(__file__).resolve().parent.parent.parent / "aethyme"
if str(_AETHYME_PKG) not in sys.path:
    sys.path.insert(0, str(_AETHYME_PKG))

from src.eval.probes import (  # noqa: E402
    ToolCall,
    run_all_probes,
)


def _iter_tool_uses_from_session(session_file: Path) -> list[tuple[str, dict]]:
    """Yield (tool_name, arguments) pairs in order from a Claude JSONL session.

    Assistant messages in the stream contain a `content` list; tool_use
    blocks have shape {type: "tool_use", name, input}. We ignore everything
    else (thinking, text, tool_result).
    """
    pairs: list[tuple[str, dict]] = []
    if not session_file.exists():
        return pairs
    try:
        with open(session_file, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if msg.get("type") != "assistant":
                    continue
                content = msg.get("message", {}).get("content", [])
                if not isinstance(content, list):
                    continue
                for block in content:
                    if (
                        isinstance(block, dict)
                        and block.get("type") == "tool_use"
                    ):
                        pairs.append((
                            str(block.get("name", "?")),
                            block.get("input", {}) or {},
                        ))
    except OSError:
        pass
    return pairs


def _extract_tool_calls(
    session_file: Path | None,
    sub_agents_dir: Path | None = None,
) -> list[ToolCall]:
    """Extract an ordered ToolCall list from a Claude session + sub-agent JSONLs.

    Sub-agents (Task tool invocations that spawn their own session) are
    appended AFTER the parent stream. This is a simplification — strictly,
    sub-agent calls interleave with the parent — but matches how the
    existing main.py session counter rolls them up.
    """
    pairs: list[tuple[str, dict]] = []
    if session_file is not None:
        pairs.extend(_iter_tool_uses_from_session(session_file))
    if sub_agents_dir is not None and sub_agents_dir.exists():
        for sub_file in sorted(sub_agents_dir.glob("*.jsonl")):
            pairs.extend(_iter_tool_uses_from_session(sub_file))
    return [
        ToolCall(order=i + 1, name=name, arguments=args)
        for i, (name, args) in enumerate(pairs)
    ]


@dataclass
class ProbeRunResult:
    result_id: str
    probe_results: dict[str, dict[str, Any]]  # probe_name -> result dict
    tool_call_count: int


def _load_probe_targets(aethyme_pkg: Path, eval_type: str) -> dict[str, Any] | None:
    """Look up <eval_type>_probe_targets() in schemas.py.

    Mirrors the lookup strategy used by calibration_check — try
    mediawiki-prefixed first, then plain.
    """
    if str(aethyme_pkg) not in sys.path:
        sys.path.insert(0, str(aethyme_pkg))
    try:
        from src.eval import schemas  # type: ignore
    except Exception:
        return None
    key = eval_type.replace("-", "_")
    for candidate in (f"mediawiki_{key}_probe_targets", f"{key}_probe_targets"):
        fn = getattr(schemas, candidate, None)
        if callable(fn):
            try:
                out = fn()
                return out if isinstance(out, dict) else None
            except Exception:
                return None
    return None


def run_probes_on_row(
    *,
    result_id: str,
    eval_type: str,
    session_file: Path | None,
    sub_agents_dir: Path | None = None,
    aethyme_pkg: Path,
    store: bool = True,
) -> ProbeRunResult:
    """Extract tool calls from the session, run probes, optionally persist.

    `session_file`/`sub_agents_dir` may be None — probes then return
    {"skipped": True} and we still record that (so the UI can show "probe
    ran but had no data" rather than "probe never ran").

    `store=False` is useful in tests or for dry-runs.
    """
    calls = _extract_tool_calls(session_file, sub_agents_dir)
    ground_truth = _load_probe_targets(aethyme_pkg, eval_type)
    results = run_all_probes(calls, ground_truth)

    if store:
        from db import insert_probe_result  # local import to avoid cycles
        for probe_name, res in results.items():
            insert_probe_result(
                result_id=result_id,
                probe_name=probe_name,
                result=res,
            )

    return ProbeRunResult(
        result_id=result_id,
        probe_results=results,
        tool_call_count=len(calls),
    )
