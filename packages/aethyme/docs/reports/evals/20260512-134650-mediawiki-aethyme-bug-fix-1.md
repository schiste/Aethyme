# Eval Report: Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed. Identify which files need editing and explain how you would fix this bug.

## Meta

- Date: 2026-05-12
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
- Eval Type: bug-fix-1
- Conditions: control-cto-off, control-cto-on, explore, leverage, task-conditioned
- Aethyme Commit: `d2f51823085023279a8ab682b8195dd13ae71087`

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

- **Cost:** `-21.61%` (explore $1.1198 → leverage $1.3617)
- **Tokens:** `-23.22%` (explore 8,734,075 → leverage 10,762,185)

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 29.0 | 100.0 | 36 | $1.020 | 190.9s | 7,650,320 | 0.00 | 9.11 |
| Control (CTO on) | 44.13 | 113.31 | 37 | $1.019 | 223.2s | 7,853,426 | 0.01 | 11.86 |
| Explore | 14.57 | 84.25 | 36 | $1.120 | 182.1s | 8,734,075 | 0.00 | 4.80 |
| Leverage | 28.01 | 90.86 | 48 | $1.362 | 265.2s | 10,762,185 | 0.00 | 6.34 |
| Task-Conditioned | 31.42 | 97.1 | 36 | $1.361 | 206.5s | 10,421,390 | 0.00 | 9.13 |

## Score Breakdown

| Component | Weight | Control (CTO off) | Control (CTO on) | Explore | Leverage | Task-Conditioned |
|---|---| --- | --- | --- | --- | --- |
| Files Identified | 35% | 0.250 | 0.250 | 0.000 | 0.250 | 0.250 |
| Root Cause Quality | 25% | 0.444 | 0.889 | 0.222 | 0.333 | 0.556 |
| Fix Plan Quality | 15% | 0.000 | 0.143 | 0.000 | 0.143 | 0.000 |
| Testing Quality | 15% | 0.500 | 0.625 | 0.500 | 0.500 | 0.500 |
| Efficiency | 10% | 0.164 | 0.164 | 0.152 | 0.128 | 0.128 |

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
      "what_to_change": "Lines 1145-1149: Modify the diff view handling to conditionally call doViewUpdates based on whether the newer revision is actually being viewed. When the 'new' revision is false (line 1148), it gets converted to 0, which incorrectly marks ALL revisions as seen. The fix should either skip doViewUpdates when the newer revision doesn't exist, or only call it when the rendered page is actually being shown (not in diff-only mode)."
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "Lines 1469-1521 (getNotificationTimestamp method): Review the logic that handles the $oldid parameter. When $oldid is 0 (converted from false), it returns null which clears the notification timestamp entirely, marking ALL revisions as seen. The method may need additional logic to distinguish between 'view the current page' (which should clear the timestamp) and 'view a specific revision diff' (which should only mark that revision as seen)."
    }
  ],
  "root_cause": "In Article.php line 1147-1149, when viewing a diff, the code calls mapDiffPrevNext() to get the 'new' revision being shown in the diff. If this returns false (e.g., when following diff=prev from the latest revision), the code converts it to 0 on line 1148. This 0 value is then passed to doViewUpdates(), which eventually calls resetNotificationTimestamp() with oldid=0. In WatchedItemStore.php's resetNotificationTimestamp/getNotificationTimestamp methods, an oldid of 0 or false is treated as 'no specific revision' and the notification timestamp is set to null, which marks ALL revisions as seen instead of just the viewed revision. The core issue is that the code doesn't distinguish between viewing a diff in 'diff-only' mode (where you're not viewing the actual page content) and viewing the rendered page after a diff.",
  "fix_plan": "Two complementary fixes are needed: (1) In Article.php, check isDiffOnlyView() before calling doViewUpdates(). When viewing a diff in diff-only mode, doViewUpdates() should either not be called at all, or should be called with a flag/context indicating that this is a diff view, not a full page view. (2) Additionally, ensure that when the 'new' revision from mapDiffPrevNext() is false and gets converted to 0, this doesn't result in clearing the entire notification timestamp. The watchlist notification should only be cleared when the user has actually viewed the latest revision of the page in full-page view mode, not when just viewing a diff. The condition on line 1148-1149 should be modified to either skip doViewUpdates entirely when isDiffOnlyView() is true, or pass a different value that properly indicates 'only a diff was viewed, not the full page'.",
  "testing": "To verify the fix: (1) Add a page to your watchlist. (2) Make multiple edits to it (at least 3-4 revisions). (3) Open the watchlist and verify all revisions show as unseen. (4) Click on a diff link for a middle revision to view the diff in diff-only mode. (5) Return to the watchlist and verify that ALL revisions are NOT marked as seen - only the revisions you actually viewed (if any) should be marked as seen. (6) In diff view, if the rendered page is shown (not diff-only mode), then viewing should mark the newer revision as seen. (7) Test edge cases like diff=prev and diff=next to ensure the conversion of false to 0 doesn't cause unintended side effects."
}
```

### Control (CTO on)

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "At line 1149, the call to doViewUpdates() is missing the third parameter (RevisionRecord). Currently it passes (Authority, int $new) but should pass (Authority, int $new, ?RevisionRecord). Need to fetch the RevisionRecord for the $new revision ID before calling doViewUpdates, similar to how the normal page view at line 586 passes $this->fetchRevisionRecord()."
    }
  ],
  "root_cause": "When viewing a diff/revision, Article::showDiffPage() calls WikiPage::doViewUpdates() without passing the third parameter (RevisionRecord for the revision being viewed). This causes WatchlistManager::clearTitleUserNotifications() to receive a null RevisionRecord, which makes it default the revision ID to 0 via '$oldid = $oldRev?->getId() ?? 0'. When WatchedItemStore::resetNotificationTimestamp() is called with $oldid = 0, it treats this as 'not specified' and fetches the LATEST revision's timestamp, marking all revisions up to the latest as 'seen' instead of just the revision(s) involved in the diff being viewed.",
  "fix_plan": "1. In Article::showDiffPage() after line 1147 where mapDiffPrevNext() returns $new, fetch the RevisionRecord for that revision using $this->revisionStore->getRevisionById($new) if $new is not false/0. 2. Pass the fetched RevisionRecord as the third parameter to doViewUpdates() at line 1149. This ensures WatchlistManager::clearTitleUserNotifications() receives the correct revision record, allowing it to extract the proper revision ID and pass it to resetNotificationTimestamp(), which will then mark only the appropriate revision(s) as seen based on that specific revision's timestamp.",
  "testing": "1. Create a watchlisted page with multiple revisions. 2. View a diff between two old revisions (not the latest). 3. Verify that the watchlist shows only those two revisions as 'seen', not all revisions up to the latest. 4. Check that viewing a diff that includes the latest revision still correctly marks it as seen. 5. Verify that the wl_notificationtimestamp in the watchlist table is set to the timestamp of the appropriate revision, not the latest revision. 6. Test edge cases: viewing diff=prev, viewing diff=next, viewing a diff of the latest revision."
}
```

