# Development navigation eval

61 navigation questions a developer would ask about seven real repositories,
each with hand-verified ground truth. Explore is scored against two ripgrep
baselines with `../heldout/run.py`.

## Purpose

**This set is for iterating.** Study it, read individual failures, and rerun it
as often as you like while you improve Explore's generic ranking. It is
deliberately large and varied, so that a fix which only helps a few questions
shows up as noise inside the confidence intervals, not as progress.

**Never iterate on the private held-out set.** A separate held-out set, kept
elsewhere, is the only measure of whether a change generalizes. Do not look
for it, and do not tune against its numbers. Cardinal rule 2 still applies
here: fix generic mechanisms, never a specific question.

The 25 questions from the first held-out eval (`../heldout/`) were read in full
during the Phase 3 analysis, so they no longer measure generalization. They
are included here with `"origin": "heldout-v1-spent"`. The 36 new ones carry
`"origin": "devset-v1"`.

## Composition

| Repo | Language | Tracked files | Questions |
| --- | --- | --- | --- |
| SP42 | Rust | 512 | 9 (spent) |
| aedventure | TypeScript, some Rust | 1,845 | 9 (spent) |
| blybot | Python | 175 | 7 (spent) |
| aerie | Python, shell | 1,108 | 9 |
| aeptus-product-design | Python, many docs and YAML | 2,961 | 9 |
| XeenRemastered | C++ (a ScummVM tree); a large-repo stress case | 23,915 | 9 |
| website | TypeScript, Astro | 713 | 9 |

By kind: `where_change` 13, `which_file_handles` 12, `where_implemented` 10,
`config_read` 10, `callers` 8, `concept_to_code` 8. `concept_to_code` is a
product-language question whose answer is code. 25 of the 61 questions accept
more than one file.

The new questions deliberately include three shapes, tagged in `notes`:

- `[B-product-language]`: phrased in user or product words that mostly only
  docs share (the vocabulary-gap failure class);
- `[A-hub]`: a large hub file (a README, a big CLI module, an engine main)
  mentions everything in the question, but the answer is a smaller file (the
  scoring failure class);
- `[small-file]`: the answer is a small, specific file.

The target repositories are read-only. The runner only reads them and audits
`git status` and `.aethyme/` before and after each run.

## Schema

One JSON object per line in `questions.jsonl`:

```json
{"id": "aerie-03", "repo": "aerie", "question": "...", "kind": "concept_to_code",
 "answers": [{"path": "aerie/plugins/quota-watch/pricing.py", "start_line": 40, "end_line": 65, "primary": true},
             {"path": "aerie/plugins/quota-watch/dashboard.py", "start_line": 29, "end_line": 34, "primary": false}],
 "notes": "...", "origin": "devset-v1"}
```

- `answers` lists every file a reasonable developer would accept. `primary`
  marks the best one. If `primary` is missing, it counts as true, so older
  files keep working and every listed answer is primary. The spent questions
  have no flags.
- Scoring is by path. The line ranges document the answer.
- An optional `repo_root` on a question overrides where its repo lives.
- `repos.json` maps repo names to roots. `~` is expanded, and relative paths
  resolve against the file.

Add questions in the same order as before: read the code, write the question
and its answers, and only then run any tool on it. Never put the answer's file
name or symbol in the question.

## Running it

```bash
cd packages/aethyme-eval/devset
AETHYME_HELDOUT_RG=/opt/homebrew/bin/rg python3 -B ../heldout/run.py \
    --questions questions.jsonl --label <name> [--aethyme <binary>]
# options: --only <id> (repeatable), --repo-root NAME=PATH (repeatable),
#          --repos-json <file>, --methods explore,rg,bm25, --date <YYYY-MM-DD>
```

The script needs Python 3.11 or later and uses only the standard library. Set
`AETHYME_HELDOUT_RG` when `rg` on `PATH` is a shim. Results go to
`results/<date>-<label>.json`, which holds the full records, repo HEADs,
answer-rule precision and the `repo_writes` audit, and to a matching `.md`
summary.

Methods:

- **Explore:** `aethyme explore --format answer-json --depth 0
  --show-observability`. The ranked list is the `answer[]` paths, then
  `navigation_hints[]`, then each subsystem's `top_verification_targets[]`,
  with duplicates removed.
