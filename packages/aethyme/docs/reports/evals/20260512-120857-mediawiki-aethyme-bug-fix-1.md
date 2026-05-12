# Eval Report: Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug.

## Meta

- Date: 2026-05-12
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
- Eval Type: bug-fix-1
- Conditions: control-cto-off, control-cto-on, explore, leverage, task-conditioned
- Aethyme Commit: `bdaf014a9c8512b1d0d9b2b5dfb41c7b4e342839`

## Objective

Compare cost across conditions to localize T419918 (watchlist seen-marking) without applying a fix. Quality is a gate (the implementation file must be named); efficiency is the comparison axis.

## Constraints

- Output must be valid JSON matching the documented schema.
- Output must be saved to the agent-specified path; missing or empty output scores 0.
- Repository files must not be modified.

## Model

- Name: haiku
- Provider: anthropic
- Backend: claude
- Reasoning: default
- Permission Mode: N/A

## Discoverability Gap

Difference in cost between `explore` (skill present, no instruction) and `leverage` (skill present, agent told it exists). Positive = pointing helped; negative = pointing hurt.

- **Cost:** `+22.64%` (explore $1.3765 → leverage $1.0649)
- **Tokens:** `+20.25%` (explore 8,845,661 → leverage 7,054,634)

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 37.91 | 100.0 | 43 | $1.041 | 217.2s | 8,491,656 | 0.00 | 10.47 |
| Control (CTO on) | 31.46 | 68.31 | 43 | $1.313 | 2113.7s | 9,690,826 | 0.00 | 0.89 |
| Explore | 28.63 | 76.58 | 39 | $1.377 | 745.4s | 8,845,661 | 0.00 | 2.30 |
| Leverage | 28.04 | 87.2 | 28 | $1.065 | 346.4s | 7,054,634 | 0.00 | 4.86 |
| Task-Conditioned | 25.25 | 83.77 | 29 | $1.078 | 314.8s | 8,231,316 | 0.00 | 4.81 |

## Score Breakdown

| Component | Weight | Control (CTO off) | Control (CTO on) | Explore | Leverage | Task-Conditioned |
|---|---| --- | --- | --- | --- | --- |
| Files Identified | 35% | 0.250 | 0.250 | 0.250 | 0.250 | 0.250 |
| Root Cause Quality | 25% | 0.556 | 0.556 | 0.444 | 0.333 | 0.222 |
| Fix Plan Quality | 15% | 0.286 | 0.000 | 0.000 | 0.000 | 0.000 |
| Testing Quality | 15% | 0.625 | 0.500 | 0.500 | 0.625 | 0.625 |
| Efficiency | 10% | 0.161 | 0.132 | 0.127 | 0.158 | 0.156 |

## Prompts

### Control (CTO off)

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-off.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "files_to_edit": [
    {"path": "relative/path.php", "what_to_change": "..."}
  ],
  "root_cause": "...",
  "fix_plan": "...",
  "testing": "..."
}

Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix and do NOT write any files in the repository — produce your analysis as JSON in your final response only.



Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-off.json`.

```

### Control (CTO on)

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-on.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "files_to_edit": [
    {"path": "relative/path.php", "what_to_change": "..."}
  ],
  "root_cause": "...",
  "fix_plan": "...",
  "testing": "..."
}

Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix and do NOT write any files in the repository — produce your analysis as JSON in your final response only.



Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Control/.aethyme-eval-output-control-cto-on.json`.

```

### Explore

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-explore.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "files_to_edit": [
    {"path": "relative/path.php", "what_to_change": "..."}
  ],
  "root_cause": "...",
  "fix_plan": "...",
  "testing": "..."
}

Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix and do NOT write any files in the repository — produce your analysis as JSON in your final response only.



Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-explore.json`.

```

### Leverage

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-leverage.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "files_to_edit": [
    {"path": "relative/path.php", "what_to_change": "..."}
  ],
  "root_cause": "...",
  "fix_plan": "...",
  "testing": "..."
}

Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix and do NOT write any files in the repository — produce your analysis as JSON in your final response only.

Aethyme is available in this repository. See `.codex/skills/aethyme/SKILL.md` for usage; the wrapper at `.codex/skills/aethyme/aethyme-explore` is the convenience entry point. Use it where it helps; verify its output before acting on it.



Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-leverage.json`.

```

### Task-Conditioned

```text
IMPORTANT: You MUST save exactly one JSON object to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-task-conditioned.json` when done. Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:
{
  "files_to_edit": [
    {"path": "relative/path.php", "what_to_change": "..."}
  ],
  "root_cause": "...",
  "fix_plan": "...",
  "testing": "..."
}

Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix and do NOT write any files in the repository — produce your analysis as JSON in your final response only.

Use Aethyme tools and any task-conditioned context artifacts to navigate the repository graph, but do your own analysis.



Remember: save JSON only to `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme/.aethyme-eval-output-task-conditioned.json`.

```

## Agent Output

### Control (CTO off)

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method around line 1145-1150, after calling $de->showDiffPage(), retrieve the actual new RevisionRecord from DifferenceEngine and pass it to doViewUpdates() instead of passing the potentially zero-valued $new integer. Change from passing (int)$new as the $oldid parameter to passing the RevisionRecord object as the third $oldRev parameter, or extract the actual revision ID from the DifferenceEngine's loaded revision data."
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "In the resetNotificationTimestamp() method, add better handling to distinguish between 'no specific revision' (which should mark the entire page as seen) and 'view a specific older revision' (which should only mark that revision as seen). Alternatively, ensure the logic properly handles the case where a specific revision is being viewed versus viewing the current page state."
    }
  ],
  "root_cause": "When viewing a diff, Article.php's showDiffPage() method extracts the new revision ID from mapDiffPrevNext() and passes it to doViewUpdates() as the $oldid parameter. However, when $new is 0 (representing the latest/current revision) or false, converting it to (int)$new results in 0 being passed. The WatchedItemStore.resetNotificationTimestamp() method interprets $oldid=0 as 'no specific revision provided' and fetches the latest revision to use for marking the entire page as seen. This causes all revisions to be marked as seen instead of just the specific revision being viewed in the diff.",
  "fix_plan": "The fix should retrieve the actual RevisionRecord for the new side of the diff from the DifferenceEngine (which is loaded after calling showDiffPage() via loadRevisionData()) and pass it as the third parameter to doViewUpdates(). Specifically: (1) After calling $de->showDiffPage(), retrieve the new revision with $newRev = $de->getNewRevision(); (2) Call doViewUpdates() with the revision record: $this->mPage->doViewUpdates($context->getAuthority(), 0, $newRev); OR pass the actual revision ID if the second parameter is preferred. This ensures the watchlist clearing logic marks only the specific revision being viewed as seen, not all revisions up to the latest.",
  "testing": "Test by: (1) Creating a test page with multiple revisions; (2) Watching the page; (3) Viewing a diff between two older revisions (not involving the latest revision); (4) Verify in the watchlist that only the new side of the viewed diff is marked as seen, and older revisions remain marked as unseen; (5) Test edge cases: viewing diff with latest revision, viewing diff between very old revisions, using diff=prev and diff=next parameters; (6) Verify the watchlist notification timestamp correctly reflects viewing that specific revision rather than the entire page's latest revision."
}
```