### Explore

```json
{
  "files_to_edit": [
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "In the getNotificationTimestamp() private method (lines 1469-1521), line 1501 uses the wrong revision timestamp. Change from using getTimestampFromId($oldid) [the viewed revision] to getTimestamp() of $nextRev [the first unseen revision]."
    }
  ],
  "root_cause": "In WatchedItemStore::getNotificationTimestamp(), when a user views a specific revision (oldid), the code retrieves that revision's timestamp and sets it as the watchlist notification timestamp. However, this marks all revisions UP TO AND INCLUDING the viewed revision as seen. The bug occurs because it should instead set the notification timestamp to the NEXT revision's timestamp (the first unseen revision), so only revisions before the viewed one are marked as seen, and the viewed revision itself plus all later ones remain visible as new updates. There's even a FIXME comment in the code (lines 1502-1503) indicating this issue.",
  "fix_plan": "1. Locate the getNotificationTimestamp() private method in includes/Watchlist/WatchedItemStore.php (starting at line 1469)\n2. At line 1501, replace:\n   $notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );\n   with:\n   $notificationTimestamp = $nextRev->getTimestamp();\n3. This ensures the notification timestamp is set to the first unseen revision's timestamp, not the currently viewed revision's timestamp\n4. The variable $nextRev is already obtained on line 1487 via getNextRevision(), so it's available at this point\n5. This matches the FIXME comment that says 'this should use getTimestamp() for consistency with updates on new edits' and uses the first unseen revision timestamp as the comment suggests",
  "testing": "1. Create a watched page with multiple revisions (revisions 1, 2, 3, 4, 5)\n2. View revision 3 directly\n3. Before fix: All 5 revisions appear as read/seen in the watchlist\n4. After fix: Revisions 1, 2, 3 should appear as read, while revisions 4 and 5 appear as new/unread updates\n5. Test edge cases: viewing the latest revision (should clear notification), viewing an old revision, viewing deleted revisions\n6. Verify that the behavior works correctly with the ActivityUpdateJob that processes the notification timestamp update in the background"
}
```

