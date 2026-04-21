"""Capability probes — narrow, deterministic observations on a single run.

End-to-end eval scores live at the mercy of scenario variance + judge
variance + run variance. Probes short-circuit that stack by measuring
specific capabilities directly from the tool-call transcript:

- NavigationProbe: given the symptom, did the agent open the right
  files in its first K Read/Grep calls? Precision@K, recall@K, first-
  hit step, distractor rate.

- GraphUsageProbe: did the agent invoke Aethyme's graph tooling
  (`aethyme find-facts`, `aethyme navigate`, MCP calls) before making
  conclusions? Crucial for answering "is the skill actually being used".

Probes take a normalized ToolCall sequence (see `Transcript`) plus a
ground-truth dict from `schemas.<eval_type>_probe_targets()` and return
a small, JSON-safe result dict.

Design notes
------------
- All path matching is *suffix-aware*: we match `includes/Page/WikiPage.php`
  against both `/Users/.../mediawiki/includes/Page/WikiPage.php` and the
  bare repo-relative form, since agents use both. No normalization
  heuristic will be perfect; suffix-match is conservative but robust.
- Missing-data paths (no tool calls captured) return {"skipped": True}
  rather than zeros, so the UI can distinguish "probe ran and agent
  missed" from "probe couldn't observe".
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


# Tool names we treat as "file-reading" for navigation probes. Grep
# counts — an agent that greps for the right keyword has navigated
# correctly even without opening the file first.
_NAV_TOOLS: frozenset[str] = frozenset({"Read", "Grep", "Glob"})

# Tool names / command prefixes that signal Aethyme graph-tool usage.
_AETHYME_CLI_PREFIXES: tuple[str, ...] = (
    "aethyme ",
    "python -m src.cli",
    "python3 -m src.cli",
)
_AETHYME_MCP_TOOLS: frozenset[str] = frozenset({
    # Extend as Aethyme exposes more MCP tools; keep loose match fallback.
    "aethyme_find_facts",
    "aethyme_navigate",
    "aethyme_context_pack",
})
# Tools that mutate files — used to decide "before first edit".
_MUTATING_TOOLS: frozenset[str] = frozenset({
    "Edit", "Write", "NotebookEdit", "MultiEdit",
})


@dataclass
class ToolCall:
    """One entry in an agent's tool-call stream.

    `order` is 1-indexed across the whole transcript. `name` matches
    the tool the agent invoked (Read, Grep, Bash, Edit, ...). `arguments`
    is the raw input dict (shape varies per tool; we access lazily).
    """
    order: int
    name: str
    arguments: dict[str, Any]


def _path_from_call(call: ToolCall) -> str | None:
    """Extract a file path from a navigation tool call.

    Tool arg shapes we know:
      Read:  {"file_path": "/abs/path"}
      Grep:  {"pattern": ..., "path": "/path"}  # path is optional
      Glob:  {"pattern": ..., "path": "/path"}  # path is optional
    """
    args = call.arguments or {}
    return (
        args.get("file_path")
        or args.get("path")
        or args.get("notebook_path")
    )


def _bash_command(call: ToolCall) -> str:
    """Best-effort command-string extraction from a Bash tool call."""
    args = call.arguments or {}
    cmd = args.get("command")
    return str(cmd) if cmd is not None else ""


def _matches_target(path: str, target: str) -> bool:
    """Suffix/substring match for paths.

    Agents use varied prefixes (absolute, repo-relative, package-relative)
    and the match has to be robust to all of them. Case is folded because
    real-world repos have mixed casing (MediaWiki has both `Watchlist/`
    and `watchlist/` in places, and macOS filesystems case-fold anyway).
    A strict case-sensitive match would miss genuine hits and give false
    "agent never opened the right file" signal.
    """
    if not path or not target:
        return False
    # Normalize both sides a little — strip trailing slash, flip backslash,
    # lowercase for cross-case robustness.
    p = path.replace("\\", "/").rstrip("/").lower()
    t = target.replace("\\", "/").rstrip("/").lower()
    return p.endswith(t) or t in p


def run_navigation_probe(
    calls: list[ToolCall],
    ground_truth: dict[str, Any],
) -> dict[str, Any]:
    """Score how well the agent's first K navigation calls hit must_open.

    Returns:
      {
        "first_hit_step": int | None,   # 1-indexed call order of first hit
        "precision_at_k": float,        # hits / k_examined
        "recall_at_k": float,           # hits / |must_open|
        "distractor_rate": float,       # distractor hits / k_examined
        "hits": [must_open paths hit],
        "distractors": [must_not_open paths hit],
        "k_budget": int,
        "k_examined": int,              # capped at len(nav calls)
        "skipped": False | True,
      }

    Returns {"skipped": True} when no navigation calls were observed.
    """
    k_budget = int(ground_truth.get("k_budget", 5))
    must_open: list[str] = list(ground_truth.get("must_open", []))
    must_not_open: list[str] = list(ground_truth.get("must_not_open", []))

    nav_calls = [c for c in calls if c.name in _NAV_TOOLS]
    if not nav_calls:
        return {
            "skipped": True,
            "reason": "no Read/Grep/Glob tool calls observed",
            "k_budget": k_budget,
        }

    first_k = nav_calls[:k_budget]
    hits: set[str] = set()
    distractors: set[str] = set()
    first_hit_step: int | None = None

    for idx, call in enumerate(first_k, 1):
        path = _path_from_call(call)
        if not path:
            continue
        for target in must_open:
            if _matches_target(path, target):
                hits.add(target)
                if first_hit_step is None:
                    first_hit_step = idx
                break
        for target in must_not_open:
            if _matches_target(path, target):
                distractors.add(target)
                break

    k_examined = len(first_k)
    return {
        "skipped": False,
        "first_hit_step": first_hit_step,
        "precision_at_k": round(len(hits) / k_examined, 3) if k_examined else 0.0,
        "recall_at_k": (
            round(len(hits) / len(must_open), 3) if must_open else 0.0
        ),
        "distractor_rate": (
            round(len(distractors) / k_examined, 3) if k_examined else 0.0
        ),
        "hits": sorted(hits),
        "distractors": sorted(distractors),
        "k_budget": k_budget,
        "k_examined": k_examined,
    }


def _is_aethyme_call(call: ToolCall) -> bool:
    """Did this tool call invoke Aethyme graph tooling?"""
    if call.name in _AETHYME_MCP_TOOLS:
        return True
    if call.name.startswith("aethyme_"):
        # Future-proof: any MCP tool namespaced to aethyme counts.
        return True
    if call.name == "Bash":
        cmd = _bash_command(call)
        return any(cmd.startswith(p) for p in _AETHYME_CLI_PREFIXES)
    return False


def run_graph_usage_probe(
    calls: list[ToolCall],
    _ground_truth: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Detect whether the agent consulted Aethyme tooling, and when.

    Answers two questions:
      - "Did Aethyme get used at all?"
      - "Did it get used *before* any code edit?" — which is the
        diagnostic pattern we care about (consult the graph, then act).

    Returns:
      {
        "aethyme_called": bool,
        "aethyme_call_count": int,
        "first_aethyme_step": int | None,
        "first_mutating_step": int | None,
        "aethyme_called_before_first_edit": bool | None,
        "skipped": False | True,
      }

    Returns {"skipped": True} when the transcript has no tool calls at all.
    """
    if not calls:
        return {"skipped": True, "reason": "empty tool-call stream"}

    aethyme_calls = [c for c in calls if _is_aethyme_call(c)]
    mutating_calls = [c for c in calls if c.name in _MUTATING_TOOLS]

    first_aethyme_step = aethyme_calls[0].order if aethyme_calls else None
    first_mutating_step = mutating_calls[0].order if mutating_calls else None

    if first_aethyme_step is None:
        before_first_edit: bool | None = False
    elif first_mutating_step is None:
        # Agent never edited — vacuously "before". Return None to
        # indicate "not applicable" rather than True; misreading as True
        # would inflate the metric for explain-repo style evals where
        # no edits are expected.
        before_first_edit = None
    else:
        before_first_edit = first_aethyme_step < first_mutating_step

    return {
        "skipped": False,
        "aethyme_called": bool(aethyme_calls),
        "aethyme_call_count": len(aethyme_calls),
        "first_aethyme_step": first_aethyme_step,
        "first_mutating_step": first_mutating_step,
        "aethyme_called_before_first_edit": before_first_edit,
    }


# ---------------------------------------------------------------------------
# Top-level dispatcher
# ---------------------------------------------------------------------------

def run_all_probes(
    calls: list[ToolCall],
    ground_truth: dict[str, Any] | None,
) -> dict[str, Any]:
    """Run every applicable probe on a transcript.

    ground_truth can be None — graph_usage still works without it, but
    navigation returns {"skipped": True, "reason": "no ground truth"}.
    """
    results: dict[str, Any] = {}
    if ground_truth is not None:
        results["navigation"] = run_navigation_probe(calls, ground_truth)
    else:
        results["navigation"] = {
            "skipped": True,
            "reason": "no probe_targets() defined for this eval_type",
        }
    results["graph_usage"] = run_graph_usage_probe(calls)
    return results
