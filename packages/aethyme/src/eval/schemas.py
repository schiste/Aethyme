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


# ---------------------------------------------------------------------------
# MediaWiki bug-fix-1: T419918 — watchlist marks all revisions as seen
# ---------------------------------------------------------------------------

MEDIAWIKI_BUG_FIX_1_PATH_KEYS: frozenset[str] = frozenset({
    "files_to_edit",
})


def mediawiki_bug_fix_1_output_schema() -> dict[str, object]:
    """Schema for the MediaWiki bug-fix-1 diagnostic evaluation.

    The agent must identify which files to edit and explain how to fix the bug.
    It must NOT actually apply the fix.
    """
    return {
        "type": "object",
        "additionalProperties": False,
        "required": [
            "files_to_edit",
            "root_cause",
            "fix_plan",
        ],
        "properties": {
            "files_to_edit": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["path", "what_to_change"],
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path to the file that needs editing",
                        },
                        "what_to_change": {
                            "type": "string",
                            "description": "What specifically needs to change in this file",
                        },
                    },
                },
                "description": "Files that need editing to fix the bug",
            },
            "root_cause": {
                "type": "string",
                "description": (
                    "Technical explanation of why the bug occurs — "
                    "what code path leads to the wrong behavior"
                ),
            },
            "fix_plan": {
                "type": "string",
                "description": (
                    "Step-by-step explanation of how you would fix the bug. "
                    "Do NOT apply the fix — describe what changes are needed."
                ),
            },
        },
    }


def mediawiki_bug_fix_1_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "files_identified": 40,
            "root_cause_quality": 30,
            "fix_plan_quality": 20,
            "efficiency": 10,
        },
        "notes": [
            "files_identified: set overlap of paths with reference. "
            "Partial credit per file.",
            "root_cause_quality: keyword match for key concepts "
            "(RevisionRecord, integer vs object, doViewUpdates, showDiffPage).",
            "fix_plan_quality: keyword match for fix approach "
            "(pass RevisionRecord, deprecate $oldid, signature change).",
            "efficiency: lower token usage scores higher.",
        ],
    }


def mediawiki_bug_fix_1_reference() -> dict[str, object]:
    """Ground truth derived from commit 425c358d279e0610365cda8fbe01d889f11238f0."""
    return {
        "bug_id": "T419918",
        "title": "Viewing a diff/revision on a watchlisted page marks all revisions as seen",
        "files_to_edit": [
            {
                "path": "includes/Page/WikiPage.php",
                "what_to_change": (
                    "Change doViewUpdates() signature: replace $oldid integer param "
                    "with $oldRev RevisionRecord param. Add deprecation shim for "
                    "callers still passing an integer."
                ),
            },
            {
                "path": "includes/Page/Article.php",
                "what_to_change": (
                    "Update 3 call sites of doViewUpdates(): stop passing $oldid "
                    "integer, pass RevisionRecord object instead. In showDiffPage(), "
                    "use $de->getNewRevision() instead of (int)$new."
                ),
            },
            {
                "path": "includes/Page/ImagePage.php",
                "what_to_change": (
                    "Update doViewUpdates() call: replace $this->getOldID() "
                    "with $this->fetchRevisionRecord()."
                ),
            },
            {
                "path": "RELEASE-NOTES-1.46",
                "what_to_change": (
                    "Document that passing oldid to WikiPage::doViewUpdates "
                    "is deprecated; pass RevisionRecord instead."
                ),
            },
        ],
        "root_cause": (
            "Article::showDiffPage() passes a revision ID (integer) to "
            "WikiPage::doViewUpdates(), but the downstream code in "
            "WatchlistManager::clearTitleUserNotifications() needs a "
            "RevisionRecord to correctly identify which specific revision "
            "was viewed. When an integer is passed, the notification clearing "
            "code marks ALL revisions as seen instead of just the one being "
            "viewed."
        ),
        "root_cause_keywords": [
            "integer", "RevisionRecord", "doViewUpdates", "showDiffPage",
            "clearTitleUserNotifications", "WatchlistManager", "oldid",
            "revision", "diff",
        ],
        "fix_plan": (
            "1. Change WikiPage::doViewUpdates() signature to accept "
            "RevisionRecord|null instead of int $oldid. "
            "2. Add a deprecation shim using func_num_args() to detect "
            "callers passing the old integer signature. "
            "3. Update Article.php call sites: remove $oldid param, pass "
            "RevisionRecord objects (fetchRevisionRecord() or "
            "getNewRevision()). "
            "4. Update ImagePage.php: replace getOldID() with "
            "fetchRevisionRecord(). "
            "5. Update RELEASE-NOTES-1.46 to document the deprecation."
        ),
        "fix_plan_keywords": [
            "RevisionRecord", "deprecat", "signature", "func_num_args",
            "fetchRevisionRecord", "getNewRevision", "getOldID",
        ],
        "commit": "425c358d279e0610365cda8fbe01d889f11238f0",
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


# ---------------------------------------------------------------------------
# MediaWiki: Impact Analysis — "What breaks if I change this?"
# ---------------------------------------------------------------------------

def mediawiki_impact_analysis_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["call_sites", "summary"],
        "properties": {
            "call_sites": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["file", "line", "code"],
                    "properties": {
                        "file": {"type": "string"},
                        "line": {"type": "integer"},
                        "code": {"type": "string"},
                    },
                },
            },
            "summary": {"type": "string"},
        },
    }