### Leverage

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (lines 1147-1149), the code incorrectly handles the case where mapDiffPrevNext returns false for the newer revision. When viewing a diff with special parameters like diff=next, the newer revision ID can be false. Converting false to 0 causes doViewUpdates to mark the LATEST revision as seen instead of the revision actually being viewed. The fix should properly determine which revision was actually viewed and pass that, along with the RevisionRecord, to doViewUpdates."
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "In the getNotificationTimestamp() method (lines 1501-1503), the code uses the timestamp of the revision being viewed ($oldid) to set the notification timestamp. However, according to the FIXME comment, it should use the timestamp of the NEXT revision (the first unseen revision). The commented line shows the correct approach: use $nextRev->getTimestamp() instead of getTimestampFromId($oldid). This ensures the watchlist correctly marks which revision is the first unseen one."
    }
  ],
  "root_cause": "The bug has two root causes: (1) In Article.php showDiffPage(), the code fails to properly handle diff parameters that resolve to no newer revision (returning false), which gets converted to 0 meaning 'latest revision'. This causes the watchlist system to mark all revisions up to the latest as seen, even if the user only viewed an older revision. (2) In WatchedItemStore.php getNotificationTimestamp(), the notification timestamp is set to the timestamp of the revision being viewed, but it should be set to the timestamp of the next revision (first unseen). This prevents proper tracking of which revisions have been seen.",
  "fix_plan": "1. In includes/Page/Article.php showDiffPage() method (lines 1145-1153): Modify the code to handle the case where mapDiffPrevNext returns false or 0 for the newer revision. Instead of blindly converting false to 0, the code should determine the actual revision being viewed (likely the older revision in the diff when diff=next) and use that with the proper RevisionRecord. The fix should call doViewUpdates with the revision that was actually displayed to the user, not necessarily the newer one in a comparison. 2. In includes/Watchlist/WatchedItemStore.php getNotificationTimestamp() method (lines 1499-1521): Replace line 1501 to use $nextRev->getTimestamp() instead of $this->revisionLookup->getTimestampFromId($oldid). This aligns with the existing FIXME comment and ensures consistency with how notification timestamps are set when new edits occur. The timestamp should mark the point AFTER what the user has seen, not the exact revision they viewed.",
  "testing": "Test cases should cover: (1) Viewing a regular diff with oldid and diff parameters both numeric (e.g., ?oldid=2&diff=4) - should mark revision 4 as seen but not later revisions. (2) Viewing a diff with diff=next on a revision that is not the latest (e.g., ?oldid=4&diff=next when revision 6 exists) - should mark revision 4 as seen, not revision 6. (3) Viewing a diff with diff=next on the latest revision (e.g., ?oldid=4&diff=next when 4 is latest) - should mark all as seen correctly. (4) Viewing a single revision page to ensure normal revision viewing still works correctly. Verify that the watchlist indicator correctly shows whether there are unseen revisions after viewing each type of diff or revision page."
}
```

### Task-Conditioned

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In showDiffPage() method (around line 1145-1149), the logic for marking revisions as seen when viewing a diff is incorrect. The code currently passes the newer revision ID from the diff comparison to doViewUpdates(), but when viewing a specific revision in diff/revision context, only that specific viewed revision should be marked as seen, not all revisions up to the newer one in the diff."
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "The resetNotificationTimestamp() method (around line 1334-1422) needs to handle the case where viewing a specific revision in diff context should only mark that revision (or revisions between two specific points) as seen, not all subsequent revisions. The current implementation marks everything up to a given timestamp as seen, which doesn't distinguish between 'viewed this revision' and 'viewed a range of revisions'."
    }
  ],
  "root_cause": "When a user views a revision through a diff page (with ?oldid=X&diff=Y parameters), the showDiffPage() method in Article.php calls doViewUpdates() with the 'new' (right-side/newer) revision ID from the diff mapping. This causes the watchlist notification timestamp to be set to the timestamp of the newer revision, marking ALL older revisions as 'seen' rather than only marking the specific revision(s) that were actually viewed. The issue is that mapDiffPrevNext() can return false for the new revision in certain edge cases, which then gets converted to 0 (meaning 'latest revision'), causing all revisions to be marked as seen.",
  "fix_plan": "1. In Article.php showDiffPage() (line ~1147), identify whether the user is viewing a specific revision for the first time or comparing two revisions. 2. When viewing a specific revision via diff interface, pass the oldid (the revision being explicitly viewed) to doViewUpdates() instead of the computed 'new' revision from mapDiffPrevNext(). 3. Alternatively, modify resetNotificationTimestamp() in WatchedItemStore.php to accept additional context about whether we're viewing a specific revision vs. viewing the latest version, and only mark the appropriate revisions as seen. 4. Add safeguards to ensure that when $new is false (no next revision exists), we don't default to 0 which marks everything as seen, but instead use the oldid that was explicitly requested.",
  "testing": "1. Create a test page with multiple revisions (rev 1, 2, 3, 4, 5). 2. Add the page to your watchlist. 3. Manually view revision 3 using a revision/diff interface (?oldid=3 or ?oldid=2&diff=3). 4. Check the watchlist - revision 3 should be marked as seen, but revisions 4 and 5 should still show as unseen. 5. View revision 5 and verify all revisions are marked as seen. 6. Test edge cases: view a diff with ?oldid=1&diff=prev (should handle gracefully when prev doesn't exist), ?oldid=5&diff=cur, etc. 7. Verify that normal page viewing (without diff parameter) still marks only up to the viewed revision as seen."
}
```

## Tool Call Analysis

### Control (CTO off)

Total tool calls: 36

Top tools: `?` x36

| Tool | Count |
|---|---|
| `?` | 36 |

### Control (CTO on)

Total tool calls: 37

Top tools: `?` x37

| Tool | Count |
|---|---|
| `?` | 37 |

### Explore

Total tool calls: 36

Top tools: `?` x36

| Tool | Count |
|---|---|
| `?` | 36 |

### Leverage

Total tool calls: 48

Top tools: `?` x48

| Tool | Count |
|---|---|
| `?` | 48 |

### Task-Conditioned

Total tool calls: 36

Top tools: `?` x36

| Tool | Count |
|---|---|
| `?` | 36 |

## Context Pack Audit

- Navigation items: 0
- Risk items: 0

## Graph Quality Notes

N/A

## Prompt Effectiveness

### Control (CTO off)

- Prompt chars: 945
- Input tokens: 614
- Output tokens: 27357

### Control (CTO on)

- Prompt chars: 943
- Input tokens: 638
- Output tokens: 24917

### Explore

- Prompt chars: 929
- Input tokens: 561
- Output tokens: 22595

### Leverage

- Prompt chars: 1170
- Input tokens: 822
- Output tokens: 45234

### Task-Conditioned

- Prompt chars: 1069
- Input tokens: 610
- Output tokens: 33856

## Lessons & Action Items

- Keep context packs concise and explicitly prioritize high-confidence nodes.
- Capture parser visibility deltas across conditions to track graph quality drift.

## Verdict

**Control (CTO on)** scored highest (44.13/100), **Explore** lowest (14.57/100). Best overall value versus the control baseline: **Control (CTO on)** (113.31 recalculated eval score). Most efficient: Control (CTO on) ($1.019), most expensive: Leverage ($1.362). All conditions passed tests.

## Notes

