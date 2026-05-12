# Eval Report: Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug.

## Meta

- Date: 2026-05-10
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
- Eval Type: bug-fix-1
- Conditions: control-cto-off, control-cto-on, explore, leverage, task-conditioned
- Aethyme Commit: `e20d9151730a81a60fe357f5acb78b1ab3619ac4`

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

- **Cost:** `-13.18%` (explore $0.9027 → leverage $1.0218)
- **Tokens:** `-25.07%` (explore 5,971,937 → leverage 7,469,382)

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 50.13 | 100.0 | 37 | $0.997 | 178.9s | 7,715,409 | 0.01 | 16.82 |
| Control (CTO on) | 30.74 | 77.48 | 43 | $1.132 | 199.3s | 8,891,376 | 0.00 | 9.26 |
| Explore | 31.05 | 83.99 | 27 | $0.903 | 178.6s | 5,971,937 | 0.01 | 10.43 |
| Leverage | 33.65 | 82.84 | 36 | $1.022 | 195.3s | 7,469,382 | 0.00 | 10.34 |
| Task-Conditioned | 28.87 | 75.07 | 43 | $1.124 | 208.9s | 8,978,444 | 0.00 | 8.29 |

## Score Breakdown

| Component | Weight | Control (CTO off) | Control (CTO on) | Explore | Leverage | Task-Conditioned |
|---|---| --- | --- | --- | --- | --- |
| Files Identified | 35% | 0.500 | 0.250 | 0.250 | 0.250 | 0.250 |
| Root Cause Quality | 25% | 0.778 | 0.444 | 0.444 | 0.556 | 0.444 |
| Fix Plan Quality | 15% | 0.143 | 0.000 | 0.000 | 0.000 | 0.000 |
| Testing Quality | 15% | 0.625 | 0.625 | 0.625 | 0.625 | 0.500 |
| Efficiency | 10% | 0.167 | 0.150 | 0.181 | 0.164 | 0.151 |

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
      "what_to_change": "When calling doViewUpdates in the diff view (around line 1149), pass the RevisionRecord of the new revision being viewed instead of just the revision ID. The current code passes (int)$new which gets ignored, and passes null for $oldRev, causing all revisions to be marked as seen."
    },
    {
      "path": "includes/Page/WikiPage.php",
      "what_to_change": "Update doViewUpdates() to properly handle the $oldid parameter by loading the corresponding RevisionRecord if $oldRev is not provided, or ensure it's used when calling clearTitleUserNotifications(). Currently the $oldid parameter is accepted but completely ignored."
    }
  ],
  "root_cause": "When viewing a diff/revision on a watchlisted page, Article::showDiff() calls WikiPage::doViewUpdates() with only a revision ID (passing it as the $oldid parameter), but without the corresponding RevisionRecord ($oldRev parameter). WikiPage::doViewUpdates() only uses the $oldRev parameter when calling WatchlistManager::clearTitleUserNotifications(), completely ignoring the $oldid parameter. This causes $oldRev to be null, which is interpreted as 'viewing the latest revision'. In WatchedItemStore::resetNotificationTimestamp() and subsequently getNotificationTimestamp(), when oldid=0 or the oldRev is null, the code returns null from getNotificationTimestamp(), which clears (sets to null) the entire notification timestamp for the page, marking ALL revisions as 'seen' instead of just marking revisions up to the one being viewed.",
  "fix_plan": "The fix requires two coordinated changes: (1) In Article.php, modify the call to doViewUpdates() in the showDiff() method (around line 1149) to pass the RevisionRecord of the new revision being displayed, not just its ID. The code should load the RevisionRecord for the $new revision ID and pass it as the third parameter. (2) Alternatively or additionally, modify WikiPage::doViewUpdates() to properly extract and use the $oldid parameter when $oldRev is not provided - either by loading the RevisionRecord for that oldid or by passing the oldid to clearTitleUserNotifications() in a way that preserves the revision-specific behavior. The key is ensuring that the specific revision being viewed (not just the page) is tracked, so that future revisions still show as 'new' in the watchlist.",
  "testing": "To verify the fix: (1) Watch a page that has multiple revisions; (2) View an older diff (not the latest revision) using ?oldid=X&diff=Y parameters; (3) Check the watchlist - verify that only the revisions up to Y are marked as seen, and any revisions after Y still appear as new/unread; (4) Test with various combinations: viewing an old diff, viewing the latest diff, viewing diffs with many revisions between them. The expected behavior is that viewing a specific revision should only mark revisions up to that point as seen, allowing future edits to still appear as new notifications."
}
```

### Control (CTO on)

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (around line 1143-1149), the code calls doViewUpdates() with (int)$new. When $new is false (which happens when viewing diff=next on the latest revision), (int)false becomes 0. This causes doViewUpdates to call resetNotificationTimestamp with oldid=0, which in WatchedItemStore.php clears the entire notification timestamp for the page (returns null from getNotificationTimestamp), marking ALL revisions as seen instead of just the viewed revision."
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "The getNotificationTimestamp() method (line 1469-1521) returns null when !$oldid, which is meant to clear all notifications. However, when called from Article.php with oldid=0 (converted from false), this unintentionally clears all notifications for the page, even though the intent was to mark a specific revision as seen."
    }
  ],
  "root_cause": "In Article.php's showDiffPage() method (line 1149), when viewing a diff with diff=next and the current revision is the latest (no next revision exists), mapDiffPrevNext() returns false for the newer revision ID. The code then converts this to 0 via (int)false, intending to represent 'the latest revision'. However, this 0 value gets passed to doViewUpdates(), which interprets it as 'mark the entire page as fully viewed' rather than 'mark this specific revision as seen'. This causes resetNotificationTimestamp() to clear the entire notification timestamp (setting it to null), marking ALL revisions of the page as seen instead of just the one being viewed.",
  "fix_plan": "The fix should be in Article.php's showDiffPage() method. When $new is false (meaning the older revision is the latest), the code should either: (1) Pass $oldid instead of 0 to doViewUpdates(), since $oldid is the latest revision being shown in the diff, or (2) Add a conditional check to only call doViewUpdates() when $new is not false, or (3) Follow the same pattern used in DifferenceEngine::loadRevisionIds() to handle the false case more explicitly. The most appropriate fix is likely option 1: when $new is false, use $oldid instead, since that represents the revision actually being viewed and should be marked as seen.",
  "testing": "Test cases should include: (1) Viewing a regular diff between two specific revisions (oldid=X&diff=Y) - should mark revision Y as seen, leaving revisions after Y as unread, (2) Viewing diff=next on a revision that has a next revision - should mark the next revision as seen, (3) Viewing diff=next on the latest revision (no next revision exists, causing $new to be false) - should mark the current revision as seen WITHOUT clearing notifications for all revisions, (4) Verify in the watchlist that after viewing such a diff, only the appropriate revisions are marked as seen and unread revisions remain marked as such"
}
```

