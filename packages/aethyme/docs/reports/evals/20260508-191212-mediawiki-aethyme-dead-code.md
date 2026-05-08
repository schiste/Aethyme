# Eval Report: Find all public methods in includes/Watchlist/ that are never called from outside that directory.

## Meta

- Date: 2026-05-08
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
- Eval Type: dead-code
- Conditions: control-cto-off, control-cto-on, explore, leverage, task-conditioned
- Aethyme Commit: `dcad62ec0243e38edae788ee39698a154c595b6b`

## Model

- Name: haiku
- Provider: anthropic
- Backend: claude
- Reasoning: default
- Permission Mode: N/A

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 57.62 | 100.0 | 0 | $0.826 | 417.8s | 5,175,997 | 0.01 | 8.28 |
| Control (CTO on) | 59.03 | 97.58 | 0 | $1.062 | 362.9s | 7,707,514 | 0.01 | 9.76 |
| Explore | 60.72 | 93.79 | 0 | $0.866 | 730.8s | 7,331,157 | 0.01 | 4.99 |
| Leverage | 38.04 | 81.19 | 0 | $0.662 | 395.3s | 5,657,270 | 0.01 | 5.77 |
| Task-Conditioned | 46.78 | 72.94 | 0 | $1.118 | 973.7s | 9,665,596 | 0.00 | 2.88 |

## Score Breakdown

| Component | Weight | Control (CTO off) | Control (CTO on) | Explore | Leverage | Task-Conditioned |
|---|---| --- | --- | --- | --- | --- |
| Functions Found | 60% | 0.500 | 0.600 | 0.500 | 0.100 | 0.400 |
| False Positives | 20% | 0.833 | 0.667 | 1.000 | 1.000 | 0.667 |
| Efficiency | 20% | 0.548 | 0.485 | 0.536 | 0.602 | 0.472 |

## Prompts

### Control (CTO off)

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-off.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "unused_functions": [
    {
      "function_name": "name",
      "defined_in": "relative/path.py",
      "reason": "what you searched for and did not find"
    }
  ]
}

Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-off.json`.
```

### Control (CTO on)

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-on.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "unused_functions": [
    {
      "function_name": "name",
      "defined_in": "relative/path.py",
      "reason": "what you searched for and did not find"
    }
  ]
}

Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-on.json`.
```

### Explore

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-explore.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "unused_functions": [
    {
      "function_name": "name",
      "defined_in": "relative/path.py",
      "reason": "what you searched for and did not find"
    }
  ]
}

Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-explore.json`.
```

### Leverage

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-leverage.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "unused_functions": [
    {
      "function_name": "name",
      "defined_in": "relative/path.py",
      "reason": "what you searched for and did not find"
    }
  ]
}

Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

Aethyme is available in this repository. The shell wrapper at `.codex/skills/aethyme/aethyme-explore` routes to the native Rust binary.

Set up the command variables:
```bash
AETHYME_TOOL=".codex/skills/aethyme/aethyme-explore"
REPO="$PWD"
TASK="Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you."
```

Use the usage_boundary intent for this task:
```bash
"$AETHYME_TOOL" --repo "$REPO" --intent usage_boundary_query --request "$TASK" --scope "includes/Watchlist" --search-root . --format answer-json --show-observability
```

Read `trust_policy` and `safe_to_use_as_answer` first.

- If `safe_to_use_as_answer` is true, use `output_adapters.dead_code_eval_json.unused_functions` as your candidate list, then verify each before concluding.
- If `safe_to_use_as_answer` is false, treat the answer as ranked navigation; verify candidates with `grep`/`rg` before listing them.
- Read `excluded[]`, `degraded_reasons`, and `observability` before trusting.

Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-leverage.json`.
```

### Task-Conditioned

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-task-conditioned.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "unused_functions": [
    {
      "function_name": "name",
      "defined_in": "relative/path.py",
      "reason": "what you searched for and did not find"
    }
  ]
}

Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.

Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-task-conditioned.json`.
```

## Agent Output

### Control (CTO off)

```json
{
  "unused_functions": [
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback or ::overrideDeferredUpdatesAddCallableUpdateCallback in codebase excluding tests and vendor. Found only in test files (WatchedItemStoreUnitTest.php, WatchedItemStoreIntegrationTest.php) and definition. No production calls found outside includes/Watchlist/"
    },
    {
      "function_name": "getMaxId",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched for calls to ->getMaxId() or ::getMaxId(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) and test files. Not called from any code outside includes/Watchlist/"
    },
    {
      "function_name": "newForUser",
      "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
      "reason": "Searched for calls to ::newForUser(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) where it's used to instantiate ClearUserWatchlistJob. Not called from any code outside includes/Watchlist/"
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Searched for calls to ->countAllForUser(). Found only internal call within includes/Watchlist/WatchlistLabelStore.php (line 56) in the save() method. Not called from any code outside includes/Watchlist/"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfoQuery",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfoQuery. Found only the interface definition. No implementations or direct calls found anywhere in the codebase"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfo",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfo (excluding the query variant). Found only the interface definition. No implementations or direct calls found anywhere in the codebase"
    }
  ]
}
```

