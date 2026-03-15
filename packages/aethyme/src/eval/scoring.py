"""Scoring helpers for structured benchmark outputs."""

from __future__ import annotations

import json
import re
from typing import Any

from src.eval.schemas import BUG_FIX_PATH_KEYS, EXPLAIN_REPO_PATH_KEYS, NAVIGATION_CTF_PATH_KEYS, ONBOARDING_AUTH_PATH_KEYS


def parse_structured_output(stdout: str) -> dict[str, Any] | None:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    return payload if isinstance(payload, dict) else None


# ---------------------------------------------------------------------------
# Path normalization
# ---------------------------------------------------------------------------

_MARKDOWN_LINK_RE = re.compile(r"^\[([^\]]*)\]\(([^)]*)\)$")
_LINE_ANCHOR_RE = re.compile(r"#L\d+(-L\d+)?$")


def _normalize_path(value: Any, repo_path: str | None = None) -> Any:
    """Normalize a single path string for scoring comparison.

    Steps (in order):
    1. Return as-is if not a string or empty.
    2. Strip markdown link ``[text](url)`` — use URL if it looks like a file
       path (no ``http``), else use text.
    3. Strip line anchors ``#L\\d+(-L\\d+)?$``.
    4. Strip absolute repo prefix if *repo_path* provided.
    5. Strip leading ``./``.
    6. Strip trailing ``/``.
    """
    if not isinstance(value, str) or not value:
        return value

    # 2. Markdown links
    md = _MARKDOWN_LINK_RE.match(value)
    if md:
        text, url = md.group(1), md.group(2)
        # Use URL if it looks like a file path; else fall back to text
        if url and not url.startswith("http"):
            value = url
        else:
            value = text

    # 3. Line anchors
    value = _LINE_ANCHOR_RE.sub("", value)

    # 4. Absolute repo prefix
    if repo_path:
        prefix = repo_path.rstrip("/")
        if value.startswith(prefix + "/"):
            value = value[len(prefix) + 1 :]
        elif value == prefix:
            value = ""

    # 5. Description suffix (e.g. "path/to/file - description text")
    if " - " in value:
        candidate_path = value.split(" - ", 1)[0].strip()
        # Only strip if the prefix looks like a file path
        if "/" in candidate_path or candidate_path.endswith((".ts", ".tsx", ".js", ".py", ".json", ".md")):
            value = candidate_path

    # 6. Leading ./ (loop handles ././ etc.)
    while value.startswith("./"):
        value = value[2:]

    # 7. Trailing /
    value = value.rstrip("/")

    return value


# ---------------------------------------------------------------------------
# Guardrail diagnostics
# ---------------------------------------------------------------------------

def _extract_path_strings(
    data: Any, path_keys: frozenset[str] | None = None
) -> list[str]:
    """Recursively collect string values from path-like fields in *data*.

    When *path_keys* is provided (allowlist mode), only strings under dict
    keys present in the set are collected.  The walker always recurses into
    nested dicts and lists so that leaf-level keys are matched even inside
    deeply nested structures.

    When *path_keys* is ``None`` every string value is collected (legacy
    fallback — avoid in new code).
    """
    strings: list[str] = []
    if isinstance(data, dict):
        for key, val in data.items():
            if path_keys is not None and key in path_keys:
                # Key is a known path field — collect strings from its value.
                if isinstance(val, str) and val:
                    strings.append(val)
                elif isinstance(val, list):
                    strings.extend(s for s in val if isinstance(s, str) and s)
                else:
                    strings.extend(_extract_path_strings(val, path_keys))
            else:
                # Not a path key (or no allowlist) — recurse but only
                # collect if there's no allowlist.
                if path_keys is None and isinstance(val, str) and val:
                    strings.append(val)
                elif isinstance(val, (dict, list)):
                    strings.extend(_extract_path_strings(val, path_keys))
    elif isinstance(data, list):
        for item in data:
            if isinstance(item, str) and item:
                if path_keys is None:
                    strings.append(item)
                # With an allowlist, bare strings in a list are only
                # collected when the parent key matched (handled above).
            else:
                strings.extend(_extract_path_strings(item, path_keys))
    return strings