Sample #2 of multi-sample variance study (3 runs).

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
  "input_tokens": 614,
  "output_tokens": 27357,
  "cache_read_tokens": 7296393,
  "cache_create_tokens": 325956,
  "cost_usd": 1.01958664,
  "duration_seconds": 190.95,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Lines 1145-1149: Modify the diff view handling to conditionally call doViewUpdates based on whether the newer revision is actually being viewed. When the 'new' revision is false (line 1148), it gets converted to 0, which incorrectly marks ALL revisions as seen. The fix should either skip doViewUpdates when the newer revision doesn't exist, or only call it when the rendered page is actually being shown (not in diff-only mode)."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Lines 1469-1521 (getNotificationTimestamp method): Review the logic that handles the $oldid parameter. When $oldid is 0 (converted from false), it returns null which clears the notification timestamp entirely, marking ALL revisions as seen. The method may need additional logic to distinguish between 'view the current page' (which should clear the timestamp) and 'view a specific revision diff' (which should only mark that revision as seen)."
      }
    ],
    "root_cause": "In Article.php line 1147-1149, when viewing a diff, the code calls mapDiffPrevNext() to get the 'new' revision being shown in the diff. If this returns false (e.g., when following diff=prev from the latest revision), the code converts it to 0 on line 1148. This 0 value is then passed to doViewUpdates(), which eventually calls resetNotificationTimestamp() with oldid=0. In WatchedItemStore.php's resetNotificationTimestamp/getNotificationTimestamp methods, an oldid of 0 or false is treated as 'no specific revision' and the notification timestamp is set to null, which marks ALL revisions as seen instead of just the viewed revision. The core issue is that the code doesn't distinguish between viewing a diff in 'diff-only' mode (where you're not viewing the actual page content) and viewing the rendered page after a diff.",
    "fix_plan": "Two complementary fixes are needed: (1) In Article.php, check isDiffOnlyView() before calling doViewUpdates(). When viewing a diff in diff-only mode, doViewUpdates() should either not be called at all, or should be called with a flag/context indicating that this is a diff view, not a full page view. (2) Additionally, ensure that when the 'new' revision from mapDiffPrevNext() is false and gets converted to 0, this doesn't result in clearing the entire notification timestamp. The watchlist notification should only be cleared when the user has actually viewed the latest revision of the page in full-page view mode, not when just viewing a diff. The condition on line 1148-1149 should be modified to either skip doViewUpdates entirely when isDiffOnlyView() is true, or pass a different value that properly indicates 'only a diff was viewed, not the full page'.",
    "testing": "To verify the fix: (1) Add a page to your watchlist. (2) Make multiple edits to it (at least 3-4 revisions). (3) Open the watchlist and verify all revisions show as unseen. (4) Click on a diff link for a middle revision to view the diff in diff-only mode. (5) Return to the watchlist and verify that ALL revisions are NOT marked as seen - only the revisions you actually viewed (if any) should be marked as seen. (6) In diff view, if the rendered page is shown (not diff-only mode), then viewing should mark the newer revision as seen. (7) Test edge cases like diff=prev and diff=next to ensure the conversion of false to 0 doesn't cause unintended side effects."
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

#### Control (CTO on)

```json
{
  "input_tokens": 638,
  "output_tokens": 24917,
  "cache_read_tokens": 7510283,
  "cache_create_tokens": 317588,
  "cost_usd": 1.01858904,
  "duration_seconds": 223.212,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "At line 1149, the call to doViewUpdates() is missing the third parameter (RevisionRecord). Currently it passes (Authority, int $new) but should pass (Authority, int $new, ?RevisionRecord). Need to fetch the RevisionRecord for the $new revision ID before calling doViewUpdates, similar to how the normal page view at line 586 passes $this->fetchRevisionRecord()."
      }
    ],
    "root_cause": "When viewing a diff/revision, Article::showDiffPage() calls WikiPage::doViewUpdates() without passing the third parameter (RevisionRecord for the revision being viewed). This causes WatchlistManager::clearTitleUserNotifications() to receive a null RevisionRecord, which makes it default the revision ID to 0 via '$oldid = $oldRev?->getId() ?? 0'. When WatchedItemStore::resetNotificationTimestamp() is called with $oldid = 0, it treats this as 'not specified' and fetches the LATEST revision's timestamp, marking all revisions up to the latest as 'seen' instead of just the revision(s) involved in the diff being viewed.",
    "fix_plan": "1. In Article::showDiffPage() after line 1147 where mapDiffPrevNext() returns $new, fetch the RevisionRecord for that revision using $this->revisionStore->getRevisionById($new) if $new is not false/0. 2. Pass the fetched RevisionRecord as the third parameter to doViewUpdates() at line 1149. This ensures WatchlistManager::clearTitleUserNotifications() receives the correct revision record, allowing it to extract the proper revision ID and pass it to resetNotificationTimestamp(), which will then mark only the appropriate revision(s) as seen based on that specific revision's timestamp.",
    "testing": "1. Create a watchlisted page with multiple revisions. 2. View a diff between two old revisions (not the latest). 3. Verify that the watchlist shows only those two revisions as 'seen', not all revisions up to the latest. 4. Check that viewing a diff that includes the latest revision still correctly marks it as seen. 5. Verify that the wl_notificationtimestamp in the watchlist table is set to the timestamp of the appropriate revision, not the latest revision. 6. Test edge cases: viewing diff=prev, viewing diff=next, viewing a diff of the latest revision."
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

#### Explore

```json
{
  "input_tokens": 561,
  "output_tokens": 22595,
  "cache_read_tokens": 8349992,
  "cache_create_tokens": 360927,
  "cost_usd": 1.11975516,
  "duration_seconds": 182.108,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the getNotificationTimestamp() private method (lines 1469-1521), line 1501 uses the wrong revision timestamp. Change from using getTimestampFromId($oldid) [the viewed revision] to getTimestamp() of $nextRev [the first unseen revision]."
      }
    ],
    "root_cause": "In WatchedItemStore::getNotificationTimestamp(), when a user views a specific revision (oldid), the code retrieves that revision's timestamp and sets it as the watchlist notification timestamp. However, this marks all revisions UP TO AND INCLUDING the viewed revision as seen. The bug occurs because it should instead set the notification timestamp to the NEXT revision's timestamp (the first unseen revision), so only revisions before the viewed one are marked as seen, and the viewed revision itself plus all later ones remain visible as new updates. There's even a FIXME comment in the code (lines 1502-1503) indicating this issue.",
    "fix_plan": "1. Locate the getNotificationTimestamp() private method in includes/Watchlist/WatchedItemStore.php (starting at line 1469)\n2. At line 1501, replace:\n   $notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );\n   with:\n   $notificationTimestamp = $nextRev->getTimestamp();\n3. This ensures the notification timestamp is set to the first unseen revision's timestamp, not the currently viewed revision's timestamp\n4. The variable $nextRev is already obtained on line 1487 via getNextRevision(), so it's available at this point\n5. This matches the FIXME comment that says 'this should use getTimestamp() for consistency with updates on new edits' and uses the first unseen revision timestamp as the comment suggests",
    "testing": "1. Create a watched page with multiple revisions (revisions 1, 2, 3, 4, 5)\n2. View revision 3 directly\n3. Before fix: All 5 revisions appear as read/seen in the watchlist\n4. After fix: Revisions 1, 2, 3 should appear as read, while revisions 4 and 5 appear as new/unread updates\n5. Test edge cases: viewing the latest revision (should clear notification), viewing an old revision, viewing deleted revisions\n6. Verify that the behavior works correctly with the ActivityUpdateJob that processes the notification timestamp update in the background"
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