### Control (CTO on)

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In showDiffPage() method around line 1149, modify the watchlist update logic to only call doViewUpdates() when viewing the latest revision. Currently it unconditionally calls `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )` which marks all revisions as seen regardless of which revision is being viewed in the diff. The fix is to add a conditional: only call doViewUpdates() if `!$new` (when the new revision is false, indicating the latest). Change from: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new );` to: `if ( !$new ) { $this->mPage->doViewUpdates( $context->getAuthority() ); }`"
    }
  ],
  "root_cause": "In Article.php's showDiffPage() method (line 1149), the code unconditionally calls `doViewUpdates()` with the new revision ID from the diff comparison, regardless of whether that revision is the latest or an old one. The mapDiffPrevNext() method returns false for $new when viewing the latest revision. Currently, the code calls `doViewUpdates( $context->getAuthority(), (int)$new )` in all cases. When $new is false (viewing latest), (int)false converts to 0, which causes resetNotificationTimestamp() to call getNotificationTimestamp() with oldid=0. This returns null, clearing the entire wl_notificationtimestamp field for that page, marking ALL revisions as 'seen'. When $new is a specific revision ID (viewing an old revision), it gets passed as oldid to resetNotificationTimestamp(), which then marks all revisions up to and including that one as seen. The bug is that watchlist updates should only occur when viewing the actual page content or when viewing a diff to the latest revision, not when viewing diffs to arbitrary old revisions.",
  "fix_plan": "The fix is to add a conditional check before calling doViewUpdates(). Since mapDiffPrevNext() returns false when viewing the latest revision, we should only call doViewUpdates() when !$new. This ensures: (1) When viewing a diff to the latest revision ($new=false), doViewUpdates is called with the default oldid=0, properly marking the page as fully viewed; (2) When viewing a diff to an old revision ($new=some_id), doViewUpdates is NOT called, leaving the watchlist notification timestamp unchanged. This matches the expected behavior where watchlist notifications are only cleared when actually viewing page content or a diff to the latest version, not when just browsing through historical diffs.",
  "testing": "Verify the fix with these test cases: (1) Create a watchlisted page with 3+ revisions; (2) View watchlist - page should show unread indicator; (3) View a diff between two old revisions (e.g., oldid=100&diff=102) - return to watchlist, page should STILL show unread; (4) View a diff to the latest revision (diff=next on second-to-last, or oldid=old&diff=latest) - return to watchlist, page should now show as fully read; (5) Test diff=prev on various revisions; (6) Test diff=next on second-to-last revision (should mark latest as seen); (7) Test edge cases with deleted/archived revisions. Each test should verify that only the appropriate revisions are marked as seen in the watchlist."
}
```

### Explore

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (around line 1150), when calling doViewUpdates(), pass the old revision record in addition to the new revision ID to ensure the watchlist notification system knows which specific revision was viewed in the diff"
    },
    {
      "path": "includes/Watchlist/WatchlistManager.php",
      "what_to_change": "Ensure clearTitleUserNotifications() correctly handles the case where a diff is being viewed - it should only mark the specific revisions being compared as seen, not all revisions on the page"
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "Review resetNotificationTimestamp() to ensure it correctly handles diff viewing scenarios where both old and new revisions might need to be marked as seen separately, rather than always updating to the latest revision timestamp"
    }
  ],
  "root_cause": "When viewing a diff/revision on a watchlisted page, the Article.php showDiffPage() method calls WikiPage::doViewUpdates() with only the new revision ID (line 1150: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )`), without properly passing the old revision record. This causes WatchedItemStore::resetNotificationTimestamp() to update the notification timestamp to the new revision's timestamp. Since the watchlist notification system marks revisions as unseen only if their timestamp is >= the notification timestamp, updating to the new revision's timestamp causes all older revisions to appear as 'seen'. If the new revision happens to be the latest, all revisions on the page become marked as seen instead of just the ones in the diff view.",
  "fix_plan": "1. Modify Article.php showDiffPage() to pass the old revision record (via the $oldRev parameter) to doViewUpdates() when viewing a diff, not just the integer ID. 2. Ensure that both the old and new revisions from the diff are available to the watchlist notification system. 3. Update WatchedItemStore::resetNotificationTimestamp() or the diff viewing code to only mark the specific revisions being viewed in the diff as seen, rather than all revisions up to the latest. 4. Consider whether viewing a diff between revisions A and B should mark only B as seen (typical behavior) or both A and B as seen (since both are being viewed in the diff comparison).",
  "testing": "1. Create a watchlisted page with multiple revisions. 2. View the watchlist and verify which revisions are marked as unseen (have the 'updatedmarker'). 3. Click on a diff link to view a diff between two specific revisions (not the latest). 4. Return to the watchlist or history page. 5. Verify that ONLY the revisions that were in the diff view are marked as seen, and any newer revisions still appear as unseen. 6. Repeat with a diff involving the latest revision to ensure the behavior is consistent. 7. Test edge cases: first revision, diff with 'prev'/'next' parameters, and diffs on the latest revision."
}
```

### Leverage

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "Lines 1145-1149: When calling doViewUpdates() during a diff view, pass the older revision ID (not the newer one) to avoid triggering the 'latest revision' case that clears all notifications"
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "Lines 1476-1521 (getNotificationTimestamp function): Fix logic to properly handle the case where oldid is the latest revision in a diff view - should use the timestamp of that revision instead of returning null (which clears all notifications)"
    }
  ],
  "root_cause": "When viewing a diff where the newer/right-side revision is the latest revision of the page, Article.php passes that revision's ID (line 1149) to doViewUpdates(). WatchedItemStore::getNotificationTimestamp() interprets this as viewing the latest revision standalone and returns null to clear the notification timestamp. Setting wl_notificationtimestamp to NULL in the watchlist marks ALL revisions as 'seen' instead of just the ones in the viewed diff. The commented code at line 1503 suggests this issue was previously recognized but not fully fixed.",
  "fix_plan": "The fix requires changes in two files:\n\n1. **includes/Page/Article.php (Line 1147-1149)**: When viewing a diff where the newer revision is the latest, the code should NOT pass the newer revision's ID to doViewUpdates(). Instead, it should pass the older revision's ID. This ensures the watchlist is updated to mark revisions up to the older one as seen, while leaving a proper notification timestamp for newer revisions.\n\nAlternatively/Additionally: 2. **includes/Watchlist/WatchedItemStore.php (Lines 1476-1521 in getNotificationTimestamp)**: The function should distinguish between viewing a single revision (where null is correct for the latest) versus viewing a diff where the revision being marked is the latest. When oldid is the latest revision, instead of unconditionally returning null, it should return the timestamp of that revision. This preserves proper notification tracking for future revisions while marking current ones as seen.",
  "testing": "Test cases should verify:\n1. Viewing a single revision page marks that revision and earlier ones as seen\n2. Viewing a diff between two revisions (where neither is the latest) marks both revisions as seen\n3. Viewing a diff where the newer revision IS the latest revision should mark up to that revision as seen, but NOT mark future revisions as seen\n4. After viewing revision 3 of a 5-revision page, revisions 4 and 5 should still appear as unseen in the watchlist\n5. The watchlist notification count should reflect only truly unseen revisions after diff views"
}
```

