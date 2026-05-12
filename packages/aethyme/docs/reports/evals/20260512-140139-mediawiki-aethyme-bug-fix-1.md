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

- **Cost:** `+6.88%` (explore $0.7890 → leverage $0.7347)
- **Tokens:** `+0.18%` (explore 7,447,059 → leverage 7,433,290)

## Scorecard

| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |
|---|---|---|---|---|---|---|---|---|
| Control (CTO off) | 38.64 | 100.0 | 48 | $1.194 | 361.3s | 10,040,889 | 0.00 | 6.42 |
| Control (CTO on) | 37.14 | 117.34 | 21 | $0.652 | 137.4s | 5,428,720 | 0.01 | 16.22 |
| Explore | 32.16 | 104.7 | 32 | $0.789 | 196.0s | 7,447,059 | 0.00 | 9.85 |
| Leverage | 17.07 | 91.84 | 35 | $0.735 | 162.7s | 7,433,290 | 0.00 | 6.29 |
| Task-Conditioned | 46.96 | 116.2 | 38 | $0.981 | 195.4s | 9,309,151 | 0.01 | 14.42 |

## Score Breakdown

| Component | Weight | Control (CTO off) | Control (CTO on) | Explore | Leverage | Task-Conditioned |
|---|---| --- | --- | --- | --- | --- |
| Files Identified | 35% | 0.250 | 0.250 | 0.250 | 0.000 | 0.250 |
| Root Cause Quality | 25% | 0.667 | 0.667 | 0.556 | 0.222 | 1.000 |
| Fix Plan Quality | 15% | 0.286 | 0.000 | 0.000 | 0.000 | 0.143 |
| Testing Quality | 15% | 0.500 | 0.625 | 0.500 | 0.625 | 0.625 |
| Efficiency | 10% | 0.143 | 0.235 | 0.202 | 0.214 | 0.169 |

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
      "what_to_change": "Line 1149: When calling doViewUpdates() in the diff view code, pass the RevisionRecord for the revision being viewed ($new) as the third parameter, not just the oldid"
    }
  ],
  "root_cause": "When viewing a diff, the code at Article.php:1149 calls doViewUpdates() with only two arguments: $this->mPage->doViewUpdates( $context->getAuthority(), (int)$new ). This passes the newid as the oldid parameter but does not pass a RevisionRecord as the third argument. When doViewUpdates() then calls clearTitleUserNotifications() with $oldRev = null, the clearTitleUserNotifications() method interprets this as 3 arguments being passed and executes: $oldid = $oldRev?->getId() ?? 0, which results in $oldid becoming 0. This causes resetNotificationTimestamp() to treat it as \"no specific revision provided\" and instead fetches and uses the LATEST revision's timestamp. This marks the page as \"seen\" up to the latest revision, making all older/earlier revisions appear as \"seen\" in the watchlist.",
  "fix_plan": "Modify line 1149 in Article.php to fetch the RevisionRecord for revision $new and pass it as the third parameter to doViewUpdates(). The fix should: (1) Fetch the RevisionRecord for the revision being viewed (the $new revision from mapDiffPrevNext), (2) Pass this RevisionRecord as the third argument to doViewUpdates(). This ensures that when clearTitleUserNotifications() is called with the correct $oldRev, it will properly extract the revision ID from it and mark only the specific viewed revision as seen, not all revisions up to the latest. The pattern should match how it's done on line 586 where displaying a normal page view passes $this->fetchRevisionRecord().",
  "testing": "To test this fix: (1) Create a watchlist entry for a page with multiple revisions, (2) View a diff between two specific revisions (e.g., revision 1 vs revision 2 on a page with 5 revisions), (3) Check the watchlist - only the revision being viewed should be marked as seen, not all revisions, (4) Verify that other revisions still show as new/unseen in the watchlist, (5) Test multiple scenarios: viewing recent diffs, old diffs, comparing non-consecutive revisions to ensure the fix works correctly in all cases"
}
```

### Control (CTO on)

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (around line 1149), modify the call to doViewUpdates() to only mark revisions as seen when viewing a diff that includes the current/latest revision. Currently it unconditionally marks the newer diff revision as seen, causing all intermediate revisions to appear as 'seen' in the watchlist."
    },
    {
      "path": "includes/Watchlist/WatchlistManager.php",
      "what_to_change": "Potentially add a parameter or logic to clearTitleUserNotifications() to handle the specific case of viewing a historical diff (where neither revision being compared is the current one), preventing the premature marking of unseen revisions as 'seen'."
    }
  ],
  "root_cause": "The bug occurs in Article::showDiffPage() at line 1149, which calls $this->mPage->doViewUpdates() with the newer revision ID from the diff. This triggers WatchlistManager::clearTitleUserNotifications(), which sets the watchlist notification_timestamp to mark revisions as 'seen'. However, the watchlist system uses a single timestamp per page to track which revisions are unseen. When you view a diff of two revisions (e.g., revision 3 vs revision 5), the code sets the notification_timestamp based on revision 5, which marks ALL revisions with timestamps earlier than the next revision's timestamp as 'seen'. This includes intermediate revisions (like revision 4) that the user never actually viewed, because the watchlist compares each revision's timestamp against the page's notification_timestamp to determine if it's 'seen'.",
  "fix_plan": "The fix should prevent marking all intermediate revisions as 'seen' when viewing a historical diff. There are several approaches:\n\n1. RECOMMENDED: Only call doViewUpdates() when the newer revision in the diff is the current/latest revision of the page. When viewing a diff of two old revisions (e.g., rev 3 vs rev 5 when the current rev is 10), don't call doViewUpdates() at all, since you're viewing historical content, not the current state.\n\n2. ALTERNATIVE: Modify the logic to only update the notification timestamp if $new equals the page's latest revision. Check if $new == $this->mPage->getLatest() before calling doViewUpdates().\n\n3. ALTERNATIVE: Modify WatchlistManager::clearTitleUserNotifications() to accept a flag indicating whether we're viewing a full current-state diff vs a historical diff, and skip the notification timestamp update for historical diffs.\n\nApproach #1 or #2 is preferable because viewing a historical diff doesn't logically require marking revisions as 'seen' - that should only happen when viewing the current version of a watched page.",
  "testing": "To verify the fix:\n\n1. Create a test page with multiple revisions (at least 5-6 revisions)\n2. Watch the page\n3. Make sure you have unread revisions in your watchlist\n4. View a diff between two old revisions that are NOT the current version (e.g., ?diff=5&oldid=3 when current revision is 10)\n5. Check the watchlist - verify that ONLY the specific revisions being diffed are marked as 'seen', not all intermediate revisions\n6. Optionally: View a diff that includes the current revision (e.g., ?diff=cur&oldid=3) - in this case, it's reasonable to mark revisions up to the current as 'seen'\n\nAlso add a unit test in the WatchlistManager or Article test suite to ensure that viewing a historical diff (neither revision is current) doesn't update the watchlist notification timestamp."
}
```