### Control (CTO on)

```json
{
  "unused_functions": [
    {
      "function_name": "calculateExpiryInDays",
      "defined_in": "includes/Watchlist/WatchedItem.php",
      "reason": "Static method never called from outside includes/Watchlist/ directory. Grep search for '>calculateExpiryInDays(' and '::calculateExpiryInDays(' returned no matches outside the Watchlist directory."
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>countAllForUser(' returned no matches outside the Watchlist directory."
    },
    {
      "function_name": "isTempWatchedIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>isTempWatchedIgnoringRights(' returned no matches outside the Watchlist directory."
    },
    {
      "function_name": "removeWatchIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>removeWatchIgnoringRights(' returned no matches outside the Watchlist directory."
    },
    {
      "function_name": "newForUser",
      "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
      "reason": "Public static method never called from outside includes/Watchlist/ directory. Grep search for '>newForUser(' and '::newForUser(' returned no matches outside the Watchlist directory."
    },
    {
      "function_name": "getWatchlistOwnerHtml",
      "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
      "reason": "Protected method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file."
    },
    {
      "function_name": "buildTools",
      "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
      "reason": "Public method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file."
    },
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>overrideDeferredUpdatesAddCallableUpdateCallback(' returned no matches outside the Watchlist directory. Marked with #[\\NoDiscard] attribute suggesting it's only for internal testing use."
    },
    {
      "function_name": "getMaxId",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>getMaxId(' returned no matches outside the Watchlist directory."
    }
  ]
}
```

### Explore

```json
{
  "unused_functions": [
    {
      "function_name": "buildTools",
      "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
      "reason": "Searched for buildTools across entire codebase excluding tests/vendor. Found only within WatchlistSpecialPage.php (internal usage). No external callers detected."
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Searched for countAllForUser across entire codebase excluding tests/vendor. Found only within WatchlistLabelStore.php (internal usage). No external callers detected."
    },
    {
      "function_name": "duplicateEntry",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched for duplicateEntry across entire codebase excluding tests/vendor. Found only within WatchedItemStore.php (internal usage via $this->duplicateEntry calls). No external callers detected."
    },
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched for overrideDeferredUpdatesAddCallableUpdateCallback across entire codebase excluding tests/vendor. Only found in test files (tests/phpunit/). No usage outside Watchlist directory (excluding tests)."
    },
    {
      "function_name": "resetNotificationTimestamp",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched for resetNotificationTimestamp across entire codebase excluding tests/vendor. Found only within Watchlist directory (WatchlistManager.php calls via $this->watchedItemStore->resetNotificationTimestamp). No external callers detected."
    }
  ]
}
```

### Leverage

```json
{
  "unused_functions": [
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched entire codebase (excluding tests/ and vendor/) for calls to this method. Found only its definition in WatchedItemStore.php at line 180. Method is marked as test-only (checks MW_PHPUNIT_TEST) and is only called from unit tests, not from production code."
    }
  ]
}
```

### Task-Conditioned

```json
{
  "unused_functions": [
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback( or ::overrideDeferredUpdatesAddCallableUpdateCallback( - found zero matches from outside includes/Watchlist/"
    },
    {
      "function_name": "getMaxId",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->getMaxId( or ::getMaxId( outside includes/Watchlist/ - found zero external matches (only internal call from WatchedItemStore.php:376)"
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->countAllForUser( or ::countAllForUser( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistLabelStore.php:56)"
    },
    {
      "function_name": "isTempWatchedIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->isTempWatchedIgnoringRights( or ::isTempWatchedIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:337)"
    },
    {
      "function_name": "removeWatchIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->removeWatchIgnoringRights( or ::removeWatchIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:489)"
    },
    {
      "function_name": "onWatchedItemQueryServiceExtensions",
      "defined_in": "includes/Watchlist/Hook/WatchedItemQueryServiceExtensionsHook.php",
      "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->onWatchedItemQueryServiceExtensions( or ::onWatchedItemQueryServiceExtensions( - found zero matches from outside includes/Watchlist/"
    }
  ]
}
```

## Context Pack Audit

- Navigation items: 0
- Risk items: 0

## Graph Quality Notes

N/A

## Prompt Effectiveness

### Control (CTO off)

- Prompt chars: 1376
- Input tokens: 430
- Output tokens: 31801

### Control (CTO on)

- Prompt chars: 1374
- Input tokens: 51534
- Output tokens: 51631

### Explore

- Prompt chars: 1360
- Input tokens: 628
- Output tokens: 43958

### Leverage

- Prompt chars: 3009
- Input tokens: 454
- Output tokens: 26101

### Task-Conditioned