- **naive rg:** the question's non-stopword terms (at least 3 characters),
  each searched with `rg -i -c -F`. Files are ranked by the number of distinct
  terms they hit, then by the number of matching lines. This is the original
  baseline, and it is a strawman.
- **BM25 rg:** the same terms, searched with `rg -i --count-matches -F`, and
  ranked by Okapi BM25 with k1 = 1.2 and b = 0.75. IDF is computed over every
  non-binary file that `rg --files` lists, and document length is measured in
  tokens. This is the competent lexical search that Explore's scoring changes
  must be judged against.

Metrics:

- recall@1/3/5/8 for any accepted path and for the primary path only, each
  with a 95% Wilson interval;
- MRR within the first 25 paths, with a 95% percentile-bootstrap interval
  (fixed seed; Wilson intervals only apply to proportions);
- all of the above per repo, per kind, per origin and in total.

**Answer-rule precision** groups questions by
`observability.source_fallback.answer_safety` (`safe` and `rule`) and reports
how often the top hit is correct. The engine no longer promotes the top hit to
`answer[]`, but the verdict is still reported. A rule could be re-enabled only
if its precision clears the 95% bar.

## Baseline (2026-09-24)

These are the first measurements on the development set. Explore was run
twice: with the installed `aethyme 0.8.2` (build `4b63a60c`, before Phase 3)
and with a release build of main at `6581f7c2`. The baselines are
deterministic and identical across both runs. All seven repos lack a
materialized graph, so every Explore call was `degraded` with
`graph_store_missing`.

Any accepted answer, N = 61. Each cell gives the count or value with its 95%
interval.

| Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- |
| Explore, installed 0.8.2 | 5/61 (0.04–0.18) | 9/61 (0.08–0.26) | 12/61 (0.12–0.31) | 0.127 (0.06–0.20) |
| Explore, main 6581f7c2 | 16/61 (0.17–0.38) | 30/61 (0.37–0.61) | 44/61 (0.60–0.82) | 0.405 (0.31–0.50) |
| naive rg | 2/61 (0.01–0.11) | 6/61 (0.05–0.20) | 12/61 (0.12–0.31) | 0.093 (0.05–0.15) |
| BM25 rg | 12/61 (0.12–0.31) | 24/61 (0.28–0.52) | 38/61 (0.50–0.73) | 0.335 (0.25–0.42) |

Primary answer only: Explore on main scores R@1 16/61, R@3 29/61, R@8 39/61
and MRR 0.388. BM25 scores 11/61, 23/61, 31/61 and 0.303.

Split by origin, any accepted answer:

| Origin | Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- | --- |
| devset-v1 (36) | Explore, installed | 2/36 | 3/36 | 4/36 | 0.076 |
| devset-v1 (36) | Explore, main | 7/36 | 14/36 | 23/36 | 0.324 |
| devset-v1 (36) | naive rg | 0/36 | 2/36 | 7/36 | 0.056 |
| devset-v1 (36) | BM25 rg | 7/36 | 14/36 | 24/36 | 0.336 |
| heldout-v1-spent (25) | Explore, installed | 3/25 | 6/25 | 8/25 | 0.200 |
| heldout-v1-spent (25) | Explore, main | 9/25 | 16/25 | 21/25 | 0.520 |
| heldout-v1-spent (25) | naive rg | 2/25 | 4/25 | 5/25 | 0.146 |
| heldout-v1-spent (25) | BM25 rg | 5/25 | 10/25 | 14/25 | 0.332 |

**Read this first.** On the 36 questions nobody had seen, Explore on main ties
BM25: R@1 7 against 7, R@3 14 against 14, and MRR 0.324 against 0.336. Its
whole lead over BM25 comes from the 25 spent questions that Phase 3 was
analysed against. Treat BM25 as the bar to beat.

Answer-rule precision on main: the rule said `safe` on 9 of 61 questions, and
the top hit was correct on only 2 of them (0.22, CI 0.06–0.55).
`dominant_term_coverage` was right on 2 of 5 and `exact_symbol_definition` on
0 of 4. Both are far below the 95% bar, so promotion should stay off. Three
XeenRemastered questions reported `search_incomplete`.

Repo-write audit: in the Aethyme-enhanced repos, both runs modified
`.aethyme/broker.db*`, `.aethyme/logs/command-metrics.jsonl` and
`.aethyme/generated/experience-telemetry.jsonl`. No tracked file changed. See
`repo_writes` in the result files.