### Task-Conditioned

```json
{
  "files_to_edit": [
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "Line 1501 in the getNotificationTimestamp() method. Replace the line that gets the timestamp of the viewed revision with the timestamp of the NEXT revision after it."
    },
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "Lines 1147-1149 in showDiffPage() method. The code passes the 'new' (newer) revision ID to doViewUpdates, but for diffs comparing specific revisions, this marks all revisions up to the 'new' revision as seen. Consider whether the logic should instead be marking only the 'old' revision as seen, or both."
    }
  ],
  "root_cause": "The bug is in WatchedItemStore::getNotificationTimestamp() at line 1501. When a user views a specific revision, the code incorrectly uses the timestamp of the VIEWED revision instead of the timestamp of the NEXT (first unseen) revision. This causes the watchlist notification timestamp to be set incorrectly, making all revisions appear as 'seen' instead of only the viewed one. The commented FIXME at lines 1502-1503 explicitly identifies this issue: the code should use `$nextRev->getTimestamp()` for the first unseen revision timestamp, but currently uses `getTimestampFromId($oldid)` which gets the viewed revision's timestamp.",
  "fix_plan": "1. In WatchedItemStore::getNotificationTimestamp() (line 1501), replace `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();` to use the correct next revision's timestamp instead of the viewed revision's timestamp.\n\n2. In Article::showDiffPage() (lines 1147-1149), review whether passing the 'new' (right-side) revision ID to doViewUpdates is the correct behavior for diff pages. The current behavior marks all revisions up to the right-side revision as 'seen', which might not be the intended behavior when viewing a diff between two specific non-consecutive revisions (e.g., revision 5 vs 10). Consider whether the code should instead:\n   - Mark only the 'old' (left-side) revision as seen, OR\n   - Mark only the viewed revisions (both old and new) as seen appropriately\n\n3. Verify the interaction between the stash-based 'page seen timestamps' (resetNotificationTimestamp lines 1372-1403) and the database-stored 'wl_notificationtimestamp' (ActivityUpdateJob) to ensure they use consistent timestamps.",
  "testing": "1. Test viewing a specific revision/diff on a watchlisted page with multiple unread revisions. Verify that:\n   - Only the viewed revision(s) are marked as seen\n   - Unviewed revisions before and after remain marked as unseen\n   - The watchlist displays correctly showing only the truly unread revisions\n\n2. Test multiple scenarios:\n   - View revision 5 of 10: revisions 6-10 should show as unseen, 1-5 as seen\n   - View diff of revisions 5 and 10: verify the correct revisions are marked\n   - View the latest revision: all revisions should be marked as seen\n   - View revision 5, then later view revision 8: only revisions 9-10 should be unseen\n\n3. Check the diff page behavior specifically when diff='prev' or diff='next' is used, ensuring proper revision selection.\n\n4. Test the interaction with the stash-based seen timestamps vs database notification timestamps to ensure both systems stay synchronized."
}
```

## Tool Call Analysis

### Control (CTO off)

Total tool calls: 43

Top tools: `?` x43

| Tool | Count |
|---|---|
| `?` | 43 |

### Control (CTO on)

Total tool calls: 43

Top tools: `?` x43

| Tool | Count |
|---|---|
| `?` | 43 |

### Explore

Total tool calls: 39

Top tools: `?` x39

| Tool | Count |
|---|---|
| `?` | 39 |

### Leverage

Total tool calls: 28

Top tools: `?` x28

| Tool | Count |
|---|---|
| `?` | 28 |

### Task-Conditioned

Total tool calls: 29

Top tools: `?` x29

| Tool | Count |
|---|---|
| `?` | 29 |

## Context Pack Audit

- Navigation items: 0
- Risk items: 0

## Graph Quality Notes

N/A

## Prompt Effectiveness

### Control (CTO off)

- Prompt chars: 945
- Input tokens: 734
- Output tokens: 23603

### Control (CTO on)

- Prompt chars: 943
- Input tokens: 778
- Output tokens: 36320

### Explore

- Prompt chars: 929
- Input tokens: 712
- Output tokens: 29891

### Leverage

- Prompt chars: 1170
- Input tokens: 522
- Output tokens: 37687

### Task-Conditioned

- Prompt chars: 1069
- Input tokens: 516
- Output tokens: 59185

## Lessons & Action Items

- Keep context packs concise and explicitly prioritize high-confidence nodes.
- Capture parser visibility deltas across conditions to track graph quality drift.

## Verdict

**Control (CTO off)** scored highest (37.91/100), **Task-Conditioned** lowest (25.25/100). Best overall value versus the control baseline: **Control (CTO off)** (100.00 recalculated eval score). Most efficient: Control (CTO off) ($1.041), most expensive: Explore ($1.377). All conditions passed tests.

## Notes

Regression eval: post graph-side work (35ac25c PHP parser, 7a01c32 anchor reorder, fe15650 symbol-anchor promotion, bdaf014 multi-signal scorer) + parallel team's experience/onboarding tooling.

---

## Raw Data

### Reference Output