### Explore

```json
{
  "files_to_edit": [
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "Modify resetNotificationTimestamp() and getNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a diff vs. a full page, and handle the notification timestamp logic differently for each case. Currently, when viewing a diff, the code marks all revisions up to the viewed revision as 'seen', but it should only mark the specific revision(s) being compared as seen."
    },
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "Modify showDiffPage() to pass an additional parameter to doViewUpdates() or clearTitleUserNotifications() indicating that a diff is being viewed, rather than a full page view. This allows the watchlist notification logic to correctly handle diffs."
    },
    {
      "path": "includes/Watchlist/WatchlistManager.php",
      "what_to_change": "Modify clearTitleUserNotifications() to accept and pass through an optional parameter indicating the context of the notification clear (diff vs. page view). This ensures the parameter reaches resetNotificationTimestamp()."
    }
  ],
  "root_cause": "When viewing a diff on a watchlisted page, the system calls doViewUpdates() with the newer revision ID from the diff. This triggers resetNotificationTimestamp() which uses getNotificationTimestamp() to determine what notification timestamp to set. The problem is that getNotificationTimestamp() is designed to mark 'all revisions up to the specified revision as seen' rather than 'only this specific revision as seen'. When viewing a diff like ?oldid=100&diff=102, the code sets the notification timestamp as if the user viewed the entire page at revision 102, marking all intermediate revisions (100, 101, 102) as seen. If 102 is the latest revision (or close to it), this can mark significantly more revisions as 'seen' than were actually viewed in the diff comparison.",
  "fix_plan": "The fix requires distinguishing between two scenarios: (1) viewing a full page at a specific revision, where marking all earlier revisions as seen is correct, and (2) viewing a diff between two specific revisions, where only those specific revisions (or a more limited range) should be marked as seen. Implementation approach: 1) Add an optional 'isDiffView' parameter to clearTitleUserNotifications() in WatchlistManager.php, 2) Pass this parameter through to resetNotificationTimestamp() in WatchedItemStore.php, 3) Modify the notification timestamp calculation in getNotificationTimestamp() to handle the diff view case differently - perhaps by not setting a notification timestamp at all (returning false) or by setting it more conservatively so future revisions still trigger notifications, 4) Update the call in Article.php::showDiffPage() to pass isDiffView=true when calling doViewUpdates(). The key is that viewing a diff should not have the same side effect of marking the page as fully 'read' up to the newest revision shown.",
  "testing": "Test scenarios should include: 1) Viewing a diff between two non-latest revisions (?oldid=100&diff=102 where latest is 103+) - should not mark 103+ as seen, 2) Viewing a diff with the latest revision (?oldid=100&diff=cur) - behavior should be examined to determine if it should mark all as seen or not, 3) Viewing the full page at a specific revision - should still mark all earlier revisions as seen (existing behavior), 4) Checking that the watchlist correctly shows unseen revisions after diff viewing, 5) Verifying that email notifications are not suppressed for revisions that shouldn't have been marked as seen, 6) Testing with multiple users having the page watched at different points in the revision history"
}
```