- Prompt chars: 1378
- Input tokens: 780
- Output tokens: 60051

## Lessons & Action Items

- Keep context packs concise and explicitly prioritize high-confidence nodes.
- Capture parser visibility deltas across conditions to track graph quality drift.

## Verdict

**Explore** scored highest (60.72/100), **Leverage** lowest (38.04/100). Best overall value versus the control baseline: **Control (CTO off)** (100.00 recalculated eval score). Most efficient: Leverage ($0.662), most expensive: Task-Conditioned ($1.118). All conditions passed tests.

## Notes

Validation pass after the 2026-05-08 cleanup ladder. Sanity-check that the in-repo skill, native explore CLI, and dead-code adapter chain still work end-to-end on a 12.5K-file PHP repo.

---

## Raw Data

### Reference Output

```json
{
  "baseline_id": "mediawiki-dead-code-watchlist-v2",
  "reviewed_at": "2026-04-15",
  "selection_rule": "Public methods in includes/Watchlist/ with zero non-test, non-vendor call sites outside includes/Watchlist/.",
  "unused_functions": [
    {
      "function_name": "buildTools",
      "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
      "reason": "Public trait helper with zero direct non-test call sites outside includes/Watchlist/; only invoked through the protected outputSubtitle() helper inside the trait.",
      "review_category": "internal_trait_helper"
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Public store helper with zero external non-test callers; only used internally by WatchlistLabelStore::save() for per-user label limit checks.",
      "review_category": "internal_store_helper"
    },
    {
      "function_name": "duplicateEntry",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Interface implementation with zero direct external non-test call sites; only reached through the externally used duplicateAllAssociatedEntries() wrapper.",
      "review_category": "contract_method_internal_only"
    },
    {
      "function_name": "isTempWatchedIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public permission-bypassing helper with zero external non-test callers; only used by the externally called isTempWatched() wrapper.",
      "review_category": "permission_wrapper_internal_only"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfo",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Deprecated interface hook with zero implementations or non-test call sites outside the subsystem.",
      "review_category": "deprecated_interface_hook"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfoQuery",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Deprecated interface hook with zero implementations or non-test call sites outside the subsystem.",
      "review_category": "deprecated_interface_hook"
    },
    {
      "function_name": "newForUser",
      "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
      "reason": "Static factory has zero non-test call sites outside includes/Watchlist/; it is used only from WatchedItemStore::clearUserWatchedItemsUsingJobQueue().",
      "review_category": "internal_factory"
    },
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Test-only override hook guarded by MW_PHPUNIT_TEST with zero non-test callers outside the subsystem.",
      "review_category": "likely_dead_code"
    },
    {
      "function_name": "removeWatchIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public permission-bypassing helper with zero external non-test callers; only used by removeWatch() and setWatch() inside WatchlistManager.",
      "review_category": "permission_wrapper_internal_only"
    },
    {
      "function_name": "resetNotificationTimestamp",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Interface implementation with zero non-test callers outside includes/Watchlist/; used internally from WatchlistManager::clearTitleUserNotifications() and in tests.",
      "review_category": "contract_method_internal_only"
    }
  ],
  "literal_external_only": [
    {
      "function_name": "buildTools",
      "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
      "reason": "Public trait helper with zero direct non-test call sites outside includes/Watchlist/; only invoked through the protected outputSubtitle() helper inside the trait.",
      "review_category": "internal_trait_helper"
    },
    {
      "function_name": "countAllForUser",
      "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
      "reason": "Public store helper with zero external non-test callers; only used internally by WatchlistLabelStore::save() for per-user label limit checks.",
      "review_category": "internal_store_helper"
    },
    {
      "function_name": "duplicateEntry",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Interface implementation with zero direct external non-test call sites; only reached through the externally used duplicateAllAssociatedEntries() wrapper.",
      "review_category": "contract_method_internal_only"
    },
    {
      "function_name": "isTempWatchedIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public permission-bypassing helper with zero external non-test callers; only used by the externally called isTempWatched() wrapper.",
      "review_category": "permission_wrapper_internal_only"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfo",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Deprecated interface hook with zero implementations or non-test call sites outside the subsystem.",
      "review_category": "deprecated_interface_hook"
    },
    {
      "function_name": "modifyWatchedItemsWithRCInfoQuery",
      "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
      "reason": "Deprecated interface hook with zero implementations or non-test call sites outside the subsystem.",
      "review_category": "deprecated_interface_hook"
    },
    {
      "function_name": "newForUser",
      "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
      "reason": "Static factory has zero non-test call sites outside includes/Watchlist/; it is used only from WatchedItemStore::clearUserWatchedItemsUsingJobQueue().",
      "review_category": "internal_factory"
    },
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Test-only override hook guarded by MW_PHPUNIT_TEST with zero non-test callers outside the subsystem.",
      "review_category": "likely_dead_code"
    },
    {
      "function_name": "removeWatchIgnoringRights",
      "defined_in": "includes/Watchlist/WatchlistManager.php",
      "reason": "Public permission-bypassing helper with zero external non-test callers; only used by removeWatch() and setWatch() inside WatchlistManager.",
      "review_category": "permission_wrapper_internal_only"
    },
    {
      "function_name": "resetNotificationTimestamp",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Interface implementation with zero non-test callers outside includes/Watchlist/; used internally from WatchlistManager::clearTitleUserNotifications() and in tests.",
      "review_category": "contract_method_internal_only"
    }
  ],
  "likely_dead_code": [
    {
      "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
      "defined_in": "includes/Watchlist/WatchedItemStore.php",
      "reason": "Explicitly test-only hook with no production callers and no evidence of a required contract."
    }
  ],
  "scope": "includes/Watchlist/",
  "exclusions": [
    "tests/",
    "vendor/",
    "__construct",
    "__destruct"
  ],
  "function_keywords": [
    "buildTools",
    "countAllForUser",
    "duplicateEntry",
    "isTempWatchedIgnoringRights",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "newForUser",
    "overrideDeferredUpdatesAddCallableUpdateCallback",
    "removeWatchIgnoringRights",
    "resetNotificationTimestamp"
  ],
  "engineering_review": {
    "likely_dead_code": [
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Explicitly test-only hook with no production callers and no evidence of a required contract."
      }
    ],
    "internal_only_but_intentional": [
      {
        "function_name": "buildTools",
        "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
        "reason": "Used indirectly inside the trait by outputSubtitle(); public for trait consumers, not clearly dead."
      },
      {
        "function_name": "countAllForUser",
        "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
        "reason": "Legitimate internal helper used by save(); literal zero external callers does not make it dead."
      },
      {
        "function_name": "isTempWatchedIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Clear internal wrapper for the permission-checked public API isTempWatched()."
      },
      {
        "function_name": "newForUser",
        "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
        "reason": "Internal static factory still used inside the subsystem; not dead code in the maintainability sense."
      },
      {
        "function_name": "removeWatchIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Clear internal wrapper for the permission-checked public API removeWatch()."
      }
    ],
    "contract_or_interface_surface": [
      {
        "function_name": "duplicateEntry",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Required interface implementation; directly internal-only but part of a broader store contract."
      },
      {
        "function_name": "resetNotificationTimestamp",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Required interface implementation; direct external-call count is zero, but it remains contract surface."
      }
    ],
    "deprecated_or_inactive_hook_surface": [
      {
        "function_name": "modifyWatchedItemsWithRCInfo",
        "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
        "reason": "Deprecated extension hook interface with no active implementations found."
      },
      {
        "function_name": "modifyWatchedItemsWithRCInfoQuery",
        "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
        "reason": "Deprecated extension hook interface with no active implementations found."
      }
    ]
  }
}
```

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "unused_functions"
  ],
  "properties": {
    "unused_functions": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "function_name",
          "defined_in",
          "reason"
        ],
        "properties": {
          "function_name": {
            "type": "string"
          },
          "defined_in": {
            "type": "string"
          },
          "reason": {
            "type": "string"
          }
        }
      }
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "notes": [
    "functions_found: recall \u2014 how many of the reviewed literal external-only baseline functions were identified.",
    "false_positives: precision \u2014 penalty for listing functions that are outside the reviewed literal baseline.",
    "efficiency: cost relative to $1.00 baseline.",
    "Important: this benchmark uses the prompt semantics ('zero non-test callers outside includes/Watchlist/'), not a pure software-maintenance notion of dead code."
  ]
}
```

### Per-Condition Run Records

#### Control (CTO off)

```json
{
  "command": "claude --dangerously-skip-permissions --model haiku",
  "exit_code": 0,
  "duration_seconds": 417.779,
  "num_turns": 53,
  "cost_usd": 0.8256633200000001,
  "input_tokens": 430,
  "output_tokens": 31801,
  "cache_read_tokens": 4832229,
  "cache_create_tokens": 311537,
  "stdout": "{\"unused_functions\": [{\"function_name\": \"overrideDeferredUpdatesAddCallableUpdateCallback\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback or ::overrideDeferredUpdatesAddCallableUpdateCallback in codebase excluding tests and vendor. Found only in test files (WatchedItemStoreUnitTest.php, WatchedItemStoreIntegrationTest.php) and definition. No production calls found outside includes/Watchlist/\"}, {\"function_name\": \"getMaxId\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched for calls to ->getMaxId() or ::getMaxId(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) and test files. Not called from any code outside includes/Watchlist/\"}, {\"function_name\": \"newForUser\", \"defined_in\": \"includes/Watchlist/ClearUserWatchlistJob.php\", \"reason\": \"Searched for calls to ::newForUser(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) where it's used to instantiate ClearUserWatchlistJob. Not called from any code outside includes/Watchlist/\"}, {\"function_name\": \"countAllForUser\", \"defined_in\": \"includes/Watchlist/WatchlistLabelStore.php\", \"reason\": \"Searched for calls to ->countAllForUser(). Found only internal call within includes/Watchlist/WatchlistLabelStore.php (line 56) in the save() method. Not called from any code outside includes/Watchlist/\"}, {\"function_name\": \"modifyWatchedItemsWithRCInfoQuery\", \"defined_in\": \"includes/Watchlist/WatchedItemQueryServiceExtension.php\", \"reason\": \"Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfoQuery. Found only the interface definition. No implementations or direct calls found anywhere in the codebase\"}, {\"function_name\": \"modifyWatchedItemsWithRCInfo\", \"defined_in\": \"includes/Watchlist/WatchedItemQueryServiceExtension.php\", \"reason\": \"Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfo (excluding the query variant). Found only the interface definition. No implementations or direct calls found anywhere in the codebase\"}]}",
  "stderr": "",
  "retries": 0,
  "review_burden": 0,
  "final_output_message": "",
  "structured_output": {
    "unused_functions": [
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback or ::overrideDeferredUpdatesAddCallableUpdateCallback in codebase excluding tests and vendor. Found only in test files (WatchedItemStoreUnitTest.php, WatchedItemStoreIntegrationTest.php) and definition. No production calls found outside includes/Watchlist/"
      },
      {
        "function_name": "getMaxId",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched for calls to ->getMaxId() or ::getMaxId(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) and test files. Not called from any code outside includes/Watchlist/"
      },
      {
        "function_name": "newForUser",
        "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
        "reason": "Searched for calls to ::newForUser(). Found only internal call within includes/Watchlist/WatchedItemStore.php (line 376) where it's used to instantiate ClearUserWatchlistJob. Not called from any code outside includes/Watchlist/"
      },
      {
        "function_name": "countAllForUser",
        "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
        "reason": "Searched for calls to ->countAllForUser(). Found only internal call within includes/Watchlist/WatchlistLabelStore.php (line 56) in the save() method. Not called from any code outside includes/Watchlist/"
      },
      {
        "function_name": "modifyWatchedItemsWithRCInfoQuery",
        "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
        "reason": "Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfoQuery. Found only the interface definition. No implementations or direct calls found anywhere in the codebase"
      },
      {
        "function_name": "modifyWatchedItemsWithRCInfo",
        "defined_in": "includes/Watchlist/WatchedItemQueryServiceExtension.php",
        "reason": "Interface method in WatchedItemQueryServiceExtension. Searched entire codebase for calls to modifyWatchedItemsWithRCInfo (excluding the query variant). Found only the interface definition. No implementations or direct calls found anywhere in the codebase"
      }
    ]
  },
  "tool_calls": null
}
```

#### Control (CTO on)

```json
{
  "command": "claude --dangerously-skip-permissions --model haiku",
  "exit_code": 0,
  "duration_seconds": 362.862,
  "num_turns": 63,
  "cost_usd": 1.06186724,
  "input_tokens": 51534,
  "output_tokens": 51631,
  "cache_read_tokens": 7380688,
  "cache_create_tokens": 223661,
  "stdout": "{\"unused_functions\": [{\"function_name\": \"calculateExpiryInDays\", \"defined_in\": \"includes/Watchlist/WatchedItem.php\", \"reason\": \"Static method never called from outside includes/Watchlist/ directory. Grep search for '>calculateExpiryInDays(' and '::calculateExpiryInDays(' returned no matches outside the Watchlist directory.\"}, {\"function_name\": \"countAllForUser\", \"defined_in\": \"includes/Watchlist/WatchlistLabelStore.php\", \"reason\": \"Public method never called from outside includes/Watchlist/ directory. Grep search for '>countAllForUser(' returned no matches outside the Watchlist directory.\"}, {\"function_name\": \"isTempWatchedIgnoringRights\", \"defined_in\": \"includes/Watchlist/WatchlistManager.php\", \"reason\": \"Public method never called from outside includes/Watchlist/ directory. Grep search for '>isTempWatchedIgnoringRights(' returned no matches outside the Watchlist directory.\"}, {\"function_name\": \"removeWatchIgnoringRights\", \"defined_in\": \"includes/Watchlist/WatchlistManager.php\", \"reason\": \"Public method never called from outside includes/Watchlist/ directory. Grep search for '>removeWatchIgnoringRights(' returned no matches outside the Watchlist directory.\"}, {\"function_name\": \"newForUser\", \"defined_in\": \"includes/Watchlist/ClearUserWatchlistJob.php\", \"reason\": \"Public static method never called from outside includes/Watchlist/ directory. Grep search for '>newForUser(' and '::newForUser(' returned no matches outside the Watchlist directory.\"}, {\"function_name\": \"getWatchlistOwnerHtml\", \"defined_in\": \"includes/Watchlist/WatchlistSpecialPage.php\", \"reason\": \"Protected method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file.\"}, {\"function_name\": \"buildTools\", \"defined_in\": \"includes/Watchlist/WatchlistSpecialPage.php\", \"reason\": \"Public method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file.\"}, {\"function_name\": \"overrideDeferredUpdatesAddCallableUpdateCallback\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Public method never called from outside includes/Watchlist/ directory. Grep search for '>overrideDeferredUpdatesAddCallableUpdateCallback(' returned no matches outside the Watchlist directory. Marked with #[\\\\NoDiscard] attribute suggesting it's only for internal testing use.\"}, {\"function_name\": \"getMaxId\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Public method never called from outside includes/Watchlist/ directory. Grep search for '>getMaxId(' returned no matches outside the Watchlist directory.\"}]}",
  "stderr": "",
  "retries": 0,
  "review_burden": 0,
  "final_output_message": "",
  "structured_output": {
    "unused_functions": [
      {
        "function_name": "calculateExpiryInDays",
        "defined_in": "includes/Watchlist/WatchedItem.php",
        "reason": "Static method never called from outside includes/Watchlist/ directory. Grep search for '>calculateExpiryInDays(' and '::calculateExpiryInDays(' returned no matches outside the Watchlist directory."
      },
      {
        "function_name": "countAllForUser",
        "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
        "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>countAllForUser(' returned no matches outside the Watchlist directory."
      },
      {
        "function_name": "isTempWatchedIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>isTempWatchedIgnoringRights(' returned no matches outside the Watchlist directory."
      },
      {
        "function_name": "removeWatchIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>removeWatchIgnoringRights(' returned no matches outside the Watchlist directory."
      },
      {
        "function_name": "newForUser",
        "defined_in": "includes/Watchlist/ClearUserWatchlistJob.php",
        "reason": "Public static method never called from outside includes/Watchlist/ directory. Grep search for '>newForUser(' and '::newForUser(' returned no matches outside the Watchlist directory."
      },
      {
        "function_name": "getWatchlistOwnerHtml",
        "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
        "reason": "Protected method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file."
      },
      {
        "function_name": "buildTools",
        "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
        "reason": "Public method defined in WatchlistSpecialPage trait never called from outside includes/Watchlist/ directory. Only called internally by outputSubtitle() method within the same trait file."
      },
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>overrideDeferredUpdatesAddCallableUpdateCallback(' returned no matches outside the Watchlist directory. Marked with #[\\NoDiscard] attribute suggesting it's only for internal testing use."
      },
      {
        "function_name": "getMaxId",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Public method never called from outside includes/Watchlist/ directory. Grep search for '>getMaxId(' returned no matches outside the Watchlist directory."
      }
    ]
  },
  "tool_calls": null
}
```

#### Explore

```json
{
  "command": "claude --dangerously-skip-permissions --model haiku",
  "exit_code": 0,
  "duration_seconds": 730.788,
  "num_turns": 77,
  "cost_usd": 0.8656884400000001,
  "input_tokens": 628,
  "output_tokens": 43958,
  "cache_read_tokens": 7170888,
  "cache_create_tokens": 115683,
  "stdout": "{\"unused_functions\": [{\"function_name\": \"buildTools\", \"defined_in\": \"includes/Watchlist/WatchlistSpecialPage.php\", \"reason\": \"Searched for buildTools across entire codebase excluding tests/vendor. Found only within WatchlistSpecialPage.php (internal usage). No external callers detected.\"}, {\"function_name\": \"countAllForUser\", \"defined_in\": \"includes/Watchlist/WatchlistLabelStore.php\", \"reason\": \"Searched for countAllForUser across entire codebase excluding tests/vendor. Found only within WatchlistLabelStore.php (internal usage). No external callers detected.\"}, {\"function_name\": \"duplicateEntry\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched for duplicateEntry across entire codebase excluding tests/vendor. Found only within WatchedItemStore.php (internal usage via $this->duplicateEntry calls). No external callers detected.\"}, {\"function_name\": \"overrideDeferredUpdatesAddCallableUpdateCallback\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched for overrideDeferredUpdatesAddCallableUpdateCallback across entire codebase excluding tests/vendor. Only found in test files (tests/phpunit/). No usage outside Watchlist directory (excluding tests).\"}, {\"function_name\": \"resetNotificationTimestamp\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched for resetNotificationTimestamp across entire codebase excluding tests/vendor. Found only within Watchlist directory (WatchlistManager.php calls via $this->watchedItemStore->resetNotificationTimestamp). No external callers detected.\"}]}",
  "stderr": "",
  "retries": 0,
  "review_burden": 0,
  "final_output_message": "",
  "structured_output": {
    "unused_functions": [
      {
        "function_name": "buildTools",
        "defined_in": "includes/Watchlist/WatchlistSpecialPage.php",
        "reason": "Searched for buildTools across entire codebase excluding tests/vendor. Found only within WatchlistSpecialPage.php (internal usage). No external callers detected."
      },
      {
        "function_name": "countAllForUser",
        "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
        "reason": "Searched for countAllForUser across entire codebase excluding tests/vendor. Found only within WatchlistLabelStore.php (internal usage). No external callers detected."
      },
      {
        "function_name": "duplicateEntry",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched for duplicateEntry across entire codebase excluding tests/vendor. Found only within WatchedItemStore.php (internal usage via $this->duplicateEntry calls). No external callers detected."
      },
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched for overrideDeferredUpdatesAddCallableUpdateCallback across entire codebase excluding tests/vendor. Only found in test files (tests/phpunit/). No usage outside Watchlist directory (excluding tests)."
      },
      {
        "function_name": "resetNotificationTimestamp",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched for resetNotificationTimestamp across entire codebase excluding tests/vendor. Found only within Watchlist directory (WatchlistManager.php calls via $this->watchedItemStore->resetNotificationTimestamp). No external callers detected."
      }
    ]
  },
  "tool_calls": null
}
```

#### Leverage

```json
{
  "command": "claude --dangerously-skip-permissions --model haiku",
  "exit_code": 0,
  "duration_seconds": 395.296,
  "num_turns": 56,
  "cost_usd": 0.66157364,
  "input_tokens": 454,
  "output_tokens": 26101,
  "cache_read_tokens": 5515118,
  "cache_create_tokens": 115597,
  "stdout": "{\"unused_functions\": [{\"function_name\": \"overrideDeferredUpdatesAddCallableUpdateCallback\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched entire codebase (excluding tests/ and vendor/) for calls to this method. Found only its definition in WatchedItemStore.php at line 180. Method is marked as test-only (checks MW_PHPUNIT_TEST) and is only called from unit tests, not from production code.\"}]}",
  "stderr": "",
  "retries": 0,
  "review_burden": 0,
  "final_output_message": "",
  "structured_output": {
    "unused_functions": [
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched entire codebase (excluding tests/ and vendor/) for calls to this method. Found only its definition in WatchedItemStore.php at line 180. Method is marked as test-only (checks MW_PHPUNIT_TEST) and is only called from unit tests, not from production code."
      }
    ]
  },
  "tool_calls": null
}
```

#### Task-Conditioned

```json
{
  "command": "claude --dangerously-skip-permissions --model haiku",
  "exit_code": 0,
  "duration_seconds": 973.721,
  "num_turns": 96,
  "cost_usd": 1.1182301200000002,
  "input_tokens": 780,
  "output_tokens": 60051,
  "cache_read_tokens": 9486264,
  "cache_create_tokens": 118501,
  "stdout": "{\"unused_functions\": [{\"function_name\": \"overrideDeferredUpdatesAddCallableUpdateCallback\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback( or ::overrideDeferredUpdatesAddCallableUpdateCallback( - found zero matches from outside includes/Watchlist/\"}, {\"function_name\": \"getMaxId\", \"defined_in\": \"includes/Watchlist/WatchedItemStore.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->getMaxId( or ::getMaxId( outside includes/Watchlist/ - found zero external matches (only internal call from WatchedItemStore.php:376)\"}, {\"function_name\": \"countAllForUser\", \"defined_in\": \"includes/Watchlist/WatchlistLabelStore.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->countAllForUser( or ::countAllForUser( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistLabelStore.php:56)\"}, {\"function_name\": \"isTempWatchedIgnoringRights\", \"defined_in\": \"includes/Watchlist/WatchlistManager.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->isTempWatchedIgnoringRights( or ::isTempWatchedIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:337)\"}, {\"function_name\": \"removeWatchIgnoringRights\", \"defined_in\": \"includes/Watchlist/WatchlistManager.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->removeWatchIgnoringRights( or ::removeWatchIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:489)\"}, {\"function_name\": \"onWatchedItemQueryServiceExtensions\", \"defined_in\": \"includes/Watchlist/Hook/WatchedItemQueryServiceExtensionsHook.php\", \"reason\": \"Searched entire codebase (excluding tests/vendor) for calls to ->onWatchedItemQueryServiceExtensions( or ::onWatchedItemQueryServiceExtensions( - found zero matches from outside includes/Watchlist/\"}]}",
  "stderr": "",
  "retries": 0,
  "review_burden": 0,
  "final_output_message": "",
  "structured_output": {
    "unused_functions": [
      {
        "function_name": "overrideDeferredUpdatesAddCallableUpdateCallback",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->overrideDeferredUpdatesAddCallableUpdateCallback( or ::overrideDeferredUpdatesAddCallableUpdateCallback( - found zero matches from outside includes/Watchlist/"
      },
      {
        "function_name": "getMaxId",
        "defined_in": "includes/Watchlist/WatchedItemStore.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->getMaxId( or ::getMaxId( outside includes/Watchlist/ - found zero external matches (only internal call from WatchedItemStore.php:376)"
      },
      {
        "function_name": "countAllForUser",
        "defined_in": "includes/Watchlist/WatchlistLabelStore.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->countAllForUser( or ::countAllForUser( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistLabelStore.php:56)"
      },
      {
        "function_name": "isTempWatchedIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->isTempWatchedIgnoringRights( or ::isTempWatchedIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:337)"
      },
      {
        "function_name": "removeWatchIgnoringRights",
        "defined_in": "includes/Watchlist/WatchlistManager.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->removeWatchIgnoringRights( or ::removeWatchIgnoringRights( outside includes/Watchlist/ - found zero external matches (only internal call from WatchlistManager.php:489)"
      },
      {
        "function_name": "onWatchedItemQueryServiceExtensions",
        "defined_in": "includes/Watchlist/Hook/WatchedItemQueryServiceExtensionsHook.php",
        "reason": "Searched entire codebase (excluding tests/vendor) for calls to ->onWatchedItemQueryServiceExtensions( or ::onWatchedItemQueryServiceExtensions( - found zero matches from outside includes/Watchlist/"
      }
    ]
  },
  "tool_calls": null
}
```

### Per-Condition Assessments

#### Control (CTO off)

```json
{
  "scores": {
    "functions_found": 0.5,
    "false_positives": 0.8333333333333334,
    "efficiency": 0.5477461200239264
  },
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "weighted_score": 57.62,
  "max_score": 100,
  "functions_matched": [
    "countAllForUser",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "newForUser",
    "overrideDeferredUpdatesAddCallableUpdateCallback"
  ],
  "functions_missed": [
    "buildTools",
    "duplicateEntry",
    "isTempWatchedIgnoringRights",
    "removeWatchIgnoringRights",
    "resetNotificationTimestamp"
  ],
  "false_positives": [
    "getMaxId"
  ]
}
```

#### Control (CTO on)

```json
{
  "scores": {
    "functions_found": 0.6,
    "false_positives": 0.6666666666666666,
    "efficiency": 0.48499727848627155
  },
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "weighted_score": 59.03,
  "max_score": 100,
  "functions_matched": [
    "buildTools",
    "countAllForUser",
    "isTempWatchedIgnoringRights",
    "newForUser",
    "overrideDeferredUpdatesAddCallableUpdateCallback",
    "removeWatchIgnoringRights"
  ],
  "functions_missed": [
    "duplicateEntry",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "resetNotificationTimestamp"
  ],
  "false_positives": [
    "calculateExpiryInDays",
    "getMaxId",
    "getWatchlistOwnerHtml"
  ]
}
```

#### Explore

```json
{
  "scores": {
    "functions_found": 0.5,
    "false_positives": 1.0,
    "efficiency": 0.5359951739852127
  },
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "weighted_score": 60.72,
  "max_score": 100,
  "functions_matched": [
    "buildTools",
    "countAllForUser",
    "duplicateEntry",
    "overrideDeferredUpdatesAddCallableUpdateCallback",
    "resetNotificationTimestamp"
  ],
  "functions_missed": [
    "isTempWatchedIgnoringRights",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "newForUser",
    "removeWatchIgnoringRights"
  ],
  "false_positives": []
}
```

#### Leverage

```json
{
  "scores": {
    "functions_found": 0.1,
    "false_positives": 1.0,
    "efficiency": 0.6018391095804818
  },
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "weighted_score": 38.04,
  "max_score": 100,
  "functions_matched": [
    "overrideDeferredUpdatesAddCallableUpdateCallback"
  ],
  "functions_missed": [
    "buildTools",
    "countAllForUser",
    "duplicateEntry",
    "isTempWatchedIgnoringRights",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "newForUser",
    "removeWatchIgnoringRights",
    "resetNotificationTimestamp"
  ],
  "false_positives": []
}
```

#### Task-Conditioned

```json
{
  "scores": {
    "functions_found": 0.4,
    "false_positives": 0.6666666666666666,
    "efficiency": 0.47209223896787944
  },
  "weights": {
    "functions_found": 60,
    "false_positives": 20,
    "efficiency": 20
  },
  "weighted_score": 46.78,
  "max_score": 100,
  "functions_matched": [
    "countAllForUser",
    "isTempWatchedIgnoringRights",
    "overrideDeferredUpdatesAddCallableUpdateCallback",
    "removeWatchIgnoringRights"
  ],
  "functions_missed": [
    "buildTools",
    "duplicateEntry",
    "modifyWatchedItemsWithRCInfo",
    "modifyWatchedItemsWithRCInfoQuery",
    "newForUser",
    "resetNotificationTimestamp"
  ],
  "false_positives": [
    "getMaxId",
    "onWatchedItemQueryServiceExtensions"
  ]
}
```