def _compute_guardrails(
    candidate: dict[str, Any],
    *,
    repo_path: str | None,
    raw_score: float,
    normalized_score: float,
    path_keys: frozenset[str] | None = None,
) -> dict[str, Any]:
    """Return diagnostic metrics about path formatting in *candidate*."""
    paths = _extract_path_strings(candidate, path_keys)
    absolute_count = 0
    markdown_link_count = 0
    line_anchor_count = 0
    for p in paths:
        md = _MARKDOWN_LINK_RE.match(p)
        if md:
            markdown_link_count += 1
            # Check the URL inside the link for absolute paths
            if md.group(2).startswith("/"):
                absolute_count += 1
        elif p.startswith("/"):
            absolute_count += 1
        if _LINE_ANCHOR_RE.search(p):
            line_anchor_count += 1

    # Compute delta from raw floats first, then round — avoids cascading
    # rounding error from round(round(a) - round(b)).
    delta = normalized_score - raw_score
    return {
        "path_format_issues": {
            "absolute_count": absolute_count,
            "markdown_link_count": markdown_link_count,
            "line_anchor_count": line_anchor_count,
        },
        "raw_score": round(raw_score, 2),
        "normalized_score": round(normalized_score, 2),
        "normalization_delta": round(delta, 2),
    }


# ---------------------------------------------------------------------------
# Public scoring functions
# ---------------------------------------------------------------------------


def score_explain_repo_output(
    candidate: dict[str, Any] | None,
    reference: dict[str, Any],
    repo_path: str | None = None,
) -> dict[str, Any] | None:
    if candidate is None:
        return None

    weights = {
        "code_areas": 20,
        "reference_areas": 10,
        "entrypoints": 20,
        "important_docs": 15,
        "key_configs": 10,
        "key_languages": 10,
        "high_risk_areas": 5,
        "navigation_order": 5,
        "representative_code_files": 3,
        "representative_docs": 2,
    }
    assert sum(weights.values()) == 100, f"explain-repo weights sum to {sum(weights.values())}, expected 100"

    def _score_all(rp: str | None) -> dict[str, float]:
        return {
            "code_areas": _list_score(candidate.get("code_areas"), reference.get("code_areas"), repo_path=rp),
            "reference_areas": _list_score(candidate.get("reference_areas"), reference.get("reference_areas"), repo_path=rp),
            "entrypoints": _list_score(candidate.get("entrypoints"), reference.get("entrypoints"), repo_path=rp),
            "important_docs": _list_score(candidate.get("important_docs"), reference.get("important_docs"), repo_path=rp),
            "key_configs": _list_score(candidate.get("key_configs"), reference.get("key_configs"), repo_path=rp),
            "key_languages": _list_score(candidate.get("key_languages"), reference.get("key_languages"), repo_path=rp),
            "high_risk_areas": _list_score(candidate.get("high_risk_areas"), reference.get("high_risk_areas"), repo_path=rp),
            "navigation_order": _ordered_list_score(candidate.get("navigation_order"), reference.get("navigation_order"), repo_path=rp),
            "representative_code_files": _list_score(candidate.get("representative_code_files"), reference.get("representative_code_files"), repo_path=rp),
            "representative_docs": _list_score(candidate.get("representative_docs"), reference.get("representative_docs"), repo_path=rp),
        }

    raw_scores = _score_all(None)
    normalized_scores = _score_all(repo_path)

    raw_total = sum(raw_scores[k] * weights[k] for k in raw_scores)
    normalized_total = sum(normalized_scores[k] * weights[k] for k in normalized_scores)

    result: dict[str, Any] = {
        "scores": normalized_scores,
        "weighted_score": round(normalized_total, 2),
        "max_score": 100,
    }
    result["guardrails"] = _compute_guardrails(
        candidate,
        repo_path=repo_path,
        raw_score=raw_total,
        normalized_score=normalized_total,
        path_keys=EXPLAIN_REPO_PATH_KEYS,
    )
    return result