#### Leverage

```json
{
  "input_tokens": 822,
  "output_tokens": 45234,
  "cache_read_tokens": 10365242,
  "cache_create_tokens": 350887,
  "cost_usd": 1.36169996,
  "duration_seconds": 265.241,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (lines 1147-1149), the code incorrectly handles the case where mapDiffPrevNext returns false for the newer revision. When viewing a diff with special parameters like diff=next, the newer revision ID can be false. Converting false to 0 causes doViewUpdates to mark the LATEST revision as seen instead of the revision actually being viewed. The fix should properly determine which revision was actually viewed and pass that, along with the RevisionRecord, to doViewUpdates."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the getNotificationTimestamp() method (lines 1501-1503), the code uses the timestamp of the revision being viewed ($oldid) to set the notification timestamp. However, according to the FIXME comment, it should use the timestamp of the NEXT revision (the first unseen revision). The commented line shows the correct approach: use $nextRev->getTimestamp() instead of getTimestampFromId($oldid). This ensures the watchlist correctly marks which revision is the first unseen one."
      }
    ],
    "root_cause": "The bug has two root causes: (1) In Article.php showDiffPage(), the code fails to properly handle diff parameters that resolve to no newer revision (returning false), which gets converted to 0 meaning 'latest revision'. This causes the watchlist system to mark all revisions up to the latest as seen, even if the user only viewed an older revision. (2) In WatchedItemStore.php getNotificationTimestamp(), the notification timestamp is set to the timestamp of the revision being viewed, but it should be set to the timestamp of the next revision (first unseen). This prevents proper tracking of which revisions have been seen.",
    "fix_plan": "1. In includes/Page/Article.php showDiffPage() method (lines 1145-1153): Modify the code to handle the case where mapDiffPrevNext returns false or 0 for the newer revision. Instead of blindly converting false to 0, the code should determine the actual revision being viewed (likely the older revision in the diff when diff=next) and use that with the proper RevisionRecord. The fix should call doViewUpdates with the revision that was actually displayed to the user, not necessarily the newer one in a comparison. 2. In includes/Watchlist/WatchedItemStore.php getNotificationTimestamp() method (lines 1499-1521): Replace line 1501 to use $nextRev->getTimestamp() instead of $this->revisionLookup->getTimestampFromId($oldid). This aligns with the existing FIXME comment and ensures consistency with how notification timestamps are set when new edits occur. The timestamp should mark the point AFTER what the user has seen, not the exact revision they viewed.",
    "testing": "Test cases should cover: (1) Viewing a regular diff with oldid and diff parameters both numeric (e.g., ?oldid=2&diff=4) - should mark revision 4 as seen but not later revisions. (2) Viewing a diff with diff=next on a revision that is not the latest (e.g., ?oldid=4&diff=next when revision 6 exists) - should mark revision 4 as seen, not revision 6. (3) Viewing a diff with diff=next on the latest revision (e.g., ?oldid=4&diff=next when 4 is latest) - should mark all as seen correctly. (4) Viewing a single revision page to ensure normal revision viewing still works correctly. Verify that the watchlist indicator correctly shows whether there are unseen revisions after viewing each type of diff or revision page."
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
    },
    {
      "name": "?"
    },
    {
      "name": "?"
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
  "input_tokens": 610,
  "output_tokens": 33856,
  "cache_read_tokens": 9958795,
  "cache_create_tokens": 428129,
  "cost_usd": 1.3607446,
  "duration_seconds": 206.466,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In showDiffPage() method (around line 1145-1149), the logic for marking revisions as seen when viewing a diff is incorrect. The code currently passes the newer revision ID from the diff comparison to doViewUpdates(), but when viewing a specific revision in diff/revision context, only that specific viewed revision should be marked as seen, not all revisions up to the newer one in the diff."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The resetNotificationTimestamp() method (around line 1334-1422) needs to handle the case where viewing a specific revision in diff context should only mark that revision (or revisions between two specific points) as seen, not all subsequent revisions. The current implementation marks everything up to a given timestamp as seen, which doesn't distinguish between 'viewed this revision' and 'viewed a range of revisions'."
      }
    ],
    "root_cause": "When a user views a revision through a diff page (with ?oldid=X&diff=Y parameters), the showDiffPage() method in Article.php calls doViewUpdates() with the 'new' (right-side/newer) revision ID from the diff mapping. This causes the watchlist notification timestamp to be set to the timestamp of the newer revision, marking ALL older revisions as 'seen' rather than only marking the specific revision(s) that were actually viewed. The issue is that mapDiffPrevNext() can return false for the new revision in certain edge cases, which then gets converted to 0 (meaning 'latest revision'), causing all revisions to be marked as seen.",
    "fix_plan": "1. In Article.php showDiffPage() (line ~1147), identify whether the user is viewing a specific revision for the first time or comparing two revisions. 2. When viewing a specific revision via diff interface, pass the oldid (the revision being explicitly viewed) to doViewUpdates() instead of the computed 'new' revision from mapDiffPrevNext(). 3. Alternatively, modify resetNotificationTimestamp() in WatchedItemStore.php to accept additional context about whether we're viewing a specific revision vs. viewing the latest version, and only mark the appropriate revisions as seen. 4. Add safeguards to ensure that when $new is false (no next revision exists), we don't default to 0 which marks everything as seen, but instead use the oldid that was explicitly requested.",
    "testing": "1. Create a test page with multiple revisions (rev 1, 2, 3, 4, 5). 2. Add the page to your watchlist. 3. Manually view revision 3 using a revision/diff interface (?oldid=3 or ?oldid=2&diff=3). 4. Check the watchlist - revision 3 should be marked as seen, but revisions 4 and 5 should still show as unseen. 5. View revision 5 and verify all revisions are marked as seen. 6. Test edge cases: view a diff with ?oldid=1&diff=prev (should handle gracefully when prev doesn't exist), ?oldid=5&diff=cur, etc. 7. Verify that normal page viewing (without diff parameter) still marks only up to the viewed revision as seen."
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

### Per-Condition Assessments

#### Control (CTO off)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.4444444444444444,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.1639899892639034
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 29.0,
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
        "what_to_change": "Lines 1145-1149: Modify the diff view handling to conditionally call doViewUpdates based on whether the newer revision is actually being viewed. When the 'new' revision is false (line 1148), it gets converted to 0, which incorrectly marks ALL revisions as seen. The fix should either skip doViewUpdates when the newer revision doesn't exist, or only call it when the rendered page is actually being shown (not in diff-only mode)."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "Lines 1469-1521 (getNotificationTimestamp method): Review the logic that handles the $oldid parameter. When $oldid is 0 (converted from false), it returns null which clears the notification timestamp entirely, marking ALL revisions as seen. The method may need additional logic to distinguish between 'view the current page' (which should clear the timestamp) and 'view a specific revision diff' (which should only mark that revision as seen)."
      }
    ],
    "root_cause": "In Article.php line 1147-1149, when viewing a diff, the code calls mapDiffPrevNext() to get the 'new' revision being shown in the diff. If this returns false (e.g., when following diff=prev from the latest revision), the code converts it to 0 on line 1148. This 0 value is then passed to doViewUpdates(), which eventually calls resetNotificationTimestamp() with oldid=0. In WatchedItemStore.php's resetNotificationTimestamp/getNotificationTimestamp methods, an oldid of 0 or false is treated as 'no specific revision' and the notification timestamp is set to null, which marks ALL revisions as seen instead of just the viewed revision. The core issue is that the code doesn't distinguish between viewing a diff in 'diff-only' mode (where you're not viewing the actual page content) and viewing the rendered page after a diff.",
    "fix_plan": "Two complementary fixes are needed: (1) In Article.php, check isDiffOnlyView() before calling doViewUpdates(). When viewing a diff in diff-only mode, doViewUpdates() should either not be called at all, or should be called with a flag/context indicating that this is a diff view, not a full page view. (2) Additionally, ensure that when the 'new' revision from mapDiffPrevNext() is false and gets converted to 0, this doesn't result in clearing the entire notification timestamp. The watchlist notification should only be cleared when the user has actually viewed the latest revision of the page in full-page view mode, not when just viewing a diff. The condition on line 1148-1149 should be modified to either skip doViewUpdates entirely when isDiffOnlyView() is true, or pass a different value that properly indicates 'only a diff was viewed, not the full page'.",
    "testing": "To verify the fix: (1) Add a page to your watchlist. (2) Make multiple edits to it (at least 3-4 revisions). (3) Open the watchlist and verify all revisions show as unseen. (4) Click on a diff link for a middle revision to view the diff in diff-only mode. (5) Return to the watchlist and verify that ALL revisions are NOT marked as seen - only the revisions you actually viewed (if any) should be marked as seen. (6) In diff view, if the rendered page is shown (not diff-only mode), then viewing should mark the newer revision as seen. (7) Test edge cases like diff=prev and diff=next to ensure the conversion of false to 0 doesn't cause unintended side effects."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 29.0,
    "normalized_score": 29.0,
    "normalization_delta": 0.0
  }
}
```