def mediawiki_impact_analysis_reference() -> dict[str, object]:
    return {
        "call_sites": [
            {"file": "includes/Page/Article.php", "line": 541},
            {"file": "includes/Page/Article.php", "line": 586},
            {"file": "includes/Page/Article.php", "line": 733},
            {"file": "includes/Page/Article.php", "line": 1149},
            {"file": "includes/Page/ImagePage.php", "line": 156},
            {"file": "includes/Actions/ActionEntryPoint.php", "line": 134},
        ],
        "reference_files": [
            "includes/Page/Article.php",
            "includes/Page/ImagePage.php",
            "includes/Actions/ActionEntryPoint.php",
        ],
        "target": "WikiPage::doViewUpdates()",
    }


# ---------------------------------------------------------------------------
# MediaWiki: Feature Localization — "Where does this behavior live?"
# ---------------------------------------------------------------------------

def mediawiki_feature_localization_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["chain", "summary"],
        "properties": {
            "chain": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["file", "method", "description"],
                    "properties": {
                        "file": {"type": "string"},
                        "method": {"type": "string"},
                        "description": {"type": "string"},
                    },
                },
            },
            "summary": {"type": "string"},
        },
    }


def mediawiki_feature_localization_reference() -> dict[str, object]:
    return {
        "chain": [
            {"file": "includes/Actions/WatchAction.php", "method": "onSubmit"},
            {"file": "includes/Watchlist/WatchlistManager.php", "method": "setWatch"},
            {"file": "includes/Watchlist/WatchlistManager.php", "method": "addWatchIgnoringRights"},
            {"file": "includes/Watchlist/WatchedItemStore.php", "method": "addWatch"},
        ],
        "reference_files": [
            "includes/Actions/WatchAction.php",
            "includes/Watchlist/WatchlistManager.php",
            "includes/Watchlist/WatchedItemStore.php",
        ],
        "entry_class": "WatchAction",
        "terminal_class": "WatchedItemStore",
    }


# ---------------------------------------------------------------------------
# MediaWiki: Configuration Audit — "What controls this behavior?"
# ---------------------------------------------------------------------------

def mediawiki_config_audit_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["config_variable", "default_location", "enforcement_class", "disable_method"],
        "properties": {
            "config_variable": {"type": "string"},
            "default_location": {"type": "string"},
            "enforcement_class": {"type": "string"},
            "disable_method": {"type": "string"},
        },
    }