def score_navigation_ctf_output(
    candidate: dict[str, Any] | None,
    reference: dict[str, Any],
    repo_path: str | None = None,
) -> dict[str, Any] | None:
    if candidate is None:
        return None

    weights = {
        "config_target": 30,
        "code_target": 30,
        "management_area": 20,
        "relationship_chain": 20,
    }
    assert sum(weights.values()) == 100, f"navigation-ctf weights sum to {sum(weights.values())}, expected 100"

    def _score_all(rp: str | None) -> dict[str, float]:
        return {
            "config_target": _exact_path_score(
                candidate.get("config_target", {}).get("path"),
                reference.get("config_target", {}).get("path"),
                repo_path=rp,
            ),
            "code_target": _exact_path_score(
                candidate.get("code_target", {}).get("path"),
                reference.get("code_target", {}).get("path"),
                repo_path=rp,
            ),
            "management_area": _exact_path_score(
                candidate.get("management_area", {}).get("name"),
                reference.get("management_area", {}).get("name"),
                repo_path=rp,
            ),
            "relationship_chain": _relationship_chain_score(
                candidate.get("relationship_chain"),
                reference.get("relationship_chain"),
                repo_path=rp,
            ),
        }

    raw_scores = _score_all(None)
    normalized_scores = _score_all(repo_path)

    raw_total = sum(raw_scores[k] * weights[k] for k in raw_scores)
    normalized_total = sum(normalized_scores[k] * weights[k] for k in normalized_scores)

    result: dict[str, Any] = {
        "scores": normalized_scores,
        "weighted_score": round(normalized_total, 2),
        "max_score": 100,
    }
    result["guardrails"] = _compute_guardrails(
        candidate,
        repo_path=repo_path,
        raw_score=raw_total,
        normalized_score=normalized_total,
        path_keys=NAVIGATION_CTF_PATH_KEYS,
    )
    return result


# ---------------------------------------------------------------------------
# Internal scoring helpers
# ---------------------------------------------------------------------------


def _list_score(
    candidate: object, reference: object, *, repo_path: str | None = None
) -> float:
    candidate_list = _normalize_list(candidate, repo_path=repo_path)
    reference_list = _normalize_list(reference, repo_path=repo_path)
    if not reference_list:
        return 1.0
    matched = sum(1 for item in reference_list if item in candidate_list)
    return matched / len(reference_list)


def _ordered_list_score(
    candidate: object, reference: object, *, repo_path: str | None = None
) -> float:
    candidate_list = _normalize_list(candidate, repo_path=repo_path)
    reference_list = _normalize_list(reference, repo_path=repo_path)
    if not reference_list:
        return 1.0
    matched = 0.0
    for index, item in enumerate(reference_list):
        if index < len(candidate_list) and candidate_list[index] == item:
            matched += 1.0
        elif item in candidate_list:
            matched += 0.5
    return matched / len(reference_list)


def _exact_path_score(
    candidate: object, reference: object, *, repo_path: str | None = None
) -> float:
    if not isinstance(reference, str) or not reference:
        return 1.0
    c = _normalize_path(candidate, repo_path) if isinstance(candidate, str) else candidate
    r = _normalize_path(reference, repo_path)
    return 1.0 if c == r else 0.0


def _relationship_chain_score(
    candidate: object, reference: object, *, repo_path: str | None = None
) -> float:
    if not isinstance(reference, list) or not reference:
        return 1.0
    if not isinstance(candidate, list):
        return 0.0

    def _norm_pairs(items: list[Any]) -> set[tuple[str, str, str]]:
        pairs: set[tuple[str, str, str]] = set()
        for item in items:
            if not isinstance(item, dict):
                continue
            f = item.get("from", "")
            t = item.get("to", "")
            r = item.get("relation", "")
            f = _normalize_path(f, repo_path) if isinstance(f, str) else f
            t = _normalize_path(t, repo_path) if isinstance(t, str) else t
            pairs.add((f, t, r))
        return pairs

    candidate_pairs = _norm_pairs(candidate)
    reference_pairs = _norm_pairs(reference)
    if not reference_pairs:
        return 1.0
    matched = sum(1 for item in reference_pairs if item in candidate_pairs)
    return matched / len(reference_pairs)


def _normalize_list(value: object, *, repo_path: str | None = None) -> list[str]:
    if not isinstance(value, list):
        return []
    return [_normalize_path(item, repo_path) for item in value if isinstance(item, str)]


def _keyword_score(text: object, keywords: list[str]) -> float:
    """Score prose text by fraction of required keywords present (case-insensitive)."""
    if not isinstance(text, str) or not text:
        return 0.0
    if not keywords:
        return 1.0
    lower = text.lower()
    matched = sum(1 for kw in keywords if kw.lower() in lower)
    return matched / len(keywords)


