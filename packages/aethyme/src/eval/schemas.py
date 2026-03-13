"""Structured output schemas for local Aethyme benchmarks."""

from __future__ import annotations


# ---------------------------------------------------------------------------
# Path key sets — which dict keys carry path-like strings for guardrails.
#
# _extract_path_strings walks a candidate dict recursively.  When it finds a
# dict key that is in ``path_keys``:
#   - if the value is a string  → collect it
#   - if the value is a list    → collect all strings from it
# It always recurses into nested dicts and lists regardless of key name, so
# leaf-level keys like "path" or "from" are matched even inside nested objects.
# ---------------------------------------------------------------------------

EXPLAIN_REPO_PATH_KEYS: frozenset[str] = frozenset({
    "code_areas",
    "reference_areas",
    "entrypoints",
    "important_docs",
    "key_configs",
    "high_risk_areas",
    "navigation_order",
    "representative_code_files",
    "representative_docs",
    # Excluded: repo_summary (prose), evidence (not scored),
    #           key_languages (language names, not file paths)
})

NAVIGATION_CTF_PATH_KEYS: frozenset[str] = frozenset({
    "path",   # config_target.path, code_target.path, rejected_candidates[].path
    "name",   # management_area.name
    "from",   # relationship_chain[].from
    "to",     # relationship_chain[].to
    # Excluded: why, reason, relation, confidence (all prose)
})


def explain_repo_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "repo_summary",
            "code_areas",
            "reference_areas",
            "entrypoints",
            "important_docs",
            "key_configs",
            "key_languages",
            "high_risk_areas",
            "navigation_order",
            "representative_code_files",
            "representative_docs",
            "evidence",
        ],
        "properties": {
            "repo_summary": {"type": "string"},
            "code_areas": {"type": "array", "items": {"type": "string"}},
            "reference_areas": {"type": "array", "items": {"type": "string"}},
            "entrypoints": {"type": "array", "items": {"type": "string"}},
            "important_docs": {"type": "array", "items": {"type": "string"}},
            "key_configs": {"type": "array", "items": {"type": "string"}},
            "key_languages": {"type": "array", "items": {"type": "string"}},
            "high_risk_areas": {"type": "array", "items": {"type": "string"}},
            "navigation_order": {"type": "array", "items": {"type": "string"}},
            "representative_code_files": {"type": "array", "items": {"type": "string"}},
            "representative_docs": {"type": "array", "items": {"type": "string"}},
            "evidence": {"type": "array", "items": {"type": "string"}},
        },
    }


def explain_repo_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
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
        },
        "notes": [
            "Prefer exact path and area matches.",
            "Navigation order is partial-credit and ordered.",
            "Repo summary is informative but not currently machine-scored.",
            "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison.",
        ],
    }


def navigation_ctf_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "config_target",
            "code_target",
            "management_area",
            "relationship_chain",
            "rejected_candidates",
            "confidence",
        ],
        "properties": {
            "config_target": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "why"],
                "properties": {
                    "path": {"type": "string"},
                    "why": {"type": "string"},
                },
            },
            "code_target": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "why"],
                "properties": {
                    "path": {"type": "string"},
                    "why": {"type": "string"},
                },
            },
            "management_area": {
                "type": "object",
                "additionalProperties": False,
                "required": ["name", "why"],
                "properties": {
                    "name": {"type": "string"},
                    "why": {"type": "string"},
                },
            },
            "relationship_chain": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["from", "to", "relation"],
                    "properties": {
                        "from": {"type": "string"},
                        "to": {"type": "string"},
                        "relation": {"type": "string"},
                    },
                },
            },
            "rejected_candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["path", "reason"],
                    "properties": {
                        "path": {"type": "string"},
                        "reason": {"type": "string"},
                    },
                },
            },
            "confidence": {"type": "string"},
        },
    }


def navigation_ctf_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "config_target": 30,
            "code_target": 30,
            "management_area": 20,
            "relationship_chain": 20,
        },
        "notes": [
            "Exact config/code path matches carry most of the score.",
            "Relationship chain must express both ownership and management links.",
            "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison.",
        ],
    }


# ---------------------------------------------------------------------------
# Onboarding challenge: "Explain how authentication is managed"
# ---------------------------------------------------------------------------

ONBOARDING_AUTH_PATH_KEYS: frozenset[str] = frozenset({
    "auth_package",
    "key_files",
    "consumers",
    "dependencies",
})


BUG_FIX_PATH_KEYS: frozenset[str] = frozenset({
    "bug_file",
})


def bug_fix_output_schema() -> dict[str, object]:
    """Schema for the bug-fix evaluation challenge."""
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "bug_file",
            "root_cause",
            "fix_applied",
            "fix_description",
        ],
        "properties": {
            "bug_file": {
                "type": "string",
                "description": "Path to the file containing the bug",
            },
            "root_cause": {
                "type": "string",
                "description": "What caused the test to fail",
            },
            "fix_applied": {
                "type": "boolean",
                "description": "Did you modify a file to fix the bug?",
            },
            "fix_description": {
                "type": "string",
                "description": "What change did you make?",
            },
        },
    }


def bug_fix_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "fix_test": 60,
            "regression": 20,
            "correct_file": 10,
            "efficiency": 10,
        },
        "notes": [
            "fix_test: vitest run on the planted test — binary pass/fail.",
            "regression: vitest run on all auth tests — no regressions introduced.",
            "correct_file: agent identifies the correct file in structured output.",
            "efficiency: lower token usage scores higher.",
        ],
    }


def onboarding_auth_output_schema() -> dict[str, object]:
    """Schema for the onboarding auth explanation challenge."""
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "auth_package",
            "key_files",
            "summary",
            "session_management",
            "rbac_model",
            "logout_mechanism",
            "consumers",
            "dependencies",
        ],
        "properties": {
            "auth_package": {
                "type": "string",
                "description": "Path to the auth package directory",
            },
            "key_files": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Key files in the auth package with their roles",
            },
            "summary": {
                "type": "string",
                "description": "One-paragraph summary of how auth works",
            },
            "session_management": {
                "type": "string",
                "description": "How sessions are fetched, cached, and invalidated",
            },
            "rbac_model": {
                "type": "string",
                "description": "How RBAC/permissions work (actions, resources, CASL)",
            },
            "logout_mechanism": {
                "type": "string",
                "description": "How logout works including cross-tab broadcast",
            },
            "consumers": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Packages or files that import from @aeptus/auth",
            },
            "dependencies": {
                "type": "array",
                "items": {"type": "string"},
                "description": "External dependencies the auth package relies on",
            },
        },
    }


def onboarding_auth_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "auth_package": 10,
            "key_files": 25,
            "session_keywords": 15,
            "rbac_keywords": 15,
            "logout_keywords": 10,
            "consumers": 10,
            "dependencies": 15,
        },
        "notes": [
            "auth_package: exact path match to packages/auth.",
            "key_files: set overlap with reference file list.",
            "session/rbac/logout: keyword presence in prose fields.",
            "consumers: set overlap with known importers.",
            "dependencies: set overlap with known deps (@aeptus/types, @casl/ability).",
        ],
    }