```json
{
  "bug_id": "T419918",
  "title": "Viewing a diff/revision on a watchlisted page marks all revisions as seen",
  "files_to_edit": [
    {
      "path": "includes/Page/WikiPage.php",
      "what_to_change": "Change doViewUpdates() signature: replace $oldid integer param with $oldRev RevisionRecord param. Add deprecation shim for callers still passing an integer."
    },
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "Update 3 call sites of doViewUpdates(): stop passing $oldid integer, pass RevisionRecord object instead. In showDiffPage(), use $de->getNewRevision() instead of (int)$new."
    },
    {
      "path": "includes/Page/ImagePage.php",
      "what_to_change": "Update doViewUpdates() call: replace $this->getOldID() with $this->fetchRevisionRecord()."
    },
    {
      "path": "RELEASE-NOTES-1.46",
      "what_to_change": "Document that passing oldid to WikiPage::doViewUpdates is deprecated; pass RevisionRecord instead."
    }
  ],
  "root_cause": "Article::showDiffPage() passes a revision ID (integer) to WikiPage::doViewUpdates(), but the downstream code in WatchlistManager::clearTitleUserNotifications() needs a RevisionRecord to correctly identify which specific revision was viewed. When an integer is passed, the notification clearing code marks ALL revisions as seen instead of just the one being viewed.",
  "root_cause_keywords": [
    "integer",
    "RevisionRecord",
    "doViewUpdates",
    "showDiffPage",
    "clearTitleUserNotifications",
    "WatchlistManager",
    "oldid",
    "revision",
    "diff"
  ],
  "fix_plan": "1. Change WikiPage::doViewUpdates() signature to accept RevisionRecord|null instead of int $oldid. 2. Add a deprecation shim using func_num_args() to detect callers passing the old integer signature. 3. Update Article.php call sites: remove $oldid param, pass RevisionRecord objects (fetchRevisionRecord() or getNewRevision()). 4. Update ImagePage.php: replace getOldID() with fetchRevisionRecord(). 5. Update RELEASE-NOTES-1.46 to document the deprecation.",
  "fix_plan_keywords": [
    "RevisionRecord",
    "deprecat",
    "signature",
    "func_num_args",
    "fetchRevisionRecord",
    "getNewRevision",
    "getOldID"
  ],
  "testing": "Verify that viewing a diff or a specific revision only marks the target revision as seen, not all revisions. Add or update regression coverage around the diff/revision path and watchlist notification clearing.",
  "testing_keywords": [
    "diff",
    "revision",
    "watchlist",
    "notification",
    "seen",
    "regression",
    "clearTitleUserNotifications",
    "doViewUpdates"
  ],
  "commit": "425c358d279e0610365cda8fbe01d889f11238f0"
}
```

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "files_to_edit",
    "root_cause",
    "fix_plan",
    "testing"
  ],
  "properties": {
    "files_to_edit": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "path",
          "what_to_change"
        ],
        "properties": {
          "path": {
            "type": "string",
            "description": "Relative path to the file that needs editing"
          },
          "what_to_change": {
            "type": "string",
            "description": "What specifically needs to change in this file"
          }
        }
      },
      "description": "Files that need editing to fix the bug"
    },
    "root_cause": {
      "type": "string",
      "description": "Technical explanation of why the bug occurs \u2014 what code path leads to the wrong behavior"
    },
    "fix_plan": {
      "type": "string",
      "description": "Step-by-step explanation of how you would fix the bug. Do NOT apply the fix \u2014 describe what changes are needed."
    },
    "testing": {
      "type": "string",
      "description": "How to verify the fix and what regressions should be checked."
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "tunability": {
    "files_identified": "medium",
    "root_cause_quality": "high",
    "fix_plan_quality": "high",
    "testing_quality": "high",
    "efficiency": "high"
  },
  "notes": [
    "files_identified: set overlap of paths with reference. Partial credit per file.",
    "root_cause_quality: keyword match for key concepts (RevisionRecord, integer vs object, doViewUpdates, showDiffPage).",
    "fix_plan_quality: keyword match for fix approach (pass RevisionRecord, deprecate $oldid, signature change).",
    "testing_quality: keyword match for validation plan and regression coverage around diff/revision watchlist behavior.",
    "efficiency: lower token usage scores higher.",
    "tunability: per-component susceptibility to gaming. low = hard to inflate without genuine capability (e.g., binary external test pass, exact-path match). medium = inflatable with effort (e.g., set-overlap recall \u2014 return more candidates). high = trivially inflatable (e.g., keyword presence \u2014 sprinkle the keyword list; efficiency \u2014 caching/prompt-min that doesn't reflect skill). ANNOTATION ONLY: do not change weights based on these values without independent evidence of measurement bias."
  ]
}
```

### Per-Condition Run Records

#### Control (CTO off)

```json
{
  "input_tokens": 734,
  "output_tokens": 23603,
  "cache_read_tokens": 8174925,
  "cache_create_tokens": 292394,
  "cost_usd": 1.0413872,
  "duration_seconds": 217.154,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method around line 1145-1150, after calling $de->showDiffPage(), retrieve the actual new RevisionRecord from DifferenceEngine and pass it to doViewUpdates() instead of passing the potentially zero-valued $new integer. Change from passing (int)$new as the $oldid parameter to passing the RevisionRecord object as the third $oldRev parameter, or extract the actual revision ID from the DifferenceEngine's loaded revision data."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the resetNotificationTimestamp() method, add better handling to distinguish between 'no specific revision' (which should mark the entire page as seen) and 'view a specific older revision' (which should only mark that revision as seen). Alternatively, ensure the logic properly handles the case where a specific revision is being viewed versus viewing the current page state."
      }
    ],
    "root_cause": "When viewing a diff, Article.php's showDiffPage() method extracts the new revision ID from mapDiffPrevNext() and passes it to doViewUpdates() as the $oldid parameter. However, when $new is 0 (representing the latest/current revision) or false, converting it to (int)$new results in 0 being passed. The WatchedItemStore.resetNotificationTimestamp() method interprets $oldid=0 as 'no specific revision provided' and fetches the latest revision to use for marking the entire page as seen. This causes all revisions to be marked as seen instead of just the specific revision being viewed in the diff.",
    "fix_plan": "The fix should retrieve the actual RevisionRecord for the new side of the diff from the DifferenceEngine (which is loaded after calling showDiffPage() via loadRevisionData()) and pass it as the third parameter to doViewUpdates(). Specifically: (1) After calling $de->showDiffPage(), retrieve the new revision with $newRev = $de->getNewRevision(); (2) Call doViewUpdates() with the revision record: $this->mPage->doViewUpdates($context->getAuthority(), 0, $newRev); OR pass the actual revision ID if the second parameter is preferred. This ensures the watchlist clearing logic marks only the specific revision being viewed as seen, not all revisions up to the latest.",
    "testing": "Test by: (1) Creating a test page with multiple revisions; (2) Watching the page; (3) Viewing a diff between two older revisions (not involving the latest revision); (4) Verify in the watchlist that only the new side of the viewed diff is marked as seen, and older revisions remain marked as unseen; (5) Test edge cases: viewing diff with latest revision, viewing diff between very old revisions, using diff=prev and diff=next parameters; (6) Verify the watchlist notification timestamp correctly reflects viewing that specific revision rather than the entire page's latest revision."
  },
  "exit_code": 0,
  "tool_calls": [
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    }
  ]
}
```

#### Control (CTO on)

```json
{
  "input_tokens": 778,
  "output_tokens": 36320,
  "cache_read_tokens": 9224632,
  "cache_create_tokens": 429096,
  "cost_usd": 1.31296896,
  "duration_seconds": 2113.727,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In showDiffPage() method around line 1149, modify the watchlist update logic to only call doViewUpdates() when viewing the latest revision. Currently it unconditionally calls `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )` which marks all revisions as seen regardless of which revision is being viewed in the diff. The fix is to add a conditional: only call doViewUpdates() if `!$new` (when the new revision is false, indicating the latest). Change from: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new );` to: `if ( !$new ) { $this->mPage->doViewUpdates( $context->getAuthority() ); }`"
      }
    ],
    "root_cause": "In Article.php's showDiffPage() method (line 1149), the code unconditionally calls `doViewUpdates()` with the new revision ID from the diff comparison, regardless of whether that revision is the latest or an old one. The mapDiffPrevNext() method returns false for $new when viewing the latest revision. Currently, the code calls `doViewUpdates( $context->getAuthority(), (int)$new )` in all cases. When $new is false (viewing latest), (int)false converts to 0, which causes resetNotificationTimestamp() to call getNotificationTimestamp() with oldid=0. This returns null, clearing the entire wl_notificationtimestamp field for that page, marking ALL revisions as 'seen'. When $new is a specific revision ID (viewing an old revision), it gets passed as oldid to resetNotificationTimestamp(), which then marks all revisions up to and including that one as seen. The bug is that watchlist updates should only occur when viewing the actual page content or when viewing a diff to the latest revision, not when viewing diffs to arbitrary old revisions.",
    "fix_plan": "The fix is to add a conditional check before calling doViewUpdates(). Since mapDiffPrevNext() returns false when viewing the latest revision, we should only call doViewUpdates() when !$new. This ensures: (1) When viewing a diff to the latest revision ($new=false), doViewUpdates is called with the default oldid=0, properly marking the page as fully viewed; (2) When viewing a diff to an old revision ($new=some_id), doViewUpdates is NOT called, leaving the watchlist notification timestamp unchanged. This matches the expected behavior where watchlist notifications are only cleared when actually viewing page content or a diff to the latest version, not when just browsing through historical diffs.",
    "testing": "Verify the fix with these test cases: (1) Create a watchlisted page with 3+ revisions; (2) View watchlist - page should show unread indicator; (3) View a diff between two old revisions (e.g., oldid=100&diff=102) - return to watchlist, page should STILL show unread; (4) View a diff to the latest revision (diff=next on second-to-last, or oldid=old&diff=latest) - return to watchlist, page should now show as fully read; (5) Test diff=prev on various revisions; (6) Test diff=next on second-to-last revision (should mark latest as seen); (7) Test edge cases with deleted/archived revisions. Each test should verify that only the appropriate revisions are marked as seen in the watchlist."
  },
  "exit_code": 0,
  "tool_calls": [
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    }
  ]
}
```