def _efficiency_score(cost_usd: float, reference_cost: float = 0.10) -> float:
    """Continuous efficiency score from cost (USD).

    Uses ``reference_cost / (reference_cost + cost_usd)`` — a smooth,
    monotonically decreasing function where:

    - cost = 0          → 1.0  (free is perfect)
    - cost = ref        → 0.5  (reference cost scores 50%)
    - cost = 2 × ref    → 0.33
    - cost → ∞          → 0.0

    *reference_cost* is the "par" cost for the task — the cost at which
    an agent scores 50% on efficiency.  Default $0.10 is calibrated for
    a single bug-fix on a cheap model.  Harder tasks or expensive models
    should pass a higher reference.

    Cost is the only metric comparable across all backends — it naturally
    normalizes caching strategies, token types, and model pricing.
    """
    if cost_usd <= 0:
        return 0.0
    return reference_cost / (reference_cost + cost_usd)


_BUG_FIX_WEIGHTS = {
    "fix_test": 60,
    "regression": 20,
    "correct_file": 10,
    "efficiency": 10,
}

_CROSS_PACKAGE_WEIGHTS = {
    "fix_test": 40,
    "regression": 15,
    "correct_file": 15,
    "efficiency": 30,
}


def score_bug_fix_output(
    candidate: dict[str, Any] | None,
    reference: dict[str, Any],
    *,
    test_pass: bool = False,
    regression_pass: bool = False,
    cost_usd: float = 0.0,
    tokens_used: int = 0,
    repo_path: str | None = None,
    scenario: str = "implication-share",
) -> dict[str, Any] | None:
    """Score a bug-fix eval result.

    Primary scoring is test pass/fail (objective).  Secondary is file
    identification and efficiency.

    Parameters
    ----------
    candidate:
        Structured output from the agent (``bug_file``, ``root_cause``, etc.).
    reference:
        Ground truth with ``bug_file`` and ``bug_description``.
    test_pass:
        Whether the planted test passes after the agent's changes.
    regression_pass:
        Whether all existing auth tests still pass.
    cost_usd:
        Total cost in USD for this agent run.  This is the primary
        efficiency metric — comparable across all backends and models.
    tokens_used:
        Total tokens consumed (stored for reference, not used in scoring).
    repo_path:
        Absolute repo path for path normalization.
    scenario:
        Which bug scenario weights to use.  ``"implication-share"`` uses the
        original weights (fix-heavy).  ``"cross-package"`` weights efficiency
        at 30% since it's the primary differentiator.
    """
    if candidate is None:
        return None

    weights = (
        _CROSS_PACKAGE_WEIGHTS if scenario == "cross-package"
        else _BUG_FIX_WEIGHTS
    )
    assert sum(weights.values()) == 100

    scores: dict[str, float] = {
        "fix_test": 1.0 if test_pass else 0.0,
        "regression": 1.0 if regression_pass else 0.0,
        "correct_file": _exact_path_score(
            candidate.get("bug_file", ""),
            reference.get("bug_file", ""),
            repo_path=repo_path,
        ),
        "efficiency": _efficiency_score(cost_usd),
    }

    weighted = sum(scores[k] * weights[k] for k in weights)

    result: dict[str, Any] = {
        "scores": scores,
        "weighted_score": round(weighted, 2),
        "max_score": 100,
        "test_pass": test_pass,
        "regression_pass": regression_pass,
        "cost_usd": cost_usd,
        "tokens_used": tokens_used,
        "scenario": scenario,
        "weights": weights,
    }
    result["guardrails"] = _compute_guardrails(
        candidate,
        repo_path=repo_path,
        raw_score=weighted,
        normalized_score=weighted,
        path_keys=BUG_FIX_PATH_KEYS,
    )
    return result