### Leverage

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (around line 1145), modify the code that calls doViewUpdates to handle the case when mapDiffPrevNext returns false for the new revision. Currently it converts false to 0, which incorrectly marks the latest revision as seen."
    }
  ],
  "root_cause": "When viewing a diff on a watchlisted page, Article.php's showDiffPage() method calls mapDiffPrevNext() to determine which revision's notification should be cleared. However, when mapDiffPrevNext returns false for the new revision (e.g., when using 'diff=next' with no following revision), the code converts this false to 0 via (int)false. This 0 value is then passed to doViewUpdates, which passes it to WatchedItemStore.resetNotificationTimestamp. In resetNotificationTimestamp, when oldid=0, the code queries for the latest revision instead of the specific revision being viewed. This causes the watchlist notification to be cleared as if the user has seen ALL revisions up to the latest, rather than just the revisions in the diff they actually viewed.",
  "fix_plan": "Modify Article.php showDiffPage() (lines 1145-1147) to check if the new revision from mapDiffPrevNext is false before calling doViewUpdates. Only call doViewUpdates if $new is a valid revision ID. Specifically: (1) Change the current code that blindly converts false to 0 with (int)$new, (2) Add a condition to only call doViewUpdates if $new is not false, or alternatively only if $new is a positive integer. This ensures that viewing a diff of old revisions only marks those specific revisions as seen, not all revisions up to the latest. The fix should be in the range: [ , $new ] = $de->mapDiffPrevNext( $oldid, $diff ); if ( $new ) { $this->mPage->doViewUpdates(...); }",
  "testing": "To verify the fix: (1) Create a page with multiple revisions (e.g., revisions 100, 105, 110, 115), (2) Watch the page as a test user, (3) Navigate to a diff view like '?oldid=100&diff=next', (4) Verify the watchlist notification is NOT completely cleared but rather only updated to reflect that revision 105 (or the actual next revision) was seen, (5) Verify that if revisions 110 and 115 exist, they still appear as unseen in the watchlist, (6) Test with explicit diff parameters like '?oldid=100&diff=105' to ensure they still work correctly, (7) Test with 'diff=cur' to ensure viewing a diff against current revision properly marks the current revision as seen"
}
```

### Task-Conditioned

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In showDiffPage() method (around line 1145-1149), the code unconditionally calls doViewUpdates() with the newer revision from the diff. This causes the notification timestamp to be cleared (set to NULL) when the newer revision is the latest, marking ALL revisions as seen instead of only marking that specific revision viewed. The fix requires: (1) Only call doViewUpdates when the diff includes the latest revision, OR (2) Don't call doViewUpdates for diffs at all and let users explicitly mark pages as read, OR (3) Modify the logic to ensure that viewing a historical diff doesn't affect the watchlist notification state for the page."
    }
  ],
  "root_cause": "In Article.php showDiffPage() method (lines 1145-1149), the code gets the newer revision ID from the diff comparison via mapDiffPrevNext() and passes it to doViewUpdates(). This eventually calls WatchedItemStore::resetNotificationTimestamp() with that revision ID. The problem occurs in WatchedItemStore::getNotificationTimestamp() (lines 1487-1491): when the viewed revision is the latest revision of the page, the function returns null (to clear the timestamp), which marks the ENTIRE WATCHLIST ITEM as caught-up. This happens because setting wl_notificationtimestamp to NULL means the user has seen all changes, not just the specific revision viewed in the diff.",
  "fix_plan": "The core issue is that doViewUpdates() is being called with a diff revision ID in a way that causes the entire page's notification to be cleared when viewing a diff of the latest revision. There are multiple possible approaches: (1) Check if the newer revision in the diff is actually the latest revision of the page, and only call doViewUpdates() if the diff includes viewing the latest changes. (2) Remove the doViewUpdates() call from showDiffPage() entirely, since diffing revisions is a navigation action, not a viewing of the current article state. (3) Add a parameter to resetNotificationTimestamp() to indicate it's being called for a diff view, and modify getNotificationTimestamp() to avoid clearing the timestamp in that case. The most conservative fix would be approach (1): only mark the page as seen when viewing a diff that includes the current/latest revision of the page.",
  "testing": "Create a test case where: (1) User watches a page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5), (2) View a diff between revisions 1 and 2 or 3 and 4 (older revisions, not the latest), (3) Verify that the watchlist still shows the page as having unread changes (notification timestamp should not be cleared), (4) View a diff where the newer revision is the latest revision (e.g., diff between revision 4 and 5, or just view revision 5), (5) Verify that in this case, the notification timestamp IS cleared appropriately. Additionally, test edge cases: (a) viewing 'prev' diff when not at the latest, (b) viewing 'next' diff which brings you to the latest, (c) viewing a diff with 'diffonly' parameter set, (d) normal article view vs. diff view to ensure they behave differently."
}
```

## Tool Call Analysis

### Control (CTO off)

Total tool calls: 37

Top tools: `?` x37

| Tool | Count |
|---|---|
| `?` | 37 |

### Control (CTO on)

Total tool calls: 43

Top tools: `?` x43

| Tool | Count |
|---|---|
| `?` | 43 |

### Explore

Total tool calls: 27

Top tools: `?` x27

| Tool | Count |
|---|---|
| `?` | 27 |

### Leverage

Total tool calls: 36

Top tools: `?` x36

| Tool | Count |
|---|---|
| `?` | 36 |

### Task-Conditioned

Total tool calls: 43

Top tools: `?` x43

| Tool | Count |
|---|---|
| `?` | 43 |

## Context Pack Audit

- Navigation items: 0
- Risk items: 0

## Graph Quality Notes

N/A

## Prompt Effectiveness

### Control (CTO off)

- Prompt chars: 945
- Input tokens: 630
- Output tokens: 21947

### Control (CTO on)

- Prompt chars: 943
- Input tokens: 734
- Output tokens: 32049

### Explore

- Prompt chars: 929
- Input tokens: 494
- Output tokens: 34241

### Leverage

- Prompt chars: 1170
- Input tokens: 630
- Output tokens: 34350

### Task-Conditioned

- Prompt chars: 1069
- Input tokens: 738
- Output tokens: 27771

## Lessons & Action Items

- Keep context packs concise and explicitly prioritize high-confidence nodes.
- Capture parser visibility deltas across conditions to track graph quality drift.

## Verdict

**Control (CTO off)** scored highest (50.13/100), **Task-Conditioned** lowest (28.87/100). Best overall value versus the control baseline: **Control (CTO off)** (100.00 recalculated eval score). Most efficient: Explore ($0.903), most expensive: Control (CTO on) ($1.132). All conditions passed tests.