#### Explore

```json
{
  "input_tokens": 712,
  "output_tokens": 29891,
  "cache_read_tokens": 8215917,
  "cache_create_tokens": 599141,
  "cost_usd": 1.37654796,
  "duration_seconds": 745.422,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1150), when calling doViewUpdates(), pass the old revision record in addition to the new revision ID to ensure the watchlist notification system knows which specific revision was viewed in the diff"
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Ensure clearTitleUserNotifications() correctly handles the case where a diff is being viewed - it should only mark the specific revisions being compared as seen, not all revisions on the page"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Review resetNotificationTimestamp() to ensure it correctly handles diff viewing scenarios where both old and new revisions might need to be marked as seen separately, rather than always updating to the latest revision timestamp"
      }
    ],
    "root_cause": "When viewing a diff/revision on a watchlisted page, the Article.php showDiffPage() method calls WikiPage::doViewUpdates() with only the new revision ID (line 1150: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )`), without properly passing the old revision record. This causes WatchedItemStore::resetNotificationTimestamp() to update the notification timestamp to the new revision's timestamp. Since the watchlist notification system marks revisions as unseen only if their timestamp is >= the notification timestamp, updating to the new revision's timestamp causes all older revisions to appear as 'seen'. If the new revision happens to be the latest, all revisions on the page become marked as seen instead of just the ones in the diff view.",
    "fix_plan": "1. Modify Article.php showDiffPage() to pass the old revision record (via the $oldRev parameter) to doViewUpdates() when viewing a diff, not just the integer ID. 2. Ensure that both the old and new revisions from the diff are available to the watchlist notification system. 3. Update WatchedItemStore::resetNotificationTimestamp() or the diff viewing code to only mark the specific revisions being viewed in the diff as seen, rather than all revisions up to the latest. 4. Consider whether viewing a diff between revisions A and B should mark only B as seen (typical behavior) or both A and B as seen (since both are being viewed in the diff comparison).",
    "testing": "1. Create a watchlisted page with multiple revisions. 2. View the watchlist and verify which revisions are marked as unseen (have the 'updatedmarker'). 3. Click on a diff link to view a diff between two specific revisions (not the latest). 4. Return to the watchlist or history page. 5. Verify that ONLY the revisions that were in the diff view are marked as seen, and any newer revisions still appear as unseen. 6. Repeat with a diff involving the latest revision to ensure the behavior is consistent. 7. Test edge cases: first revision, diff with 'prev'/'next' parameters, and diffs on the latest revision."
  },
  "exit_code": 0,
  "tool_calls": [
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    }
  ]
}
```

#### Leverage

```json
{
  "input_tokens": 522,
  "output_tokens": 37687,
  "cache_read_tokens": 6633364,
  "cache_create_tokens": 383061,
  "cost_usd": 1.06489572,
  "duration_seconds": 346.36,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Lines 1145-1149: When calling doViewUpdates() during a diff view, pass the older revision ID (not the newer one) to avoid triggering the 'latest revision' case that clears all notifications"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Lines 1476-1521 (getNotificationTimestamp function): Fix logic to properly handle the case where oldid is the latest revision in a diff view - should use the timestamp of that revision instead of returning null (which clears all notifications)"
      }
    ],
    "root_cause": "When viewing a diff where the newer/right-side revision is the latest revision of the page, Article.php passes that revision's ID (line 1149) to doViewUpdates(). WatchedItemStore::getNotificationTimestamp() interprets this as viewing the latest revision standalone and returns null to clear the notification timestamp. Setting wl_notificationtimestamp to NULL in the watchlist marks ALL revisions as 'seen' instead of just the ones in the viewed diff. The commented code at line 1503 suggests this issue was previously recognized but not fully fixed.",
    "fix_plan": "The fix requires changes in two files:\n\n1. **includes/Page/Article.php (Line 1147-1149)**: When viewing a diff where the newer revision is the latest, the code should NOT pass the newer revision's ID to doViewUpdates(). Instead, it should pass the older revision's ID. This ensures the watchlist is updated to mark revisions up to the older one as seen, while leaving a proper notification timestamp for newer revisions.\n\nAlternatively/Additionally: 2. **includes/Watchlist/WatchedItemStore.php (Lines 1476-1521 in getNotificationTimestamp)**: The function should distinguish between viewing a single revision (where null is correct for the latest) versus viewing a diff where the revision being marked is the latest. When oldid is the latest revision, instead of unconditionally returning null, it should return the timestamp of that revision. This preserves proper notification tracking for future revisions while marking current ones as seen.",
    "testing": "Test cases should verify:\n1. Viewing a single revision page marks that revision and earlier ones as seen\n2. Viewing a diff between two revisions (where neither is the latest) marks both revisions as seen\n3. Viewing a diff where the newer revision IS the latest revision should mark up to that revision as seen, but NOT mark future revisions as seen\n4. After viewing revision 3 of a 5-revision page, revisions 4 and 5 should still appear as unseen in the watchlist\n5. The watchlist notification count should reflect only truly unseen revisions after diff views"
  },
  "exit_code": 0,
  "tool_calls": [
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    }
  ]
}
```

#### Task-Conditioned

```json
{
  "input_tokens": 516,
  "output_tokens": 59185,
  "cache_read_tokens": 7968232,
  "cache_create_tokens": 203383,
  "cost_usd": 1.07799436,
  "duration_seconds": 314.785,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Line 1501 in the getNotificationTimestamp() method. Replace the line that gets the timestamp of the viewed revision with the timestamp of the NEXT revision after it."
      },
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Lines 1147-1149 in showDiffPage() method. The code passes the 'new' (newer) revision ID to doViewUpdates, but for diffs comparing specific revisions, this marks all revisions up to the 'new' revision as seen. Consider whether the logic should instead be marking only the 'old' revision as seen, or both."
      }
    ],
    "root_cause": "The bug is in WatchedItemStore::getNotificationTimestamp() at line 1501. When a user views a specific revision, the code incorrectly uses the timestamp of the VIEWED revision instead of the timestamp of the NEXT (first unseen) revision. This causes the watchlist notification timestamp to be set incorrectly, making all revisions appear as 'seen' instead of only the viewed one. The commented FIXME at lines 1502-1503 explicitly identifies this issue: the code should use `$nextRev->getTimestamp()` for the first unseen revision timestamp, but currently uses `getTimestampFromId($oldid)` which gets the viewed revision's timestamp.",
    "fix_plan": "1. In WatchedItemStore::getNotificationTimestamp() (line 1501), replace `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();` to use the correct next revision's timestamp instead of the viewed revision's timestamp.\n\n2. In Article::showDiffPage() (lines 1147-1149), review whether passing the 'new' (right-side) revision ID to doViewUpdates is the correct behavior for diff pages. The current behavior marks all revisions up to the right-side revision as 'seen', which might not be the intended behavior when viewing a diff between two specific non-consecutive revisions (e.g., revision 5 vs 10). Consider whether the code should instead:\n   - Mark only the 'old' (left-side) revision as seen, OR\n   - Mark only the viewed revisions (both old and new) as seen appropriately\n\n3. Verify the interaction between the stash-based 'page seen timestamps' (resetNotificationTimestamp lines 1372-1403) and the database-stored 'wl_notificationtimestamp' (ActivityUpdateJob) to ensure they use consistent timestamps.",
    "testing": "1. Test viewing a specific revision/diff on a watchlisted page with multiple unread revisions. Verify that:\n   - Only the viewed revision(s) are marked as seen\n   - Unviewed revisions before and after remain marked as unseen\n   - The watchlist displays correctly showing only the truly unread revisions\n\n2. Test multiple scenarios:\n   - View revision 5 of 10: revisions 6-10 should show as unseen, 1-5 as seen\n   - View diff of revisions 5 and 10: verify the correct revisions are marked\n   - View the latest revision: all revisions should be marked as seen\n   - View revision 5, then later view revision 8: only revisions 9-10 should be unseen\n\n3. Check the diff page behavior specifically when diff='prev' or diff='next' is used, ensuring proper revision selection.\n\n4. Test the interaction with the stash-based seen timestamps vs database notification timestamps to ensure both systems stay synchronized."
  },
  "exit_code": 0,
  "tool_calls": [
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    },
    {
      "name": "?"
    }
  ]
}
```

### Per-Condition Assessments

#### Control (CTO off)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.5555555555555556,
    "fix_plan_quality": 0.2857142857142857,
    "testing_quality": 0.625,
    "efficiency": 0.1611100871670016
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 37.91,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php",
    "includes/Page/WikiPage.php"
  ],
  "files_extra": [
    "includes/Watchlist/WatchedItemStore.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method around line 1145-1150, after calling $de->showDiffPage(), retrieve the actual new RevisionRecord from DifferenceEngine and pass it to doViewUpdates() instead of passing the potentially zero-valued $new integer. Change from passing (int)$new as the $oldid parameter to passing the RevisionRecord object as the third $oldRev parameter, or extract the actual revision ID from the DifferenceEngine's loaded revision data."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the resetNotificationTimestamp() method, add better handling to distinguish between 'no specific revision' (which should mark the entire page as seen) and 'view a specific older revision' (which should only mark that revision as seen). Alternatively, ensure the logic properly handles the case where a specific revision is being viewed versus viewing the current page state."
      }
    ],
    "root_cause": "When viewing a diff, Article.php's showDiffPage() method extracts the new revision ID from mapDiffPrevNext() and passes it to doViewUpdates() as the $oldid parameter. However, when $new is 0 (representing the latest/current revision) or false, converting it to (int)$new results in 0 being passed. The WatchedItemStore.resetNotificationTimestamp() method interprets $oldid=0 as 'no specific revision provided' and fetches the latest revision to use for marking the entire page as seen. This causes all revisions to be marked as seen instead of just the specific revision being viewed in the diff.",
    "fix_plan": "The fix should retrieve the actual RevisionRecord for the new side of the diff from the DifferenceEngine (which is loaded after calling showDiffPage() via loadRevisionData()) and pass it as the third parameter to doViewUpdates(). Specifically: (1) After calling $de->showDiffPage(), retrieve the new revision with $newRev = $de->getNewRevision(); (2) Call doViewUpdates() with the revision record: $this->mPage->doViewUpdates($context->getAuthority(), 0, $newRev); OR pass the actual revision ID if the second parameter is preferred. This ensures the watchlist clearing logic marks only the specific revision being viewed as seen, not all revisions up to the latest.",
    "testing": "Test by: (1) Creating a test page with multiple revisions; (2) Watching the page; (3) Viewing a diff between two older revisions (not involving the latest revision); (4) Verify in the watchlist that only the new side of the viewed diff is marked as seen, and older revisions remain marked as unseen; (5) Test edge cases: viewing diff with latest revision, viewing diff between very old revisions, using diff=prev and diff=next parameters; (6) Verify the watchlist notification timestamp correctly reflects viewing that specific revision rather than the entire page's latest revision."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 37.91,
    "normalized_score": 37.91,
    "normalization_delta": 0.0
  }
}
```