def score_mediawiki_bug_fix_1(
    candidate: dict[str, Any] | None,
    reference: dict[str, Any],
    *,
    cost_usd: float = 0.0,
    tokens_used: int = 0,
    repo_path: str | None = None,
) -> dict[str, Any]:
    """Score the MediaWiki bug-fix-1 diagnostic evaluation (T419918).

    The agent must identify files to edit and explain the fix approach.
    No actual code modification is evaluated — this is purely diagnostic.
    """
    from .schemas import MEDIAWIKI_BUG_FIX_1_PATH_KEYS

    weights = {
        "files_identified": 40,
        "root_cause_quality": 30,
        "fix_plan_quality": 20,
        "efficiency": 10,
    }

    if candidate is None:
        return {
            "scores": {k: 0.0 for k in weights},
            "weights": weights,
            "weighted_score": 0.0,
            "max_score": 100,
        }

    # --- Files identified (40%) ---
    # Set overlap on normalized file paths
    ref_files = {
        _normalize_path(f["path"], repo_path)
        for f in reference.get("files_to_edit", [])
    }
    candidate_files_raw = candidate.get("files_to_edit", [])
    if isinstance(candidate_files_raw, list):
        cand_files = {
            _normalize_path(
                f["path"] if isinstance(f, dict) else f,
                repo_path,
            )
            for f in candidate_files_raw
        }
    else:
        cand_files = set()

    if ref_files:
        files_score = len(ref_files & cand_files) / len(ref_files)
    else:
        files_score = 0.0

    # --- Root cause quality (30%) ---
    root_cause_text = candidate.get("root_cause", "")
    root_cause_kws = reference.get("root_cause_keywords", [])
    root_cause_score = _keyword_score(root_cause_text, root_cause_kws)

    # --- Fix plan quality (20%) ---
    fix_plan_text = candidate.get("fix_plan", "")
    fix_plan_kws = reference.get("fix_plan_keywords", [])
    fix_plan_score = _keyword_score(fix_plan_text, fix_plan_kws)

    # --- Efficiency (10%) ---
    eff_score = _efficiency_score(cost_usd, reference_cost=0.20)

    scores = {
        "files_identified": files_score,
        "root_cause_quality": root_cause_score,
        "fix_plan_quality": fix_plan_score,
        "efficiency": eff_score,
    }

    weighted = sum(scores[k] * weights[k] for k in weights)

    guardrails = _compute_guardrails(
        candidate,
        repo_path=repo_path,
        raw_score=weighted,
        normalized_score=weighted,
        path_keys=MEDIAWIKI_BUG_FIX_1_PATH_KEYS,
    )

    return {
        "scores": scores,
        "weights": weights,
        "weighted_score": round(weighted, 2),
        "max_score": 100,
        "files_matched": sorted(ref_files & cand_files),
        "files_missed": sorted(ref_files - cand_files),
        "files_extra": sorted(cand_files - ref_files),
        "guardrails": guardrails,
    }


def score_onboarding_auth_output(
    candidate: dict[str, Any] | None,
    reference: dict[str, Any],
    repo_path: str | None = None,
) -> dict[str, Any] | None:
    """Score the onboarding auth explanation challenge.

    Uses a mix of exact matching (paths, sets) and keyword presence
    (prose fields) to evaluate quality of the explanation.
    """
    if candidate is None:
        return None

    weights = {
        "auth_package": 10,
        "key_files": 25,
        "session_keywords": 15,
        "rbac_keywords": 15,
        "logout_keywords": 10,
        "consumers": 10,
        "dependencies": 15,
    }
    assert sum(weights.values()) == 100

    def _score_all(rp: str | None) -> dict[str, float]:
        return {
            "auth_package": _exact_path_score(
                candidate.get("auth_package"),
                reference["auth_package"],
                repo_path=rp,
            ),
            "key_files": _list_score(
                candidate.get("key_files"),
                reference["key_files"],
                repo_path=rp,
            ),
            "session_keywords": _keyword_score(
                candidate.get("session_management"),
                reference["session_keywords"],
            ),
            "rbac_keywords": _keyword_score(
                candidate.get("rbac_model"),
                reference["rbac_keywords"],
            ),
            "logout_keywords": _keyword_score(
                candidate.get("logout_mechanism"),
                reference["logout_keywords"],
            ),
            "consumers": _list_score(
                candidate.get("consumers"),
                reference["consumers"],
                repo_path=rp,
            ),
            "dependencies": _list_score(
                candidate.get("dependencies"),
                reference["dependencies"],
                repo_path=rp,
            ),
        }

    raw_scores = _score_all(None)
    normalized_scores = _score_all(repo_path)

    raw_total = sum(raw_scores[k] * weights[k] for k in raw_scores)
    normalized_total = sum(normalized_scores[k] * weights[k] for k in normalized_scores)

    result: dict[str, Any] = {
        "scores": normalized_scores,
        "weighted_score": round(normalized_total, 2),
        "max_score": 100,
    }
    result["guardrails"] = _compute_guardrails(
        candidate,
        repo_path=repo_path,
        raw_score=raw_total,
        normalized_score=normalized_total,
        path_keys=ONBOARDING_AUTH_PATH_KEYS,
    )
    return result