## Notes

N/A

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
  "input_tokens": 630,
  "output_tokens": 21947,
  "cache_read_tokens": 7374445,
  "cache_create_tokens": 318387,
  "cost_usd": 0.9966346,
  "duration_seconds": 178.856,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "When calling doViewUpdates in the diff view (around line 1149), pass the RevisionRecord of the new revision being viewed instead of just the revision ID. The current code passes (int)$new which gets ignored, and passes null for $oldRev, causing all revisions to be marked as seen."
      },
      {
        "path": "includes/Page/WikiPage.php",
        "what_to_change": "Update doViewUpdates() to properly handle the $oldid parameter by loading the corresponding RevisionRecord if $oldRev is not provided, or ensure it's used when calling clearTitleUserNotifications(). Currently the $oldid parameter is accepted but completely ignored."
      }
    ],
    "root_cause": "When viewing a diff/revision on a watchlisted page, Article::showDiff() calls WikiPage::doViewUpdates() with only a revision ID (passing it as the $oldid parameter), but without the corresponding RevisionRecord ($oldRev parameter). WikiPage::doViewUpdates() only uses the $oldRev parameter when calling WatchlistManager::clearTitleUserNotifications(), completely ignoring the $oldid parameter. This causes $oldRev to be null, which is interpreted as 'viewing the latest revision'. In WatchedItemStore::resetNotificationTimestamp() and subsequently getNotificationTimestamp(), when oldid=0 or the oldRev is null, the code returns null from getNotificationTimestamp(), which clears (sets to null) the entire notification timestamp for the page, marking ALL revisions as 'seen' instead of just marking revisions up to the one being viewed.",
    "fix_plan": "The fix requires two coordinated changes: (1) In Article.php, modify the call to doViewUpdates() in the showDiff() method (around line 1149) to pass the RevisionRecord of the new revision being displayed, not just its ID. The code should load the RevisionRecord for the $new revision ID and pass it as the third parameter. (2) Alternatively or additionally, modify WikiPage::doViewUpdates() to properly extract and use the $oldid parameter when $oldRev is not provided - either by loading the RevisionRecord for that oldid or by passing the oldid to clearTitleUserNotifications() in a way that preserves the revision-specific behavior. The key is ensuring that the specific revision being viewed (not just the page) is tracked, so that future revisions still show as 'new' in the watchlist.",
    "testing": "To verify the fix: (1) Watch a page that has multiple revisions; (2) View an older diff (not the latest revision) using ?oldid=X&diff=Y parameters; (3) Check the watchlist - verify that only the revisions up to Y are marked as seen, and any revisions after Y still appear as new/unread; (4) Test with various combinations: viewing an old diff, viewing the latest diff, viewing diffs with many revisions between them. The expected behavior is that viewing a specific revision should only mark revisions up to that point as seen, allowing future edits to still appear as new notifications."
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
    }
  ]
}
```

#### Control (CTO on)

```json
{
  "input_tokens": 734,
  "output_tokens": 32049,
  "cache_read_tokens": 8538834,
  "cache_create_tokens": 319759,
  "cost_usd": 1.13164892,
  "duration_seconds": 199.261,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1143-1149), the code calls doViewUpdates() with (int)$new. When $new is false (which happens when viewing diff=next on the latest revision), (int)false becomes 0. This causes doViewUpdates to call resetNotificationTimestamp with oldid=0, which in WatchedItemStore.php clears the entire notification timestamp for the page (returns null from getNotificationTimestamp), marking ALL revisions as seen instead of just the viewed revision."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The getNotificationTimestamp() method (line 1469-1521) returns null when !$oldid, which is meant to clear all notifications. However, when called from Article.php with oldid=0 (converted from false), this unintentionally clears all notifications for the page, even though the intent was to mark a specific revision as seen."
      }
    ],
    "root_cause": "In Article.php's showDiffPage() method (line 1149), when viewing a diff with diff=next and the current revision is the latest (no next revision exists), mapDiffPrevNext() returns false for the newer revision ID. The code then converts this to 0 via (int)false, intending to represent 'the latest revision'. However, this 0 value gets passed to doViewUpdates(), which interprets it as 'mark the entire page as fully viewed' rather than 'mark this specific revision as seen'. This causes resetNotificationTimestamp() to clear the entire notification timestamp (setting it to null), marking ALL revisions of the page as seen instead of just the one being viewed.",
    "fix_plan": "The fix should be in Article.php's showDiffPage() method. When $new is false (meaning the older revision is the latest), the code should either: (1) Pass $oldid instead of 0 to doViewUpdates(), since $oldid is the latest revision being shown in the diff, or (2) Add a conditional check to only call doViewUpdates() when $new is not false, or (3) Follow the same pattern used in DifferenceEngine::loadRevisionIds() to handle the false case more explicitly. The most appropriate fix is likely option 1: when $new is false, use $oldid instead, since that represents the revision actually being viewed and should be marked as seen.",
    "testing": "Test cases should include: (1) Viewing a regular diff between two specific revisions (oldid=X&diff=Y) - should mark revision Y as seen, leaving revisions after Y as unread, (2) Viewing diff=next on a revision that has a next revision - should mark the next revision as seen, (3) Viewing diff=next on the latest revision (no next revision exists, causing $new to be false) - should mark the current revision as seen WITHOUT clearing notifications for all revisions, (4) Verify in the watchlist that after viewing such a diff, only the appropriate revisions are marked as seen and unread revisions remain marked as such"
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
  "input_tokens": 494,
  "output_tokens": 34241,
  "cache_read_tokens": 5621542,
  "cache_create_tokens": 315660,
  "cost_usd": 0.9027425600000001,
  "duration_seconds": 178.57,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Modify resetNotificationTimestamp() and getNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a diff vs. a full page, and handle the notification timestamp logic differently for each case. Currently, when viewing a diff, the code marks all revisions up to the viewed revision as 'seen', but it should only mark the specific revision(s) being compared as seen."
      },
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Modify showDiffPage() to pass an additional parameter to doViewUpdates() or clearTitleUserNotifications() indicating that a diff is being viewed, rather than a full page view. This allows the watchlist notification logic to correctly handle diffs."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Modify clearTitleUserNotifications() to accept and pass through an optional parameter indicating the context of the notification clear (diff vs. page view). This ensures the parameter reaches resetNotificationTimestamp()."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, the system calls doViewUpdates() with the newer revision ID from the diff. This triggers resetNotificationTimestamp() which uses getNotificationTimestamp() to determine what notification timestamp to set. The problem is that getNotificationTimestamp() is designed to mark 'all revisions up to the specified revision as seen' rather than 'only this specific revision as seen'. When viewing a diff like ?oldid=100&diff=102, the code sets the notification timestamp as if the user viewed the entire page at revision 102, marking all intermediate revisions (100, 101, 102) as seen. If 102 is the latest revision (or close to it), this can mark significantly more revisions as 'seen' than were actually viewed in the diff comparison.",
    "fix_plan": "The fix requires distinguishing between two scenarios: (1) viewing a full page at a specific revision, where marking all earlier revisions as seen is correct, and (2) viewing a diff between two specific revisions, where only those specific revisions (or a more limited range) should be marked as seen. Implementation approach: 1) Add an optional 'isDiffView' parameter to clearTitleUserNotifications() in WatchlistManager.php, 2) Pass this parameter through to resetNotificationTimestamp() in WatchedItemStore.php, 3) Modify the notification timestamp calculation in getNotificationTimestamp() to handle the diff view case differently - perhaps by not setting a notification timestamp at all (returning false) or by setting it more conservatively so future revisions still trigger notifications, 4) Update the call in Article.php::showDiffPage() to pass isDiffView=true when calling doViewUpdates(). The key is that viewing a diff should not have the same side effect of marking the page as fully 'read' up to the newest revision shown.",
    "testing": "Test scenarios should include: 1) Viewing a diff between two non-latest revisions (?oldid=100&diff=102 where latest is 103+) - should not mark 103+ as seen, 2) Viewing a diff with the latest revision (?oldid=100&diff=cur) - behavior should be examined to determine if it should mark all as seen or not, 3) Viewing the full page at a specific revision - should still mark all earlier revisions as seen (existing behavior), 4) Checking that the watchlist correctly shows unseen revisions after diff viewing, 5) Verifying that email notifications are not suppressed for revisions that shouldn't have been marked as seen, 6) Testing with multiple users having the page watched at different points in the revision history"
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
    }
  ]
}
```

#### Leverage

```json
{
  "input_tokens": 630,
  "output_tokens": 34350,
  "cache_read_tokens": 7120151,
  "cache_create_tokens": 314251,
  "cost_usd": 1.02176708,
  "duration_seconds": 195.321,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1145), modify the code that calls doViewUpdates to handle the case when mapDiffPrevNext returns false for the new revision. Currently it converts false to 0, which incorrectly marks the latest revision as seen."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, Article.php's showDiffPage() method calls mapDiffPrevNext() to determine which revision's notification should be cleared. However, when mapDiffPrevNext returns false for the new revision (e.g., when using 'diff=next' with no following revision), the code converts this false to 0 via (int)false. This 0 value is then passed to doViewUpdates, which passes it to WatchedItemStore.resetNotificationTimestamp. In resetNotificationTimestamp, when oldid=0, the code queries for the latest revision instead of the specific revision being viewed. This causes the watchlist notification to be cleared as if the user has seen ALL revisions up to the latest, rather than just the revisions in the diff they actually viewed.",
    "fix_plan": "Modify Article.php showDiffPage() (lines 1145-1147) to check if the new revision from mapDiffPrevNext is false before calling doViewUpdates. Only call doViewUpdates if $new is a valid revision ID. Specifically: (1) Change the current code that blindly converts false to 0 with (int)$new, (2) Add a condition to only call doViewUpdates if $new is not false, or alternatively only if $new is a positive integer. This ensures that viewing a diff of old revisions only marks those specific revisions as seen, not all revisions up to the latest. The fix should be in the range: [ , $new ] = $de->mapDiffPrevNext( $oldid, $diff ); if ( $new ) { $this->mPage->doViewUpdates(...); }",
    "testing": "To verify the fix: (1) Create a page with multiple revisions (e.g., revisions 100, 105, 110, 115), (2) Watch the page as a test user, (3) Navigate to a diff view like '?oldid=100&diff=next', (4) Verify the watchlist notification is NOT completely cleared but rather only updated to reflect that revision 105 (or the actual next revision) was seen, (5) Verify that if revisions 110 and 115 exist, they still appear as unseen in the watchlist, (6) Test with explicit diff parameters like '?oldid=100&diff=105' to ensure they still work correctly, (7) Test with 'diff=cur' to ensure viewing a diff against current revision properly marks the current revision as seen"
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
    }
  ]
}
```

#### Task-Conditioned

```json
{
  "input_tokens": 738,
  "output_tokens": 27771,
  "cache_read_tokens": 8628093,
  "cache_create_tokens": 321842,
  "cost_usd": 1.12376384,
  "duration_seconds": 208.907,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In showDiffPage() method (around line 1145-1149), the code unconditionally calls doViewUpdates() with the newer revision from the diff. This causes the notification timestamp to be cleared (set to NULL) when the newer revision is the latest, marking ALL revisions as seen instead of only marking that specific revision viewed. The fix requires: (1) Only call doViewUpdates when the diff includes the latest revision, OR (2) Don't call doViewUpdates for diffs at all and let users explicitly mark pages as read, OR (3) Modify the logic to ensure that viewing a historical diff doesn't affect the watchlist notification state for the page."
      }
    ],
    "root_cause": "In Article.php showDiffPage() method (lines 1145-1149), the code gets the newer revision ID from the diff comparison via mapDiffPrevNext() and passes it to doViewUpdates(). This eventually calls WatchedItemStore::resetNotificationTimestamp() with that revision ID. The problem occurs in WatchedItemStore::getNotificationTimestamp() (lines 1487-1491): when the viewed revision is the latest revision of the page, the function returns null (to clear the timestamp), which marks the ENTIRE WATCHLIST ITEM as caught-up. This happens because setting wl_notificationtimestamp to NULL means the user has seen all changes, not just the specific revision viewed in the diff.",
    "fix_plan": "The core issue is that doViewUpdates() is being called with a diff revision ID in a way that causes the entire page's notification to be cleared when viewing a diff of the latest revision. There are multiple possible approaches: (1) Check if the newer revision in the diff is actually the latest revision of the page, and only call doViewUpdates() if the diff includes viewing the latest changes. (2) Remove the doViewUpdates() call from showDiffPage() entirely, since diffing revisions is a navigation action, not a viewing of the current article state. (3) Add a parameter to resetNotificationTimestamp() to indicate it's being called for a diff view, and modify getNotificationTimestamp() to avoid clearing the timestamp in that case. The most conservative fix would be approach (1): only mark the page as seen when viewing a diff that includes the current/latest revision of the page.",
    "testing": "Create a test case where: (1) User watches a page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5), (2) View a diff between revisions 1 and 2 or 3 and 4 (older revisions, not the latest), (3) Verify that the watchlist still shows the page as having unread changes (notification timestamp should not be cleared), (4) View a diff where the newer revision is the latest revision (e.g., diff between revision 4 and 5, or just view revision 5), (5) Verify that in this case, the notification timestamp IS cleared appropriately. Additionally, test edge cases: (a) viewing 'prev' diff when not at the latest, (b) viewing 'next' diff which brings you to the latest, (c) viewing a diff with 'diffonly' parameter set, (d) normal article view vs. diff view to ensure they behave differently."
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