#### Control (CTO on)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.5555555555555556,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.13219041850005964
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 31.46,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php",
    "includes/Page/WikiPage.php"
  ],
  "files_extra": [],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In showDiffPage() method around line 1149, modify the watchlist update logic to only call doViewUpdates() when viewing the latest revision. Currently it unconditionally calls `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )` which marks all revisions as seen regardless of which revision is being viewed in the diff. The fix is to add a conditional: only call doViewUpdates() if `!$new` (when the new revision is false, indicating the latest). Change from: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new );` to: `if ( !$new ) { $this->mPage->doViewUpdates( $context->getAuthority() ); }`"
      }
    ],
    "root_cause": "In Article.php's showDiffPage() method (line 1149), the code unconditionally calls `doViewUpdates()` with the new revision ID from the diff comparison, regardless of whether that revision is the latest or an old one. The mapDiffPrevNext() method returns false for $new when viewing the latest revision. Currently, the code calls `doViewUpdates( $context->getAuthority(), (int)$new )` in all cases. When $new is false (viewing latest), (int)false converts to 0, which causes resetNotificationTimestamp() to call getNotificationTimestamp() with oldid=0. This returns null, clearing the entire wl_notificationtimestamp field for that page, marking ALL revisions as 'seen'. When $new is a specific revision ID (viewing an old revision), it gets passed as oldid to resetNotificationTimestamp(), which then marks all revisions up to and including that one as seen. The bug is that watchlist updates should only occur when viewing the actual page content or when viewing a diff to the latest revision, not when viewing diffs to arbitrary old revisions.",
    "fix_plan": "The fix is to add a conditional check before calling doViewUpdates(). Since mapDiffPrevNext() returns false when viewing the latest revision, we should only call doViewUpdates() when !$new. This ensures: (1) When viewing a diff to the latest revision ($new=false), doViewUpdates is called with the default oldid=0, properly marking the page as fully viewed; (2) When viewing a diff to an old revision ($new=some_id), doViewUpdates is NOT called, leaving the watchlist notification timestamp unchanged. This matches the expected behavior where watchlist notifications are only cleared when actually viewing page content or a diff to the latest version, not when just browsing through historical diffs.",
    "testing": "Verify the fix with these test cases: (1) Create a watchlisted page with 3+ revisions; (2) View watchlist - page should show unread indicator; (3) View a diff between two old revisions (e.g., oldid=100&diff=102) - return to watchlist, page should STILL show unread; (4) View a diff to the latest revision (diff=next on second-to-last, or oldid=old&diff=latest) - return to watchlist, page should now show as fully read; (5) Test diff=prev on various revisions; (6) Test diff=next on second-to-last revision (should mark latest as seen); (7) Test edge cases with deleted/archived revisions. Each test should verify that only the appropriate revisions are marked as seen in the watchlist."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 31.46,
    "normalized_score": 31.46,
    "normalization_delta": 0.0
  }
}
```