### Explore

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method (around line 1149), the call to doViewUpdates() is passing (int)$new which converts false to 0, causing the latest revision to be marked as seen instead of only the revisions being diffed. The fix should either: (1) pass the actual oldid instead of the mapped new revision ID, or (2) only call doViewUpdates for specific revision pairs, not when using symbolic identifiers like 'cur'/'prev'/'next'"
    },
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "The resetNotificationTimestamp() method (starting at line 1334) should be modified to handle the case where a diff view is marking revisions as seen. When $oldid is 0 but the user is intentionally viewing a specific diff (not the latest), the current logic incorrectly marks all revisions up to the latest as seen. Need to either add a flag to distinguish 'view latest' from 'view specific diff' or change how the notification timestamp is calculated for diff-only views."
    }
  ],
  "root_cause": "Bug T419918 occurs because when viewing a diff/revision, the code in Article.php:showDiffPage() calls doViewUpdates() with the result of mapDiffPrevNext(), which can return 0 (false converted to int) when there is no next revision or when viewing 'cur'. This 0 value is passed to WatchedItemStore::resetNotificationTimestamp() as the $oldid parameter. The resetNotificationTimestamp() method interprets $oldid=0 as 'mark all revisions up to the latest as seen' rather than 'mark only this specific revision as seen', because it internally fetches the latest revision's timestamp when $oldid is falsy. This causes all revisions of the watchlisted page to be marked as 'seen' instead of just the specific revision(s) being viewed in the diff.",
  "fix_plan": "The fix should address the fact that diff viewing has different semantics than regular page viewing. When viewing a diff: (1) Identify which specific revisions are being compared (the oldid and new revision ID from the diff parameters). (2) Modify Article.php:showDiffPage() to pass the actual revision being viewed (likely the 'new' revision from the diff, but NOT as 0). (3) Alternatively, modify WatchedItemStore::resetNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a single specific revision vs. the latest version, and adjust the timestamp calculation logic accordingly. (4) The key is to ensure that when a user views a diff between old revisions, only those specific revisions are marked as seen, not all revisions up to the latest. Consider whether the watch notification should only be cleared for the specific new revision being viewed, or if it should update to reflect 'I've seen this revision', rather than 'I've seen everything up to this point'.",
  "testing": "To verify the fix works: (1) Create a page with multiple revisions (e.g., 5 different edits). (2) Watch the page. (3) Make it unwatched by viewing the latest revision. (4) Add several more revisions to the page (5 more edits). (5) View a diff between one of the earlier revisions and a middle revision (not the latest). (6) Check the watchlist - only the revisions involved in that specific diff view should be marked as 'seen', and newer revisions not shown in the diff should remain 'unseen'. (7) View another diff involving different older revisions. (8) Verify that the watchlist correctly reflects which revisions have been viewed in diffs vs. which remain unseen. Also test edge cases: viewing diffs with 'cur', 'prev', 'next' special identifiers to ensure they don't incorrectly mark all revisions as seen."
}
```

### Leverage

```json
{
  "files_to_edit": [
    {
      "path": "includes/Watchlist/WatchedItemStore.php",
      "what_to_change": "In the private function getNotificationTimestamp() (around line 1501), replace the line `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`"
    }
  ],
  "root_cause": "When viewing a specific revision/diff on a watchlisted page, the getNotificationTimestamp() method incorrectly sets the notification timestamp to the timestamp of the VIEWED revision, rather than the timestamp of the NEXT revision after the viewed one. This causes the watchlist system to mark all revisions up to and including the viewed revision as 'seen', when it should only mark revisions before the next unseen revision as 'seen'. The bug is that viewing revision 3 marks revisions 1, 2, and 3 as seen, when it should only mark 1 and 2 as seen (and keep 3+ marked as unseen).",
  "fix_plan": "The fix is straightforward: in includes/Watchlist/WatchedItemStore.php, inside the getNotificationTimestamp() private method, replace line 1501 which reads `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`. The variable $nextRev is already fetched earlier in the function (around line 1486) via `$nextRev = $this->revisionLookup->getNextRevision( $oldRev );`, so this change simply uses the already-retrieved next revision's timestamp instead of the viewed revision's timestamp. This is exactly what the existing FIXME comment on line 1502 suggests should be done.",
  "testing": "To verify the fix: (1) Create a watchlisted page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5). (2) View a specific revision's diff (e.g., revision 3). (3) Check that the notification timestamp is set to the timestamp of revision 4 (the next revision after 3), not revision 3. (4) Verify in the watchlist that revisions 4 and 5 appear as 'unseen' (with update markers), while revisions 1, 2, and 3 appear as 'seen'. (5) Alternatively, test by viewing the diff between two non-consecutive revisions (e.g., diff=3&oldid=1) and verify that only revisions after 3 are marked as unseen. The bug is confirmed if viewing any revision marks all preceding revisions (including the viewed one) as seen."
}
```

### Task-Conditioned

```json
{
  "files_to_edit": [
    {
      "path": "includes/Page/Article.php",
      "what_to_change": "In the showDiffPage() method around line 1149, the code calls `doViewUpdates()` with only the revision ID `(int)$new`, but doesn't pass the corresponding RevisionRecord. This causes the revision ID to not properly flow to the watchlist notification update code. Need to fetch the RevisionRecord for the $new revision and pass it as the third parameter to doViewUpdates()."
    },
    {
      "path": "includes/Watchlist/WatchlistManager.php",
      "what_to_change": "In the clearTitleUserNotifications() method around line 183-185, when no RevisionRecord is passed ($oldRev is null), the code defaults to $oldid = 0. The logic should be improved to handle the case where $oldid is passed but $oldRev is null, so it can still work correctly with just the integer revision ID without needing a full RevisionRecord."
    }
  ],
  "root_cause": "When viewing a diff on a watchlisted page, the Article::showDiffPage() method correctly determines which revision should be marked as seen (the 'new' or 'right side' revision via mapDiffPrevNext()). However, when calling WikiPage::doViewUpdates() on line 1149, it only passes the revision ID as an integer and not the corresponding RevisionRecord. The WatchlistManager::clearTitleUserNotifications() method expects either a RevisionRecord or will use only the integer $oldid. When $oldRev is null, the code falls back to extracting the ID: `$oldid = $oldRev?->getId() ?? 0;` which results in $oldid becoming 0 (line 184). The value 0 is then passed to WatchedItemStore::resetNotificationTimestamp(), which interprets this as 'no revision specified' and calls WatchedItemStore::getNotificationTimestamp() with $oldid=0. When $oldid is 0, the getNotificationTimestamp() method returns null (line 1477-1478), which means 'clear all notifications for this page' instead of marking only the viewed revision as seen. This causes ALL revisions to be marked as seen rather than just the one being viewed.",
  "fix_plan": "The fix requires changes to two files:\n\n1. **includes/Page/Article.php (showDiffPage method, around line 1149)**:\n   - After line 1147 where `mapDiffPrevNext()` is called to get the $new revision ID\n   - Load the RevisionRecord for the $new revision using the RevisionStore (available via $this->revisionStore)\n   - Pass this RevisionRecord as the third parameter to doViewUpdates()\n   - Example: `$newRev = $this->revisionStore->getRevisionById((int)$new); $this->mPage->doViewUpdates($context->getAuthority(), (int)$new, $newRev);`\n\n2. **includes/Watchlist/WatchlistManager.php (clearTitleUserNotifications method, around line 183-185)** (Optional but recommended):\n   - Improve the logic to properly handle the case where $oldid is provided as an integer parameter but $oldRev is null\n   - Instead of `$oldid = $oldRev?->getId() ?? 0;`, check if $oldid was explicitly passed and use that value\n   - This makes the code more robust and clearer in intent\n   - The current code works if the RevisionRecord is passed, but fails when only the integer ID is passed",
  "testing": "To verify the fix:\n\n1. **Setup**: Create a watched page with multiple revisions (at least 3)\n\n2. **Test Case 1 - View diff between two non-current revisions**:\n   - Navigate to the diff view between revision A and revision B (neither being the latest)\n   - Using the watchlist, verify that only revision B's changes are marked as seen\n   - Verify that revisions after B are still marked as unseen (have a notification timestamp)\n\n3. **Test Case 2 - View diff with latest revision**:\n   - Navigate to the diff view between revision A and the current (latest) revision\n   - Using the watchlist, verify that the entire page is marked as seen (notification timestamp is null)\n\n4. **Test Case 3 - View diff using Special:Diff**:\n   - Use Special:Diff/revisionB/revisionC or similar URLs\n   - Verify the same behavior as Test Cases 1-2\n\n5. **Test Case 4 - Different diff modes**:\n   - Test with diff=prev, diff=next, diff=cur parameters\n   - Verify that the correct revision is marked as seen in each case\n\n6. **Unit Tests to update/add**:\n   - In tests/phpunit/unit/includes/Watchlist/WatchedItemStoreUnitTest.php\n   - Add tests for the scenario where a specific revision is viewed in a diff (not the latest)\n   - Verify that only that revision's timestamp is updated, not all revisions\n\n7. **Integration Tests**:\n   - Test the full flow: user views diff \u2192 watchlist notification timestamp is updated correctly\n   - Test with multiple watched pages to ensure the fix doesn't affect unrelated pages"
}
```

## Tool Call Analysis

### Control (CTO off)

Total tool calls: 48

Top tools: `?` x48

| Tool | Count |
|---|---|
| `?` | 48 |

### Control (CTO on)

Total tool calls: 21

Top tools: `?` x21

| Tool | Count |
|---|---|
| `?` | 21 |

### Explore

Total tool calls: 32

Top tools: `?` x32

| Tool | Count |
|---|---|
| `?` | 32 |

### Leverage

Total tool calls: 35

Top tools: `?` x35

| Tool | Count |
|---|---|
| `?` | 35 |

### Task-Conditioned

Total tool calls: 38

Top tools: `?` x38

| Tool | Count |
|---|---|
| `?` | 38 |

## Context Pack Audit

- Navigation items: 0
- Risk items: 0

## Graph Quality Notes

N/A

## Prompt Effectiveness

### Control (CTO off)

- Prompt chars: 945
- Input tokens: 826
- Output tokens: 26016

### Control (CTO on)

- Prompt chars: 943
- Input tokens: 390
- Output tokens: 18643

### Explore

- Prompt chars: 929
- Input tokens: 505
- Output tokens: 21911

### Leverage

- Prompt chars: 1170
- Input tokens: 618
- Output tokens: 20051

### Task-Conditioned

- Prompt chars: 1069
- Input tokens: 623
- Output tokens: 31752

## Lessons & Action Items

- Keep context packs concise and explicitly prioritize high-confidence nodes.
- Capture parser visibility deltas across conditions to track graph quality drift.

## Verdict

**Task-Conditioned** scored highest (46.96/100), **Leverage** lowest (17.07/100). Best overall value versus the control baseline: **Control (CTO on)** (117.34 recalculated eval score). Most efficient: Control (CTO on) ($0.652), most expensive: Control (CTO off) ($1.194). All conditions passed tests.

## Notes

Sample #3 of multi-sample variance study (3 runs total).

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
  "input_tokens": 826,
  "output_tokens": 26016,
  "cache_read_tokens": 9700744,
  "cache_create_tokens": 313303,
  "cost_usd": 1.19408732,
  "duration_seconds": 361.3,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "Line 1149: When calling doViewUpdates() in the diff view code, pass the RevisionRecord for the revision being viewed ($new) as the third parameter, not just the oldid"
      }
    ],
    "root_cause": "When viewing a diff, the code at Article.php:1149 calls doViewUpdates() with only two arguments: $this->mPage->doViewUpdates( $context->getAuthority(), (int)$new ). This passes the newid as the oldid parameter but does not pass a RevisionRecord as the third argument. When doViewUpdates() then calls clearTitleUserNotifications() with $oldRev = null, the clearTitleUserNotifications() method interprets this as 3 arguments being passed and executes: $oldid = $oldRev?->getId() ?? 0, which results in $oldid becoming 0. This causes resetNotificationTimestamp() to treat it as \"no specific revision provided\" and instead fetches and uses the LATEST revision's timestamp. This marks the page as \"seen\" up to the latest revision, making all older/earlier revisions appear as \"seen\" in the watchlist.",
    "fix_plan": "Modify line 1149 in Article.php to fetch the RevisionRecord for revision $new and pass it as the third parameter to doViewUpdates(). The fix should: (1) Fetch the RevisionRecord for the revision being viewed (the $new revision from mapDiffPrevNext), (2) Pass this RevisionRecord as the third argument to doViewUpdates(). This ensures that when clearTitleUserNotifications() is called with the correct $oldRev, it will properly extract the revision ID from it and mark only the specific viewed revision as seen, not all revisions up to the latest. The pattern should match how it's done on line 586 where displaying a normal page view passes $this->fetchRevisionRecord().",
    "testing": "To test this fix: (1) Create a watchlist entry for a page with multiple revisions, (2) View a diff between two specific revisions (e.g., revision 1 vs revision 2 on a page with 5 revisions), (3) Check the watchlist - only the revision being viewed should be marked as seen, not all revisions, (4) Verify that other revisions still show as new/unseen in the watchlist, (5) Test multiple scenarios: viewing recent diffs, old diffs, comparing non-consecutive revisions to ensure the fix works correctly in all cases"
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

#### Control (CTO on)

```json
{
  "input_tokens": 390,
  "output_tokens": 18643,
  "cache_read_tokens": 5252990,
  "cache_create_tokens": 156697,
  "cost_usd": 0.6518202000000001,
  "duration_seconds": 137.399,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1149), modify the call to doViewUpdates() to only mark revisions as seen when viewing a diff that includes the current/latest revision. Currently it unconditionally marks the newer diff revision as seen, causing all intermediate revisions to appear as 'seen' in the watchlist."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Potentially add a parameter or logic to clearTitleUserNotifications() to handle the specific case of viewing a historical diff (where neither revision being compared is the current one), preventing the premature marking of unseen revisions as 'seen'."
      }
    ],
    "root_cause": "The bug occurs in Article::showDiffPage() at line 1149, which calls $this->mPage->doViewUpdates() with the newer revision ID from the diff. This triggers WatchlistManager::clearTitleUserNotifications(), which sets the watchlist notification_timestamp to mark revisions as 'seen'. However, the watchlist system uses a single timestamp per page to track which revisions are unseen. When you view a diff of two revisions (e.g., revision 3 vs revision 5), the code sets the notification_timestamp based on revision 5, which marks ALL revisions with timestamps earlier than the next revision's timestamp as 'seen'. This includes intermediate revisions (like revision 4) that the user never actually viewed, because the watchlist compares each revision's timestamp against the page's notification_timestamp to determine if it's 'seen'.",
    "fix_plan": "The fix should prevent marking all intermediate revisions as 'seen' when viewing a historical diff. There are several approaches:\n\n1. RECOMMENDED: Only call doViewUpdates() when the newer revision in the diff is the current/latest revision of the page. When viewing a diff of two old revisions (e.g., rev 3 vs rev 5 when the current rev is 10), don't call doViewUpdates() at all, since you're viewing historical content, not the current state.\n\n2. ALTERNATIVE: Modify the logic to only update the notification timestamp if $new equals the page's latest revision. Check if $new == $this->mPage->getLatest() before calling doViewUpdates().\n\n3. ALTERNATIVE: Modify WatchlistManager::clearTitleUserNotifications() to accept a flag indicating whether we're viewing a full current-state diff vs a historical diff, and skip the notification timestamp update for historical diffs.\n\nApproach #1 or #2 is preferable because viewing a historical diff doesn't logically require marking revisions as 'seen' - that should only happen when viewing the current version of a watched page.",
    "testing": "To verify the fix:\n\n1. Create a test page with multiple revisions (at least 5-6 revisions)\n2. Watch the page\n3. Make sure you have unread revisions in your watchlist\n4. View a diff between two old revisions that are NOT the current version (e.g., ?diff=5&oldid=3 when current revision is 10)\n5. Check the watchlist - verify that ONLY the specific revisions being diffed are marked as 'seen', not all intermediate revisions\n6. Optionally: View a diff that includes the current revision (e.g., ?diff=cur&oldid=3) - in this case, it's reasonable to mark revisions up to the current as 'seen'\n\nAlso add a unit test in the WatchlistManager or Article test suite to ensure that viewing a historical diff (neither revision is current) doesn't update the watchlist notification timestamp."
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
    }
  ]
}
```

#### Explore

```json
{
  "input_tokens": 505,
  "output_tokens": 21911,
  "cache_read_tokens": 7308411,
  "cache_create_tokens": 116232,
  "cost_usd": 0.7889528800000001,
  "duration_seconds": 195.962,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1149), the call to doViewUpdates() is passing (int)$new which converts false to 0, causing the latest revision to be marked as seen instead of only the revisions being diffed. The fix should either: (1) pass the actual oldid instead of the mapped new revision ID, or (2) only call doViewUpdates for specific revision pairs, not when using symbolic identifiers like 'cur'/'prev'/'next'"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The resetNotificationTimestamp() method (starting at line 1334) should be modified to handle the case where a diff view is marking revisions as seen. When $oldid is 0 but the user is intentionally viewing a specific diff (not the latest), the current logic incorrectly marks all revisions up to the latest as seen. Need to either add a flag to distinguish 'view latest' from 'view specific diff' or change how the notification timestamp is calculated for diff-only views."
      }
    ],
    "root_cause": "Bug T419918 occurs because when viewing a diff/revision, the code in Article.php:showDiffPage() calls doViewUpdates() with the result of mapDiffPrevNext(), which can return 0 (false converted to int) when there is no next revision or when viewing 'cur'. This 0 value is passed to WatchedItemStore::resetNotificationTimestamp() as the $oldid parameter. The resetNotificationTimestamp() method interprets $oldid=0 as 'mark all revisions up to the latest as seen' rather than 'mark only this specific revision as seen', because it internally fetches the latest revision's timestamp when $oldid is falsy. This causes all revisions of the watchlisted page to be marked as 'seen' instead of just the specific revision(s) being viewed in the diff.",
    "fix_plan": "The fix should address the fact that diff viewing has different semantics than regular page viewing. When viewing a diff: (1) Identify which specific revisions are being compared (the oldid and new revision ID from the diff parameters). (2) Modify Article.php:showDiffPage() to pass the actual revision being viewed (likely the 'new' revision from the diff, but NOT as 0). (3) Alternatively, modify WatchedItemStore::resetNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a single specific revision vs. the latest version, and adjust the timestamp calculation logic accordingly. (4) The key is to ensure that when a user views a diff between old revisions, only those specific revisions are marked as seen, not all revisions up to the latest. Consider whether the watch notification should only be cleared for the specific new revision being viewed, or if it should update to reflect 'I've seen this revision', rather than 'I've seen everything up to this point'.",
    "testing": "To verify the fix works: (1) Create a page with multiple revisions (e.g., 5 different edits). (2) Watch the page. (3) Make it unwatched by viewing the latest revision. (4) Add several more revisions to the page (5 more edits). (5) View a diff between one of the earlier revisions and a middle revision (not the latest). (6) Check the watchlist - only the revisions involved in that specific diff view should be marked as 'seen', and newer revisions not shown in the diff should remain 'unseen'. (7) View another diff involving different older revisions. (8) Verify that the watchlist correctly reflects which revisions have been viewed in diffs vs. which remain unseen. Also test edge cases: viewing diffs with 'cur', 'prev', 'next' special identifiers to ensure they don't incorrectly mark all revisions as seen."
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
    }
  ]
}
```

#### Leverage

```json
{
  "input_tokens": 618,
  "output_tokens": 20051,
  "cache_read_tokens": 7346327,
  "cache_create_tokens": 66294,
  "cost_usd": 0.7346985599999999,
  "duration_seconds": 162.705,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "In the private function getNotificationTimestamp() (around line 1501), replace the line `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`"
      }
    ],
    "root_cause": "When viewing a specific revision/diff on a watchlisted page, the getNotificationTimestamp() method incorrectly sets the notification timestamp to the timestamp of the VIEWED revision, rather than the timestamp of the NEXT revision after the viewed one. This causes the watchlist system to mark all revisions up to and including the viewed revision as 'seen', when it should only mark revisions before the next unseen revision as 'seen'. The bug is that viewing revision 3 marks revisions 1, 2, and 3 as seen, when it should only mark 1 and 2 as seen (and keep 3+ marked as unseen).",
    "fix_plan": "The fix is straightforward: in includes/Watchlist/WatchedItemStore.php, inside the getNotificationTimestamp() private method, replace line 1501 which reads `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`. The variable $nextRev is already fetched earlier in the function (around line 1486) via `$nextRev = $this->revisionLookup->getNextRevision( $oldRev );`, so this change simply uses the already-retrieved next revision's timestamp instead of the viewed revision's timestamp. This is exactly what the existing FIXME comment on line 1502 suggests should be done.",
    "testing": "To verify the fix: (1) Create a watchlisted page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5). (2) View a specific revision's diff (e.g., revision 3). (3) Check that the notification timestamp is set to the timestamp of revision 4 (the next revision after 3), not revision 3. (4) Verify in the watchlist that revisions 4 and 5 appear as 'unseen' (with update markers), while revisions 1, 2, and 3 appear as 'seen'. (5) Alternatively, test by viewing the diff between two non-consecutive revisions (e.g., diff=3&oldid=1) and verify that only revisions after 3 are marked as unseen. The bug is confirmed if viewing any revision marks all preceding revisions (including the viewed one) as seen."
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
    }
  ]
}
```

#### Task-Conditioned

```json
{
  "input_tokens": 623,
  "output_tokens": 31752,
  "cache_read_tokens": 9155757,
  "cache_create_tokens": 121019,
  "cost_usd": 0.98098596,
  "duration_seconds": 195.438,
  "structured_output": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method around line 1149, the code calls `doViewUpdates()` with only the revision ID `(int)$new`, but doesn't pass the corresponding RevisionRecord. This causes the revision ID to not properly flow to the watchlist notification update code. Need to fetch the RevisionRecord for the $new revision and pass it as the third parameter to doViewUpdates()."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "In the clearTitleUserNotifications() method around line 183-185, when no RevisionRecord is passed ($oldRev is null), the code defaults to $oldid = 0. The logic should be improved to handle the case where $oldid is passed but $oldRev is null, so it can still work correctly with just the integer revision ID without needing a full RevisionRecord."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, the Article::showDiffPage() method correctly determines which revision should be marked as seen (the 'new' or 'right side' revision via mapDiffPrevNext()). However, when calling WikiPage::doViewUpdates() on line 1149, it only passes the revision ID as an integer and not the corresponding RevisionRecord. The WatchlistManager::clearTitleUserNotifications() method expects either a RevisionRecord or will use only the integer $oldid. When $oldRev is null, the code falls back to extracting the ID: `$oldid = $oldRev?->getId() ?? 0;` which results in $oldid becoming 0 (line 184). The value 0 is then passed to WatchedItemStore::resetNotificationTimestamp(), which interprets this as 'no revision specified' and calls WatchedItemStore::getNotificationTimestamp() with $oldid=0. When $oldid is 0, the getNotificationTimestamp() method returns null (line 1477-1478), which means 'clear all notifications for this page' instead of marking only the viewed revision as seen. This causes ALL revisions to be marked as seen rather than just the one being viewed.",
    "fix_plan": "The fix requires changes to two files:\n\n1. **includes/Page/Article.php (showDiffPage method, around line 1149)**:\n   - After line 1147 where `mapDiffPrevNext()` is called to get the $new revision ID\n   - Load the RevisionRecord for the $new revision using the RevisionStore (available via $this->revisionStore)\n   - Pass this RevisionRecord as the third parameter to doViewUpdates()\n   - Example: `$newRev = $this->revisionStore->getRevisionById((int)$new); $this->mPage->doViewUpdates($context->getAuthority(), (int)$new, $newRev);`\n\n2. **includes/Watchlist/WatchlistManager.php (clearTitleUserNotifications method, around line 183-185)** (Optional but recommended):\n   - Improve the logic to properly handle the case where $oldid is provided as an integer parameter but $oldRev is null\n   - Instead of `$oldid = $oldRev?->getId() ?? 0;`, check if $oldid was explicitly passed and use that value\n   - This makes the code more robust and clearer in intent\n   - The current code works if the RevisionRecord is passed, but fails when only the integer ID is passed",
    "testing": "To verify the fix:\n\n1. **Setup**: Create a watched page with multiple revisions (at least 3)\n\n2. **Test Case 1 - View diff between two non-current revisions**:\n   - Navigate to the diff view between revision A and revision B (neither being the latest)\n   - Using the watchlist, verify that only revision B's changes are marked as seen\n   - Verify that revisions after B are still marked as unseen (have a notification timestamp)\n\n3. **Test Case 2 - View diff with latest revision**:\n   - Navigate to the diff view between revision A and the current (latest) revision\n   - Using the watchlist, verify that the entire page is marked as seen (notification timestamp is null)\n\n4. **Test Case 3 - View diff using Special:Diff**:\n   - Use Special:Diff/revisionB/revisionC or similar URLs\n   - Verify the same behavior as Test Cases 1-2\n\n5. **Test Case 4 - Different diff modes**:\n   - Test with diff=prev, diff=next, diff=cur parameters\n   - Verify that the correct revision is marked as seen in each case\n\n6. **Unit Tests to update/add**:\n   - In tests/phpunit/unit/includes/Watchlist/WatchedItemStoreUnitTest.php\n   - Add tests for the scenario where a specific revision is viewed in a diff (not the latest)\n   - Verify that only that revision's timestamp is updated, not all revisions\n\n7. **Integration Tests**:\n   - Test the full flow: user views diff \u2192 watchlist notification timestamp is updated correctly\n   - Test with multiple watched pages to ensure the fix doesn't affect unrelated pages"
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
    "root_cause_quality": 0.6666666666666666,
    "fix_plan_quality": 0.2857142857142857,
    "testing_quality": 0.5,
    "efficiency": 0.14346303644738698
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 38.64,
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
        "what_to_change": "Line 1149: When calling doViewUpdates() in the diff view code, pass the RevisionRecord for the revision being viewed ($new) as the third parameter, not just the oldid"
      }
    ],
    "root_cause": "When viewing a diff, the code at Article.php:1149 calls doViewUpdates() with only two arguments: $this->mPage->doViewUpdates( $context->getAuthority(), (int)$new ). This passes the newid as the oldid parameter but does not pass a RevisionRecord as the third argument. When doViewUpdates() then calls clearTitleUserNotifications() with $oldRev = null, the clearTitleUserNotifications() method interprets this as 3 arguments being passed and executes: $oldid = $oldRev?->getId() ?? 0, which results in $oldid becoming 0. This causes resetNotificationTimestamp() to treat it as \"no specific revision provided\" and instead fetches and uses the LATEST revision's timestamp. This marks the page as \"seen\" up to the latest revision, making all older/earlier revisions appear as \"seen\" in the watchlist.",
    "fix_plan": "Modify line 1149 in Article.php to fetch the RevisionRecord for revision $new and pass it as the third parameter to doViewUpdates(). The fix should: (1) Fetch the RevisionRecord for the revision being viewed (the $new revision from mapDiffPrevNext), (2) Pass this RevisionRecord as the third argument to doViewUpdates(). This ensures that when clearTitleUserNotifications() is called with the correct $oldRev, it will properly extract the revision ID from it and mark only the specific viewed revision as seen, not all revisions up to the latest. The pattern should match how it's done on line 586 where displaying a normal page view passes $this->fetchRevisionRecord().",
    "testing": "To test this fix: (1) Create a watchlist entry for a page with multiple revisions, (2) View a diff between two specific revisions (e.g., revision 1 vs revision 2 on a page with 5 revisions), (3) Check the watchlist - only the revision being viewed should be marked as seen, not all revisions, (4) Verify that other revisions still show as new/unseen in the watchlist, (5) Test multiple scenarios: viewing recent diffs, old diffs, comparing non-consecutive revisions to ensure the fix works correctly in all cases"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 38.64,
    "normalized_score": 38.64,
    "normalization_delta": 0.0
  }
}
```

#### Control (CTO on)

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.6666666666666666,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.2347913327249107
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 37.14,
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
    "includes/Watchlist/WatchlistManager.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method (around line 1149), modify the call to doViewUpdates() to only mark revisions as seen when viewing a diff that includes the current/latest revision. Currently it unconditionally marks the newer diff revision as seen, causing all intermediate revisions to appear as 'seen' in the watchlist."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "Potentially add a parameter or logic to clearTitleUserNotifications() to handle the specific case of viewing a historical diff (where neither revision being compared is the current one), preventing the premature marking of unseen revisions as 'seen'."
      }
    ],
    "root_cause": "The bug occurs in Article::showDiffPage() at line 1149, which calls $this->mPage->doViewUpdates() with the newer revision ID from the diff. This triggers WatchlistManager::clearTitleUserNotifications(), which sets the watchlist notification_timestamp to mark revisions as 'seen'. However, the watchlist system uses a single timestamp per page to track which revisions are unseen. When you view a diff of two revisions (e.g., revision 3 vs revision 5), the code sets the notification_timestamp based on revision 5, which marks ALL revisions with timestamps earlier than the next revision's timestamp as 'seen'. This includes intermediate revisions (like revision 4) that the user never actually viewed, because the watchlist compares each revision's timestamp against the page's notification_timestamp to determine if it's 'seen'.",
    "fix_plan": "The fix should prevent marking all intermediate revisions as 'seen' when viewing a historical diff. There are several approaches:\n\n1. RECOMMENDED: Only call doViewUpdates() when the newer revision in the diff is the current/latest revision of the page. When viewing a diff of two old revisions (e.g., rev 3 vs rev 5 when the current rev is 10), don't call doViewUpdates() at all, since you're viewing historical content, not the current state.\n\n2. ALTERNATIVE: Modify the logic to only update the notification timestamp if $new equals the page's latest revision. Check if $new == $this->mPage->getLatest() before calling doViewUpdates().\n\n3. ALTERNATIVE: Modify WatchlistManager::clearTitleUserNotifications() to accept a flag indicating whether we're viewing a full current-state diff vs a historical diff, and skip the notification timestamp update for historical diffs.\n\nApproach #1 or #2 is preferable because viewing a historical diff doesn't logically require marking revisions as 'seen' - that should only happen when viewing the current version of a watched page.",
    "testing": "To verify the fix:\n\n1. Create a test page with multiple revisions (at least 5-6 revisions)\n2. Watch the page\n3. Make sure you have unread revisions in your watchlist\n4. View a diff between two old revisions that are NOT the current version (e.g., ?diff=5&oldid=3 when current revision is 10)\n5. Check the watchlist - verify that ONLY the specific revisions being diffed are marked as 'seen', not all intermediate revisions\n6. Optionally: View a diff that includes the current revision (e.g., ?diff=cur&oldid=3) - in this case, it's reasonable to mark revisions up to the current as 'seen'\n\nAlso add a unit test in the WatchlistManager or Article test suite to ensure that viewing a historical diff (neither revision is current) doesn't update the watchlist notification timestamp."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 37.14,
    "normalized_score": 37.14,
    "normalization_delta": 0.0
  }
}
```