### Per-Condition Assessments

#### Control (CTO off)

```json
{
  "scores": {
    "files_identified": 0.5,
    "root_cause_quality": 0.7777777777777778,
    "fix_plan_quality": 0.14285714285714285,
    "testing_quality": 0.625,
    "efficiency": 0.1671353978900493
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 50.13,
  "max_score": 100,
  "files_matched": [
    "includes/Page/Article.php",
    "includes/Page/WikiPage.php"
  ],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/ImagePage.php"
  ],
  "files_extra": [],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "When calling doViewUpdates in the diff view (around line 1149), pass the RevisionRecord of the new revision being viewed instead of just the revision ID. The current code passes (int)$new which gets ignored, and passes null for $oldRev, causing all revisions to be marked as seen."
      },
      {
        "path": "includes/Page/WikiPage.php",
        "what_to_change": "Update doViewUpdates() to properly handle the $oldid parameter by loading the corresponding RevisionRecord if $oldRev is not provided, or ensure it's used when calling clearTitleUserNotifications(). Currently the $oldid parameter is accepted but completely ignored."
      }
    ],
    "root_cause": "When viewing a diff/revision on a watchlisted page, Article::showDiff() calls WikiPage::doViewUpdates() with only a revision ID (passing it as the $oldid parameter), but without the corresponding RevisionRecord ($oldRev parameter). WikiPage::doViewUpdates() only uses the $oldRev parameter when calling WatchlistManager::clearTitleUserNotifications(), completely ignoring the $oldid parameter. This causes $oldRev to be null, which is interpreted as 'viewing the latest revision'. In WatchedItemStore::resetNotificationTimestamp() and subsequently getNotificationTimestamp(), when oldid=0 or the oldRev is null, the code returns null from getNotificationTimestamp(), which clears (sets to null) the entire notification timestamp for the page, marking ALL revisions as 'seen' instead of just marking revisions up to the one being viewed.",
    "fix_plan": "The fix requires two coordinated changes: (1) In Article.php, modify the call to doViewUpdates() in the showDiff() method (around line 1149) to pass the RevisionRecord of the new revision being displayed, not just its ID. The code should load the RevisionRecord for the $new revision ID and pass it as the third parameter. (2) Alternatively or additionally, modify WikiPage::doViewUpdates() to properly extract and use the $oldid parameter when $oldRev is not provided - either by loading the RevisionRecord for that oldid or by passing the oldid to clearTitleUserNotifications() in a way that preserves the revision-specific behavior. The key is ensuring that the specific revision being viewed (not just the page) is tracked, so that future revisions still show as 'new' in the watchlist.",
    "testing": "To verify the fix: (1) Watch a page that has multiple revisions; (2) View an older diff (not the latest revision) using ?oldid=X&diff=Y parameters; (3) Check the watchlist - verify that only the revisions up to Y are marked as seen, and any revisions after Y still appear as new/unread; (4) Test with various combinations: viewing an old diff, viewing the latest diff, viewing diffs with many revisions between them. The expected behavior is that viewing a specific revision should only mark revisions up to that point as seen, allowing future edits to still appear as new notifications."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 50.13,
    "normalized_score": 50.13,
    "normalization_delta": 0.0
  }
}
```