#### Explore

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.4444444444444444,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.1268594454938117
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 28.63,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php",
    "includes/Page/WikiPage.php"
  ],
  "files_extra": [
    "includes/Watchlist/WatchedItemStore.php",
    "includes/Watchlist/WatchlistManager.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1150), when calling doViewUpdates(), pass the old revision record in addition to the new revision ID to ensure the watchlist notification system knows which specific revision was viewed in the diff"
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Ensure clearTitleUserNotifications() correctly handles the case where a diff is being viewed - it should only mark the specific revisions being compared as seen, not all revisions on the page"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Review resetNotificationTimestamp() to ensure it correctly handles diff viewing scenarios where both old and new revisions might need to be marked as seen separately, rather than always updating to the latest revision timestamp"
      }
    ],
    "root_cause": "When viewing a diff/revision on a watchlisted page, the Article.php showDiffPage() method calls WikiPage::doViewUpdates() with only the new revision ID (line 1150: `$this->mPage->doViewUpdates( $context->getAuthority(), (int)$new )`), without properly passing the old revision record. This causes WatchedItemStore::resetNotificationTimestamp() to update the notification timestamp to the new revision's timestamp. Since the watchlist notification system marks revisions as unseen only if their timestamp is >= the notification timestamp, updating to the new revision's timestamp causes all older revisions to appear as 'seen'. If the new revision happens to be the latest, all revisions on the page become marked as seen instead of just the ones in the diff view.",
    "fix_plan": "1. Modify Article.php showDiffPage() to pass the old revision record (via the $oldRev parameter) to doViewUpdates() when viewing a diff, not just the integer ID. 2. Ensure that both the old and new revisions from the diff are available to the watchlist notification system. 3. Update WatchedItemStore::resetNotificationTimestamp() or the diff viewing code to only mark the specific revisions being viewed in the diff as seen, rather than all revisions up to the latest. 4. Consider whether viewing a diff between revisions A and B should mark only B as seen (typical behavior) or both A and B as seen (since both are being viewed in the diff comparison).",
    "testing": "1. Create a watchlisted page with multiple revisions. 2. View the watchlist and verify which revisions are marked as unseen (have the 'updatedmarker'). 3. Click on a diff link to view a diff between two specific revisions (not the latest). 4. Return to the watchlist or history page. 5. Verify that ONLY the revisions that were in the diff view are marked as seen, and any newer revisions still appear as unseen. 6. Repeat with a diff involving the latest revision to ensure the behavior is consistent. 7. Test edge cases: first revision, diff with 'prev'/'next' parameters, and diffs on the latest revision."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 28.63,
    "normalized_score": 28.63,
    "normalization_delta": 0.0
  }
}
```

#### Leverage

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.3333333333333333,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.15811580104010473
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 28.04,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php",
    "includes/Page/WikiPage.php"
  ],
  "files_extra": [
    "includes/Watchlist/WatchedItemStore.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Lines 1145-1149: When calling doViewUpdates() during a diff view, pass the older revision ID (not the newer one) to avoid triggering the 'latest revision' case that clears all notifications"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Lines 1476-1521 (getNotificationTimestamp function): Fix logic to properly handle the case where oldid is the latest revision in a diff view - should use the timestamp of that revision instead of returning null (which clears all notifications)"
      }
    ],
    "root_cause": "When viewing a diff where the newer/right-side revision is the latest revision of the page, Article.php passes that revision's ID (line 1149) to doViewUpdates(). WatchedItemStore::getNotificationTimestamp() interprets this as viewing the latest revision standalone and returns null to clear the notification timestamp. Setting wl_notificationtimestamp to NULL in the watchlist marks ALL revisions as 'seen' instead of just the ones in the viewed diff. The commented code at line 1503 suggests this issue was previously recognized but not fully fixed.",
    "fix_plan": "The fix requires changes in two files:\n\n1. **includes/Page/Article.php (Line 1147-1149)**: When viewing a diff where the newer revision is the latest, the code should NOT pass the newer revision's ID to doViewUpdates(). Instead, it should pass the older revision's ID. This ensures the watchlist is updated to mark revisions up to the older one as seen, while leaving a proper notification timestamp for newer revisions.\n\nAlternatively/Additionally: 2. **includes/Watchlist/WatchedItemStore.php (Lines 1476-1521 in getNotificationTimestamp)**: The function should distinguish between viewing a single revision (where null is correct for the latest) versus viewing a diff where the revision being marked is the latest. When oldid is the latest revision, instead of unconditionally returning null, it should return the timestamp of that revision. This preserves proper notification tracking for future revisions while marking current ones as seen.",
    "testing": "Test cases should verify:\n1. Viewing a single revision page marks that revision and earlier ones as seen\n2. Viewing a diff between two revisions (where neither is the latest) marks both revisions as seen\n3. Viewing a diff where the newer revision IS the latest revision should mark up to that revision as seen, but NOT mark future revisions as seen\n4. After viewing revision 3 of a 5-revision page, revisions 4 and 5 should still appear as unseen in the watchlist\n5. The watchlist notification count should reflect only truly unseen revisions after diff views"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 28.04,
    "normalized_score": 28.04,
    "normalization_delta": 0.0
  }
}
```