#### Control (CTO on)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.8888888888888888,
    "fix_plan_quality": 0.14285714285714285,
    "testing_quality": 0.625,
    "efficiency": 0.16412423994885103
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 44.13,
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
        "what_to_change": "At line 1149, the call to doViewUpdates() is missing the third parameter (RevisionRecord). Currently it passes (Authority, int $new) but should pass (Authority, int $new, ?RevisionRecord). Need to fetch the RevisionRecord for the $new revision ID before calling doViewUpdates, similar to how the normal page view at line 586 passes $this->fetchRevisionRecord()."
      }
    ],
    "root_cause": "When viewing a diff/revision, Article::showDiffPage() calls WikiPage::doViewUpdates() without passing the third parameter (RevisionRecord for the revision being viewed). This causes WatchlistManager::clearTitleUserNotifications() to receive a null RevisionRecord, which makes it default the revision ID to 0 via '$oldid = $oldRev?->getId() ?? 0'. When WatchedItemStore::resetNotificationTimestamp() is called with $oldid = 0, it treats this as 'not specified' and fetches the LATEST revision's timestamp, marking all revisions up to the latest as 'seen' instead of just the revision(s) involved in the diff being viewed.",
    "fix_plan": "1. In Article::showDiffPage() after line 1147 where mapDiffPrevNext() returns $new, fetch the RevisionRecord for that revision using $this->revisionStore->getRevisionById($new) if $new is not false/0. 2. Pass the fetched RevisionRecord as the third parameter to doViewUpdates() at line 1149. This ensures WatchlistManager::clearTitleUserNotifications() receives the correct revision record, allowing it to extract the proper revision ID and pass it to resetNotificationTimestamp(), which will then mark only the appropriate revision(s) as seen based on that specific revision's timestamp.",
    "testing": "1. Create a watchlisted page with multiple revisions. 2. View a diff between two old revisions (not the latest). 3. Verify that the watchlist shows only those two revisions as 'seen', not all revisions up to the latest. 4. Check that viewing a diff that includes the latest revision still correctly marks it as seen. 5. Verify that the wl_notificationtimestamp in the watchlist table is set to the timestamp of the appropriate revision, not the latest revision. 6. Test edge cases: viewing diff=prev, viewing diff=next, viewing a diff of the latest revision."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 44.13,
    "normalized_score": 44.13,
    "normalization_delta": 0.0
  }
}
```

#### Explore

```json
{
  "scores": {
    "files_identified": 0.0,
    "root_cause_quality": 0.2222222222222222,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.15154326049386313
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 14.57,
  "max_score": 100,
  "files_matched": [],
  "files_missed": [
    "RELEASE-NOTES-1.46",
    "includes/Page/Article.php",
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
        "what_to_change": "In the getNotificationTimestamp() private method (lines 1469-1521), line 1501 uses the wrong revision timestamp. Change from using getTimestampFromId($oldid) [the viewed revision] to getTimestamp() of $nextRev [the first unseen revision]."
      }
    ],
    "root_cause": "In WatchedItemStore::getNotificationTimestamp(), when a user views a specific revision (oldid), the code retrieves that revision's timestamp and sets it as the watchlist notification timestamp. However, this marks all revisions UP TO AND INCLUDING the viewed revision as seen. The bug occurs because it should instead set the notification timestamp to the NEXT revision's timestamp (the first unseen revision), so only revisions before the viewed one are marked as seen, and the viewed revision itself plus all later ones remain visible as new updates. There's even a FIXME comment in the code (lines 1502-1503) indicating this issue.",
    "fix_plan": "1. Locate the getNotificationTimestamp() private method in includes/Watchlist/WatchedItemStore.php (starting at line 1469)\n2. At line 1501, replace:\n   $notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );\n   with:\n   $notificationTimestamp = $nextRev->getTimestamp();\n3. This ensures the notification timestamp is set to the first unseen revision's timestamp, not the currently viewed revision's timestamp\n4. The variable $nextRev is already obtained on line 1487 via getNextRevision(), so it's available at this point\n5. This matches the FIXME comment that says 'this should use getTimestamp() for consistency with updates on new edits' and uses the first unseen revision timestamp as the comment suggests",
    "testing": "1. Create a watched page with multiple revisions (revisions 1, 2, 3, 4, 5)\n2. View revision 3 directly\n3. Before fix: All 5 revisions appear as read/seen in the watchlist\n4. After fix: Revisions 1, 2, 3 should appear as read, while revisions 4 and 5 appear as new/unread updates\n5. Test edge cases: viewing the latest revision (should clear notification), viewing an old revision, viewing deleted revisions\n6. Verify that the behavior works correctly with the ActivityUpdateJob that processes the notification timestamp update in the background"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 14.57,
    "normalized_score": 14.57,
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
    "fix_plan_quality": 0.14285714285714285,
    "testing_quality": 0.5,
    "efficiency": 0.1280655728517788
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 28.01,
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
        "what_to_change": "In the showDiffPage() method (lines 1147-1149), the code incorrectly handles the case where mapDiffPrevNext returns false for the newer revision. When viewing a diff with special parameters like diff=next, the newer revision ID can be false. Converting false to 0 causes doViewUpdates to mark the LATEST revision as seen instead of the revision actually being viewed. The fix should properly determine which revision was actually viewed and pass that, along with the RevisionRecord, to doViewUpdates."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the getNotificationTimestamp() method (lines 1501-1503), the code uses the timestamp of the revision being viewed ($oldid) to set the notification timestamp. However, according to the FIXME comment, it should use the timestamp of the NEXT revision (the first unseen revision). The commented line shows the correct approach: use $nextRev->getTimestamp() instead of getTimestampFromId($oldid). This ensures the watchlist correctly marks which revision is the first unseen one."
      }
    ],
    "root_cause": "The bug has two root causes: (1) In Article.php showDiffPage(), the code fails to properly handle diff parameters that resolve to no newer revision (returning false), which gets converted to 0 meaning 'latest revision'. This causes the watchlist system to mark all revisions up to the latest as seen, even if the user only viewed an older revision. (2) In WatchedItemStore.php getNotificationTimestamp(), the notification timestamp is set to the timestamp of the revision being viewed, but it should be set to the timestamp of the next revision (first unseen). This prevents proper tracking of which revisions have been seen.",
    "fix_plan": "1. In includes/Page/Article.php showDiffPage() method (lines 1145-1153): Modify the code to handle the case where mapDiffPrevNext returns false or 0 for the newer revision. Instead of blindly converting false to 0, the code should determine the actual revision being viewed (likely the older revision in the diff when diff=next) and use that with the proper RevisionRecord. The fix should call doViewUpdates with the revision that was actually displayed to the user, not necessarily the newer one in a comparison. 2. In includes/Watchlist/WatchedItemStore.php getNotificationTimestamp() method (lines 1499-1521): Replace line 1501 to use $nextRev->getTimestamp() instead of $this->revisionLookup->getTimestampFromId($oldid). This aligns with the existing FIXME comment and ensures consistency with how notification timestamps are set when new edits occur. The timestamp should mark the point AFTER what the user has seen, not the exact revision they viewed.",
    "testing": "Test cases should cover: (1) Viewing a regular diff with oldid and diff parameters both numeric (e.g., ?oldid=2&diff=4) - should mark revision 4 as seen but not later revisions. (2) Viewing a diff with diff=next on a revision that is not the latest (e.g., ?oldid=4&diff=next when revision 6 exists) - should mark revision 4 as seen, not revision 6. (3) Viewing a diff with diff=next on the latest revision (e.g., ?oldid=4&diff=next when 4 is latest) - should mark all as seen correctly. (4) Viewing a single revision page to ensure normal revision viewing still works correctly. Verify that the watchlist indicator correctly shows whether there are unseen revisions after viewing each type of diff or revision page."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 28.01,
    "normalized_score": 28.01,
    "normalization_delta": 0.0
  }
}
```

#### Task-Conditioned

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.5555555555555556,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.12814396410533793
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 31.42,
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
        "what_to_change": "In showDiffPage() method (around line 1145-1149), the logic for marking revisions as seen when viewing a diff is incorrect. The code currently passes the newer revision ID from the diff comparison to doViewUpdates(), but when viewing a specific revision in diff/revision context, only that specific viewed revision should be marked as seen, not all revisions up to the newer one in the diff."
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The resetNotificationTimestamp() method (around line 1334-1422) needs to handle the case where viewing a specific revision in diff context should only mark that revision (or revisions between two specific points) as seen, not all subsequent revisions. The current implementation marks everything up to a given timestamp as seen, which doesn't distinguish between 'viewed this revision' and 'viewed a range of revisions'."
      }
    ],
    "root_cause": "When a user views a revision through a diff page (with ?oldid=X&diff=Y parameters), the showDiffPage() method in Article.php calls doViewUpdates() with the 'new' (right-side/newer) revision ID from the diff mapping. This causes the watchlist notification timestamp to be set to the timestamp of the newer revision, marking ALL older revisions as 'seen' rather than only marking the specific revision(s) that were actually viewed. The issue is that mapDiffPrevNext() can return false for the new revision in certain edge cases, which then gets converted to 0 (meaning 'latest revision'), causing all revisions to be marked as seen.",
    "fix_plan": "1. In Article.php showDiffPage() (line ~1147), identify whether the user is viewing a specific revision for the first time or comparing two revisions. 2. When viewing a specific revision via diff interface, pass the oldid (the revision being explicitly viewed) to doViewUpdates() instead of the computed 'new' revision from mapDiffPrevNext(). 3. Alternatively, modify resetNotificationTimestamp() in WatchedItemStore.php to accept additional context about whether we're viewing a specific revision vs. viewing the latest version, and only mark the appropriate revisions as seen. 4. Add safeguards to ensure that when $new is false (no next revision exists), we don't default to 0 which marks everything as seen, but instead use the oldid that was explicitly requested.",
    "testing": "1. Create a test page with multiple revisions (rev 1, 2, 3, 4, 5). 2. Add the page to your watchlist. 3. Manually view revision 3 using a revision/diff interface (?oldid=3 or ?oldid=2&diff=3). 4. Check the watchlist - revision 3 should be marked as seen, but revisions 4 and 5 should still show as unseen. 5. View revision 5 and verify all revisions are marked as seen. 6. Test edge cases: view a diff with ?oldid=1&diff=prev (should handle gracefully when prev doesn't exist), ?oldid=5&diff=cur, etc. 7. Verify that normal page viewing (without diff parameter) still marks only up to the viewed revision as seen."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 31.42,
    "normalized_score": 31.42,
    "normalization_delta": 0.0
  }
}
```