def mediawiki_config_audit_reference() -> dict[str, object]:
    return {
        "config_variable": "RateLimits",
        "config_variable_aliases": ["$wgRateLimits", "MainConfigNames::RateLimits", "RateLimits"],
        "default_location": "includes/MainConfigSchema.php",
        "enforcement_class": "RateLimiter",
        "enforcement_file": "includes/Permissions/RateLimiter.php",
        "disable_keywords": ["RateLimits", "empty", "RateLimitsExcludedIPs", "exclude"],
    }


# ---------------------------------------------------------------------------
# dead-code — find unused public methods in the Watchlist subsystem
# ---------------------------------------------------------------------------

MEDIAWIKI_DEAD_CODE_PATH_KEYS: frozenset[str] = frozenset({"unused_functions"})


def mediawiki_dead_code_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["unused_functions"],
        "properties": {
            "unused_functions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["function_name", "defined_in", "reason"],
                    "properties": {
                        "function_name": {"type": "string"},
                        "defined_in": {"type": "string"},
                        "reason": {"type": "string"},
                    },
                },
            },
        },
    }


def mediawiki_dead_code_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "functions_found": 60,
            "false_positives": 20,
            "efficiency": 20,
        },
        "notes": [
            "functions_found: recall — how many of the 9 truly unused functions were identified.",
            "false_positives: precision — penalty for listing functions that ARE used externally.",
            "efficiency: cost relative to $1.00 baseline.",
        ],
    }


def mediawiki_dead_code_reference() -> dict[str, object]:
    """Public functions in includes/Watchlist/ with no callers outside the subsystem.

    Ground truth generated via:
        grep -n 'public function' includes/Watchlist/*.php | grep -v __construct |
        for each: grep -rl '<func>' --include='*.php' . | grep -v includes/Watchlist/ |
                  grep -v /tests/ | grep -v vendor/ | wc -l
        Functions with 0 external callers are listed here.
    """
    return {
        "unused_functions": [
            {"function_name": "buildTools", "defined_in": "includes/Watchlist/SpecialEditWatchlist.php"},
            {"function_name": "countAllForUser", "defined_in": "includes/Watchlist/WatchedItemQueryService.php"},
            {"function_name": "duplicateEntry", "defined_in": "includes/Watchlist/WatchedItemQueryService.php"},
            {"function_name": "isTempWatchedIgnoringRights", "defined_in": "includes/Watchlist/WatchlistManager.php"},
            {"function_name": "modifyWatchedItemsWithRCInfo", "defined_in": "includes/Watchlist/WatchedItemQueryService.php"},
            {"function_name": "modifyWatchedItemsWithRCInfoQuery", "defined_in": "includes/Watchlist/WatchedItemQueryService.php"},
            {"function_name": "overrideDeferredUpdatesAddCallableUpdateCallback", "defined_in": "includes/Watchlist/WatchedItemStore.php"},
            {"function_name": "removeWatchIgnoringRights", "defined_in": "includes/Watchlist/WatchlistManager.php"},
            {"function_name": "resetNotificationTimestamp", "defined_in": "includes/Watchlist/WatchedItemStore.php"},
        ],
        "scope": "includes/Watchlist/",
        "exclusions": ["tests/", "vendor/", "__construct", "__destruct"],
        "function_keywords": [
            "buildTools", "countAllForUser", "duplicateEntry",
            "isTempWatchedIgnoringRights", "modifyWatchedItemsWithRCInfo",
            "overrideDeferredUpdatesAddCallableUpdateCallback",
            "removeWatchIgnoringRights", "resetNotificationTimestamp",
        ],
    }


# ---------------------------------------------------------------------------
# migration-planning — list all files referencing WatchedItemStore
# ---------------------------------------------------------------------------

MEDIAWIKI_MIGRATION_PATH_KEYS: frozenset[str] = frozenset({"affected_files"})


def mediawiki_migration_output_schema() -> dict[str, object]:
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["affected_files"],
        "properties": {
            "affected_files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["path", "change_needed"],
                    "properties": {
                        "path": {"type": "string"},
                        "change_needed": {"type": "string"},
                    },
                },
            },
        },
    }