#### Task-Conditioned

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.2222222222222222,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.15649521332785854
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 25.25,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php",
    "includes/Page/WikiPage.php"
  ],
  "files_extra": [
    "includes/Watchlist/WatchedItemStore.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Line 1501 in the getNotificationTimestamp() method. Replace the line that gets the timestamp of the viewed revision with the timestamp of the NEXT revision after it."
      },
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Lines 1147-1149 in showDiffPage() method. The code passes the 'new' (newer) revision ID to doViewUpdates, but for diffs comparing specific revisions, this marks all revisions up to the 'new' revision as seen. Consider whether the logic should instead be marking only the 'old' revision as seen, or both."
      }
    ],
    "root_cause": "The bug is in WatchedItemStore::getNotificationTimestamp() at line 1501. When a user views a specific revision, the code incorrectly uses the timestamp of the VIEWED revision instead of the timestamp of the NEXT (first unseen) revision. This causes the watchlist notification timestamp to be set incorrectly, making all revisions appear as 'seen' instead of only the viewed one. The commented FIXME at lines 1502-1503 explicitly identifies this issue: the code should use `$nextRev->getTimestamp()` for the first unseen revision timestamp, but currently uses `getTimestampFromId($oldid)` which gets the viewed revision's timestamp.",
    "fix_plan": "1. In WatchedItemStore::getNotificationTimestamp() (line 1501), replace `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();` to use the correct next revision's timestamp instead of the viewed revision's timestamp.\n\n2. In Article::showDiffPage() (lines 1147-1149), review whether passing the 'new' (right-side) revision ID to doViewUpdates is the correct behavior for diff pages. The current behavior marks all revisions up to the right-side revision as 'seen', which might not be the intended behavior when viewing a diff between two specific non-consecutive revisions (e.g., revision 5 vs 10). Consider whether the code should instead:\n   - Mark only the 'old' (left-side) revision as seen, OR\n   - Mark only the viewed revisions (both old and new) as seen appropriately\n\n3. Verify the interaction between the stash-based 'page seen timestamps' (resetNotificationTimestamp lines 1372-1403) and the database-stored 'wl_notificationtimestamp' (ActivityUpdateJob) to ensure they use consistent timestamps.",
    "testing": "1. Test viewing a specific revision/diff on a watchlisted page with multiple unread revisions. Verify that:\n   - Only the viewed revision(s) are marked as seen\n   - Unviewed revisions before and after remain marked as unseen\n   - The watchlist displays correctly showing only the truly unread revisions\n\n2. Test multiple scenarios:\n   - View revision 5 of 10: revisions 6-10 should show as unseen, 1-5 as seen\n   - View diff of revisions 5 and 10: verify the correct revisions are marked\n   - View the latest revision: all revisions should be marked as seen\n   - View revision 5, then later view revision 8: only revisions 9-10 should be unseen\n\n3. Check the diff page behavior specifically when diff='prev' or diff='next' is used, ensuring proper revision selection.\n\n4. Test the interaction with the stash-based seen timestamps vs database notification timestamps to ensure both systems stay synchronized."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 25.25,
    "normalized_score": 25.25,
    "normalization_delta": 0.0
  }
}
```