#### Control (CTO on)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.4444444444444444,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.15018973619563333
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 30.74,
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
        "what_to_change": "In the showDiffPage() method (around line 1143-1149), the code calls doViewUpdates() with (int)$new. When $new is false (which happens when viewing diff=next on the latest revision), (int)false becomes 0. This causes doViewUpdates to call resetNotificationTimestamp with oldid=0, which in WatchedItemStore.php clears the entire notification timestamp for the page (returns null from getNotificationTimestamp), marking ALL revisions as seen instead of just the viewed revision."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The getNotificationTimestamp() method (line 1469-1521) returns null when !$oldid, which is meant to clear all notifications. However, when called from Article.php with oldid=0 (converted from false), this unintentionally clears all notifications for the page, even though the intent was to mark a specific revision as seen."
      }
    ],
    "root_cause": "In Article.php's showDiffPage() method (line 1149), when viewing a diff with diff=next and the current revision is the latest (no next revision exists), mapDiffPrevNext() returns false for the newer revision ID. The code then converts this to 0 via (int)false, intending to represent 'the latest revision'. However, this 0 value gets passed to doViewUpdates(), which interprets it as 'mark the entire page as fully viewed' rather than 'mark this specific revision as seen'. This causes resetNotificationTimestamp() to clear the entire notification timestamp (setting it to null), marking ALL revisions of the page as seen instead of just the one being viewed.",
    "fix_plan": "The fix should be in Article.php's showDiffPage() method. When $new is false (meaning the older revision is the latest), the code should either: (1) Pass $oldid instead of 0 to doViewUpdates(), since $oldid is the latest revision being shown in the diff, or (2) Add a conditional check to only call doViewUpdates() when $new is not false, or (3) Follow the same pattern used in DifferenceEngine::loadRevisionIds() to handle the false case more explicitly. The most appropriate fix is likely option 1: when $new is false, use $oldid instead, since that represents the revision actually being viewed and should be marked as seen.",
    "testing": "Test cases should include: (1) Viewing a regular diff between two specific revisions (oldid=X&diff=Y) - should mark revision Y as seen, leaving revisions after Y as unread, (2) Viewing diff=next on a revision that has a next revision - should mark the next revision as seen, (3) Viewing diff=next on the latest revision (no next revision exists, causing $new to be false) - should mark the current revision as seen WITHOUT clearing notifications for all revisions, (4) Verify in the watchlist that after viewing such a diff, only the appropriate revisions are marked as seen and unread revisions remain marked as such"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 30.74,
    "normalized_score": 30.74,
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
    "testing_quality": 0.625,
    "efficiency": 0.1813659935279908
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 31.05,
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
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Modify resetNotificationTimestamp() and getNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a diff vs. a full page, and handle the notification timestamp logic differently for each case. Currently, when viewing a diff, the code marks all revisions up to the viewed revision as 'seen', but it should only mark the specific revision(s) being compared as seen."
      },
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Modify showDiffPage() to pass an additional parameter to doViewUpdates() or clearTitleUserNotifications() indicating that a diff is being viewed, rather than a full page view. This allows the watchlist notification logic to correctly handle diffs."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Modify clearTitleUserNotifications() to accept and pass through an optional parameter indicating the context of the notification clear (diff vs. page view). This ensures the parameter reaches resetNotificationTimestamp()."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, the system calls doViewUpdates() with the newer revision ID from the diff. This triggers resetNotificationTimestamp() which uses getNotificationTimestamp() to determine what notification timestamp to set. The problem is that getNotificationTimestamp() is designed to mark 'all revisions up to the specified revision as seen' rather than 'only this specific revision as seen'. When viewing a diff like ?oldid=100&diff=102, the code sets the notification timestamp as if the user viewed the entire page at revision 102, marking all intermediate revisions (100, 101, 102) as seen. If 102 is the latest revision (or close to it), this can mark significantly more revisions as 'seen' than were actually viewed in the diff comparison.",
    "fix_plan": "The fix requires distinguishing between two scenarios: (1) viewing a full page at a specific revision, where marking all earlier revisions as seen is correct, and (2) viewing a diff between two specific revisions, where only those specific revisions (or a more limited range) should be marked as seen. Implementation approach: 1) Add an optional 'isDiffView' parameter to clearTitleUserNotifications() in WatchlistManager.php, 2) Pass this parameter through to resetNotificationTimestamp() in WatchedItemStore.php, 3) Modify the notification timestamp calculation in getNotificationTimestamp() to handle the diff view case differently - perhaps by not setting a notification timestamp at all (returning false) or by setting it more conservatively so future revisions still trigger notifications, 4) Update the call in Article.php::showDiffPage() to pass isDiffView=true when calling doViewUpdates(). The key is that viewing a diff should not have the same side effect of marking the page as fully 'read' up to the newest revision shown.",
    "testing": "Test scenarios should include: 1) Viewing a diff between two non-latest revisions (?oldid=100&diff=102 where latest is 103+) - should not mark 103+ as seen, 2) Viewing a diff with the latest revision (?oldid=100&diff=cur) - behavior should be examined to determine if it should mark all as seen or not, 3) Viewing the full page at a specific revision - should still mark all earlier revisions as seen (existing behavior), 4) Checking that the watchlist correctly shows unseen revisions after diff viewing, 5) Verifying that email notifications are not suppressed for revisions that shouldn't have been marked as seen, 6) Testing with multiple users having the page watched at different points in the revision history"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 31.05,
    "normalized_score": 31.05,
    "normalization_delta": 0.0
  }
}
```

#### Leverage

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.5555555555555556,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.16369732273356064
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 33.65,
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
        "what_to_change": "In the showDiffPage() method (around line 1145), modify the code that calls doViewUpdates to handle the case when mapDiffPrevNext returns false for the new revision. Currently it converts false to 0, which incorrectly marks the latest revision as seen."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, Article.php's showDiffPage() method calls mapDiffPrevNext() to determine which revision's notification should be cleared. However, when mapDiffPrevNext returns false for the new revision (e.g., when using 'diff=next' with no following revision), the code converts this false to 0 via (int)false. This 0 value is then passed to doViewUpdates, which passes it to WatchedItemStore.resetNotificationTimestamp. In resetNotificationTimestamp, when oldid=0, the code queries for the latest revision instead of the specific revision being viewed. This causes the watchlist notification to be cleared as if the user has seen ALL revisions up to the latest, rather than just the revisions in the diff they actually viewed.",
    "fix_plan": "Modify Article.php showDiffPage() (lines 1145-1147) to check if the new revision from mapDiffPrevNext is false before calling doViewUpdates. Only call doViewUpdates if $new is a valid revision ID. Specifically: (1) Change the current code that blindly converts false to 0 with (int)$new, (2) Add a condition to only call doViewUpdates if $new is not false, or alternatively only if $new is a positive integer. This ensures that viewing a diff of old revisions only marks those specific revisions as seen, not all revisions up to the latest. The fix should be in the range: [ , $new ] = $de->mapDiffPrevNext( $oldid, $diff ); if ( $new ) { $this->mPage->doViewUpdates(...); }",
    "testing": "To verify the fix: (1) Create a page with multiple revisions (e.g., revisions 100, 105, 110, 115), (2) Watch the page as a test user, (3) Navigate to a diff view like '?oldid=100&diff=next', (4) Verify the watchlist notification is NOT completely cleared but rather only updated to reflect that revision 105 (or the actual next revision) was seen, (5) Verify that if revisions 110 and 115 exist, they still appear as unseen in the watchlist, (6) Test with explicit diff parameters like '?oldid=100&diff=105' to ensure they still work correctly, (7) Test with 'diff=cur' to ensure viewing a diff against current revision properly marks the current revision as seen"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 33.65,
    "normalized_score": 33.65,
    "normalization_delta": 0.0
  }
}
```