#### Explore

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 0.5555555555555556,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.5,
    "efficiency": 0.20223410441961603
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 32.16,
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
        "what_to_change": "In the showDiffPage() method (around line 1149), the call to doViewUpdates() is passing (int)$new which converts false to 0, causing the latest revision to be marked as seen instead of only the revisions being diffed. The fix should either: (1) pass the actual oldid instead of the mapped new revision ID, or (2) only call doViewUpdates for specific revision pairs, not when using symbolic identifiers like 'cur'/'prev'/'next'"
      },
      {
        "path": "includes/Watchlist/WatchedItemStore.php",
        "what_to_change": "The resetNotificationTimestamp() method (starting at line 1334) should be modified to handle the case where a diff view is marking revisions as seen. When $oldid is 0 but the user is intentionally viewing a specific diff (not the latest), the current logic incorrectly marks all revisions up to the latest as seen. Need to either add a flag to distinguish 'view latest' from 'view specific diff' or change how the notification timestamp is calculated for diff-only views."
      }
    ],
    "root_cause": "Bug T419918 occurs because when viewing a diff/revision, the code in Article.php:showDiffPage() calls doViewUpdates() with the result of mapDiffPrevNext(), which can return 0 (false converted to int) when there is no next revision or when viewing 'cur'. This 0 value is passed to WatchedItemStore::resetNotificationTimestamp() as the $oldid parameter. The resetNotificationTimestamp() method interprets $oldid=0 as 'mark all revisions up to the latest as seen' rather than 'mark only this specific revision as seen', because it internally fetches the latest revision's timestamp when $oldid is falsy. This causes all revisions of the watchlisted page to be marked as 'seen' instead of just the specific revision(s) being viewed in the diff.",
    "fix_plan": "The fix should address the fact that diff viewing has different semantics than regular page viewing. When viewing a diff: (1) Identify which specific revisions are being compared (the oldid and new revision ID from the diff parameters). (2) Modify Article.php:showDiffPage() to pass the actual revision being viewed (likely the 'new' revision from the diff, but NOT as 0). (3) Alternatively, modify WatchedItemStore::resetNotificationTimestamp() to accept an additional parameter indicating whether we're viewing a single specific revision vs. the latest version, and adjust the timestamp calculation logic accordingly. (4) The key is to ensure that when a user views a diff between old revisions, only those specific revisions are marked as seen, not all revisions up to the latest. Consider whether the watch notification should only be cleared for the specific new revision being viewed, or if it should update to reflect 'I've seen this revision', rather than 'I've seen everything up to this point'.",
    "testing": "To verify the fix works: (1) Create a page with multiple revisions (e.g., 5 different edits). (2) Watch the page. (3) Make it unwatched by viewing the latest revision. (4) Add several more revisions to the page (5 more edits). (5) View a diff between one of the earlier revisions and a middle revision (not the latest). (6) Check the watchlist - only the revisions involved in that specific diff view should be marked as 'seen', and newer revisions not shown in the diff should remain 'unseen'. (7) View another diff involving different older revisions. (8) Verify that the watchlist correctly reflects which revisions have been viewed in diffs vs. which remain unseen. Also test edge cases: viewing diffs with 'cur', 'prev', 'next' special identifiers to ensure they don't incorrectly mark all revisions as seen."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 32.16,
    "normalized_score": 32.16,
    "normalization_delta": 0.0
  }
}
```

#### Leverage

```json
{
  "scores": {
    "files_identified": 0.0,
    "root_cause_quality": 0.2222222222222222,
    "fix_plan_quality": 0.0,
    "testing_quality": 0.625,
    "efficiency": 0.21397272720736835
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 17.07,
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
        "what_to_change": "In the private function getNotificationTimestamp() (around line 1501), replace the line `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`"
      }
    ],
    "root_cause": "When viewing a specific revision/diff on a watchlisted page, the getNotificationTimestamp() method incorrectly sets the notification timestamp to the timestamp of the VIEWED revision, rather than the timestamp of the NEXT revision after the viewed one. This causes the watchlist system to mark all revisions up to and including the viewed revision as 'seen', when it should only mark revisions before the next unseen revision as 'seen'. The bug is that viewing revision 3 marks revisions 1, 2, and 3 as seen, when it should only mark 1 and 2 as seen (and keep 3+ marked as unseen).",
    "fix_plan": "The fix is straightforward: in includes/Watchlist/WatchedItemStore.php, inside the getNotificationTimestamp() private method, replace line 1501 which reads `$notificationTimestamp = $this->revisionLookup->getTimestampFromId( $oldid );` with `$notificationTimestamp = $nextRev->getTimestamp();`. The variable $nextRev is already fetched earlier in the function (around line 1486) via `$nextRev = $this->revisionLookup->getNextRevision( $oldRev );`, so this change simply uses the already-retrieved next revision's timestamp instead of the viewed revision's timestamp. This is exactly what the existing FIXME comment on line 1502 suggests should be done.",
    "testing": "To verify the fix: (1) Create a watchlisted page with multiple revisions (e.g., revisions 1, 2, 3, 4, 5). (2) View a specific revision's diff (e.g., revision 3). (3) Check that the notification timestamp is set to the timestamp of revision 4 (the next revision after 3), not revision 3. (4) Verify in the watchlist that revisions 4 and 5 appear as 'unseen' (with update markers), while revisions 1, 2, and 3 appear as 'seen'. (5) Alternatively, test by viewing the diff between two non-consecutive revisions (e.g., diff=3&oldid=1) and verify that only revisions after 3 are marked as unseen. The bug is confirmed if viewing any revision marks all preceding revisions (including the viewed one) as seen."
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 17.07,
    "normalized_score": 17.07,
    "normalization_delta": 0.0
  }
}
```

#### Task-Conditioned

```json
{
  "scores": {
    "files_identified": 0.25,
    "root_cause_quality": 1.0,
    "fix_plan_quality": 0.14285714285714285,
    "testing_quality": 0.625,
    "efficiency": 0.16935002343296274
  },
  "weights": {
    "files_identified": 35,
    "root_cause_quality": 25,
    "fix_plan_quality": 15,
    "testing_quality": 15,
    "efficiency": 10
  },
  "weighted_score": 46.96,
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
    "includes/Watchlist/WatchlistManager.php"
  ],
  "candidate": {
    "files_to_edit": [
      {
        "path": "includes/Page/Article.php",
        "what_to_change": "In the showDiffPage() method around line 1149, the code calls `doViewUpdates()` with only the revision ID `(int)$new`, but doesn't pass the corresponding RevisionRecord. This causes the revision ID to not properly flow to the watchlist notification update code. Need to fetch the RevisionRecord for the $new revision and pass it as the third parameter to doViewUpdates()."
      },
      {
        "path": "includes/Watchlist/WatchlistManager.php",
        "what_to_change": "In the clearTitleUserNotifications() method around line 183-185, when no RevisionRecord is passed ($oldRev is null), the code defaults to $oldid = 0. The logic should be improved to handle the case where $oldid is passed but $oldRev is null, so it can still work correctly with just the integer revision ID without needing a full RevisionRecord."
      }
    ],
    "root_cause": "When viewing a diff on a watchlisted page, the Article::showDiffPage() method correctly determines which revision should be marked as seen (the 'new' or 'right side' revision via mapDiffPrevNext()). However, when calling WikiPage::doViewUpdates() on line 1149, it only passes the revision ID as an integer and not the corresponding RevisionRecord. The WatchlistManager::clearTitleUserNotifications() method expects either a RevisionRecord or will use only the integer $oldid. When $oldRev is null, the code falls back to extracting the ID: `$oldid = $oldRev?->getId() ?? 0;` which results in $oldid becoming 0 (line 184). The value 0 is then passed to WatchedItemStore::resetNotificationTimestamp(), which interprets this as 'no revision specified' and calls WatchedItemStore::getNotificationTimestamp() with $oldid=0. When $oldid is 0, the getNotificationTimestamp() method returns null (line 1477-1478), which means 'clear all notifications for this page' instead of marking only the viewed revision as seen. This causes ALL revisions to be marked as seen rather than just the one being viewed.",
    "fix_plan": "The fix requires changes to two files:\n\n1. **includes/Page/Article.php (showDiffPage method, around line 1149)**:\n   - After line 1147 where `mapDiffPrevNext()` is called to get the $new revision ID\n   - Load the RevisionRecord for the $new revision using the RevisionStore (available via $this->revisionStore)\n   - Pass this RevisionRecord as the third parameter to doViewUpdates()\n   - Example: `$newRev = $this->revisionStore->getRevisionById((int)$new); $this->mPage->doViewUpdates($context->getAuthority(), (int)$new, $newRev);`\n\n2. **includes/Watchlist/WatchlistManager.php (clearTitleUserNotifications method, around line 183-185)** (Optional but recommended):\n   - Improve the logic to properly handle the case where $oldid is provided as an integer parameter but $oldRev is null\n   - Instead of `$oldid = $oldRev?->getId() ?? 0;`, check if $oldid was explicitly passed and use that value\n   - This makes the code more robust and clearer in intent\n   - The current code works if the RevisionRecord is passed, but fails when only the integer ID is passed",
    "testing": "To verify the fix:\n\n1. **Setup**: Create a watched page with multiple revisions (at least 3)\n\n2. **Test Case 1 - View diff between two non-current revisions**:\n   - Navigate to the diff view between revision A and revision B (neither being the latest)\n   - Using the watchlist, verify that only revision B's changes are marked as seen\n   - Verify that revisions after B are still marked as unseen (have a notification timestamp)\n\n3. **Test Case 2 - View diff with latest revision**:\n   - Navigate to the diff view between revision A and the current (latest) revision\n   - Using the watchlist, verify that the entire page is marked as seen (notification timestamp is null)\n\n4. **Test Case 3 - View diff using Special:Diff**:\n   - Use Special:Diff/revisionB/revisionC or similar URLs\n   - Verify the same behavior as Test Cases 1-2\n\n5. **Test Case 4 - Different diff modes**:\n   - Test with diff=prev, diff=next, diff=cur parameters\n   - Verify that the correct revision is marked as seen in each case\n\n6. **Unit Tests to update/add**:\n   - In tests/phpunit/unit/includes/Watchlist/WatchedItemStoreUnitTest.php\n   - Add tests for the scenario where a specific revision is viewed in a diff (not the latest)\n   - Verify that only that revision's timestamp is updated, not all revisions\n\n7. **Integration Tests**:\n   - Test the full flow: user views diff \u2192 watchlist notification timestamp is updated correctly\n   - Test with multiple watched pages to ensure the fix doesn't affect unrelated pages"
  },
  "guardrails": {
    "path_format_issues": {
      "absolute_count": 0,
      "markdown_link_count": 0,
      "line_anchor_count": 0
    },
    "raw_score": 46.96,
    "normalized_score": 46.96,
    "normalization_delta": 0.0
  }
}
```