def mediawiki_migration_scoring_rubric() -> dict[str, object]:
    return {
        "weights": {
            "files_found": 60,
            "false_positives": 20,
            "efficiency": 20,
        },
        "notes": [
            "files_found: recall — how many of the 53 reference files were listed.",
            "false_positives: precision — penalty for listing files that don't reference WatchedItemStore.",
            "efficiency: cost relative to $1.00 baseline.",
        ],
    }


def mediawiki_migration_reference() -> dict[str, object]:
    """All non-test, non-vendor files referencing WatchedItemStore.

    Ground truth generated via:
        grep -rl 'WatchedItemStore' --include='*.php' . |
        grep -v /tests/ | grep -v vendor/ |
        grep -v includes/Watchlist/WatchedItemStore.php | sort
    """
    return {
        "class_to_rename": "WatchedItemStore",
        "new_name": "WatchlistNotificationStore",
        "affected_files": [
            "autoload.php",
            "docs/config-vars.php",
            "includes/Actions/ActionFactory.php",
            "includes/Actions/DeleteAction.php",
            "includes/Actions/InfoAction.php",
            "includes/Actions/UnwatchAction.php",
            "includes/Actions/WatchAction.php",
            "includes/Api/ApiBlock.php",
            "includes/Api/ApiDelete.php",
            "includes/Api/ApiEditPage.php",
            "includes/Api/ApiMain.php",
            "includes/Api/ApiMove.php",
            "includes/Api/ApiProtect.php",
            "includes/Api/ApiQuery.php",
            "includes/Api/ApiQueryInfo.php",
            "includes/Api/ApiQueryUserInfo.php",
            "includes/Api/ApiQueryWatchlistRaw.php",
            "includes/Api/ApiRollback.php",
            "includes/Api/ApiSetNotificationTimestamp.php",
            "includes/Api/ApiUnblock.php",
            "includes/Api/ApiUndelete.php",
            "includes/Api/ApiUpload.php",
            "includes/Api/ApiUserrights.php",
            "includes/Api/ApiWatch.php",
            "includes/Api/ApiWatchlistTrait.php",
            "includes/EditPage/EditPage.php",
            "includes/MainConfigNames.php",
            "includes/MainConfigSchema.php",
            "includes/MediaWikiServices.php",
            "includes/Page/MergeHistory.php",
            "includes/Page/MovePage.php",
            "includes/Page/PageCommandFactory.php",
            "includes/RecentChanges/ChangesListQuery/ChangesListQuery.php",
            "includes/RecentChanges/ChangesListQuery/ChangesListQueryFactory.php",
            "includes/RecentChanges/ChangesListQuery/SeenCondition.php",
            "includes/RecentChanges/RecentChangeNotifier.php",
            "includes/ServiceWiring.php",
            "includes/Skin/SkinTemplate.php",
            "includes/SpecialPage/SpecialPageFactory.php",
            "includes/Specials/Pager/EditWatchlistPager.php",
            "includes/Specials/SpecialEditWatchlist.php",
            "includes/Specials/SpecialMovePage.php",
            "includes/Specials/SpecialRecentChanges.php",
            "includes/Specials/SpecialRecentChangesLinked.php",
            "includes/Specials/SpecialWatchlist.php",
            "includes/Watchlist/NoWriteWatchedItemStore.php",
            "includes/Watchlist/WatchedItemQueryService.php",
            "includes/Watchlist/WatchedItemStoreInterface.php",
            "includes/Watchlist/WatchlistExpiryJob.php",
            "includes/Watchlist/WatchlistManager.php",
            "includes/config-schema.php",
            "maintenance/makeTestEdits.php",
            "maintenance/purgeExpiredWatchlistItems.php",
        ],
        "file_keywords": [
            "WatchedItemStore", "ServiceWiring", "autoload",
            "ApiWatch", "WatchlistManager", "NoWriteWatchedItemStore",
        ],
    }