#### Task-Conditioned

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.4444444444444444,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.15108435051375932
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 28.87,
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
        "what_to_change": "In showDiffPage() method (around line 1145-1149), the code unconditionally calls doViewUpdates() with the newer revision from the diff. This causes the notification timestamp to be cleared (set to NULL) when the newer revision is the latest, marking ALL revisions as seen instead of only marking that specific revision viewed. The fix requires: (1) Only call doViewUpdates when the diff includes the latest revision, OR (2) Don't call doViewUpdates for diffs at all and let users explicitly mark pages as read, OR (3) Modify the logic to ensure that viewing a historical diff doesn't affect the watchlist notification state for the page."
      }
    ],
    "root_cause": "In Article.php showDiffPage() method (lines 1145-1149), the code gets the newer revision ID from the diff comparison via mapDiffPrevNext() and passes it to doViewUpdates(). This eventually calls WatchedItemStore::resetNotificationTimestamp() with that revision ID. The problem occurs in WatchedItemStore::getNotificationTimestamp() (lines 1487-1491): when the viewed revision is the latest revision of the page, the function returns null (to clear the timestamp), which marks the ENTIRE WATCHLIST ITEM as caught-up. This happens because setting wl_notificationtimestamp to NULL means the user has seen all changes, not just the specific revision viewed in the diff.",
    "fix_plan": "The core issue is that doViewUpdates() is being called with a diff revision ID in a way that causes the entire page's notification to be cleared when viewing a diff of the latest revision. There are multiple possible approaches: (1) Check if the newer revision in the diff is actually the latest revision of the page, and only call doViewUpdates() if the diff includes viewing the latest changes. (2) Remove the doViewUpdates() call from showDiffPage() entirely, since diffing revisions is a navigation action, not a viewing of the current article state. (3) Add a parameter to resetNotificationTimestamp() to indicate it's being called for a diff view, and modify getNotificationTimestamp() to avoid clearing the timestamp in that case. The most conservative fix would be approach (1): only mark the page as seen when viewing a diff that includes the current/latest revision of the page.",
    "testing": "Create a test case where: (1) User watches a page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5), (2) View a diff between revisions 1 and 2 or 3 and 4 (older revisions, not the latest), (3) Verify that the watchlist still shows the page as having unread changes (notification timestamp should not be cleared), (4) View a diff where the newer revision is the latest revision (e.g., diff between revision 4 and 5, or just view revision 5), (5) Verify that in this case, the notification timestamp IS cleared appropriately. Additionally, test edge cases: (a) viewing 'prev' diff when not at the latest, (b) viewing 'next' diff which brings you to the latest, (c) viewing a diff with 'diffonly' parameter set, (d) normal article view vs. diff view to ensure they behave differently."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 28.87,
    "normalized_score": 28.87,
    "normalization_delta": 0.0
  }
}
```

