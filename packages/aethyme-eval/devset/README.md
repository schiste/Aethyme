# Development navigation eval

61 navigation questions a developer would ask about seven real repositories,
each with hand-verified ground truth. Explore is scored against three ripgrep
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
product-language question whose answer is code. 27 of the 61 questions accept
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

- `answers` lists every span a reasonable developer would accept. A path may
  appear more than once with different ranges. `primary` marks the best ones.
  If `primary` is missing, it counts as true, so older files keep working.
- Line ranges are scored (span overlap, below), so keep them tight: the lines
  that actually answer the question, not the whole enclosing function. An
  answer without `start_line` accepts the whole file.
- `audit` (optional) records the verdict and reason from the 2026-09-24 answer
  audit for the 20 questions it changed. `reworded: "2026-09-24 audit"` marks
  the 12 questions whose wording was changed then: 10 that leaked the answer's
  file name, path segment or symbol, and 2 that were ambiguous (`xeen-06` now
  says "In Xeen", `apd-07` no longer rests on a wrong premise).
- An optional `repo_root` on a question overrides where its repo lives.
- `repos.json` maps repo names to roots. `~` is expanded, and relative paths
  resolve against the file.

Add questions in the same order as before: read the code, write the question
and its answers, and only then run any tool on it. Never put the answer's file
name, a distinctive path segment or a symbol in the question; ask the way a
developer new to the repo would.

## Running it

```bash
cd packages/aethyme-eval/devset
AETHYME_HELDOUT_RG=/opt/homebrew/bin/rg python3 -B ../heldout/run.py \
    --questions questions.jsonl --label <name> [--aethyme <binary>] \
    --methods explore,rg,bm25,bm25plus
# options: --only <id> (repeatable), --repo-root NAME=PATH (repeatable),
#          --repos-json <file>, --date <YYYY-MM-DD>,
#          --host-cache warm|cold|shared (default warm)

# paired comparison of two runs, or of two methods in one run; runs nothing
python3 -B ../heldout/run.py --compare results/A.json results/B.json
python3 -B ../heldout/run.py --compare results/A.json results/A.json \
    --compare-methods bm25plus,explore
```

The script needs Python 3.11 or later and uses only the standard library. Set
`AETHYME_HELDOUT_RG` when `rg` on `PATH` is a shim. `--methods` defaults to
`explore,rg,bm25`; pass `bm25plus` explicitly. Results go to
`results/<date>-<label>.json`, which holds the full records (every method's
whole ranked list, up to 25 paths, and its spans, so a run can be rescored
offline), the target repos' HEAD commits (`repo_heads`), each question's
Explore search completeness (`explore_search`: `complete` or `incomplete`,
from `source_fallback.complete`), answer-rule precision and the `repo_writes`
audit, and to a matching `.md` summary.

Methods:

- **Explore:** `aethyme explore --format answer-json --depth 0
  --show-observability`. The ranked list is the `answer[]` paths, then
  `navigation_hints[]`, then each subsystem's `top_verification_targets[]`,
  with duplicates removed. Its spans are the `line_refs` of those items.
- **naive rg:** the question's non-stopword terms (at least 3 characters),
  each searched with `rg -i -c -F`. Files are ranked by the number of distinct
  terms they hit, then by the number of matching lines. This is the original
  baseline, and it is a strawman.
- **BM25 rg:** the same terms, searched with `rg -i --count-matches -F`, and
  ranked by Okapi BM25 with k1 = 1.2 and b = 0.75. IDF is computed over every
  non-binary file that `rg --files` lists, and document length is measured in
  tokens.
- **BM25+ rg:** BM25 over suffix-stemmed terms, restricted to code files
  (prose, data and lock files, docs directories and tests excluded), plus an IDF-sized bonus for
  each term that appears in a file's path. It is a few dozen lines of Python
  that any agent could run, and it is the bar Explore must clear.

A baseline returns files, not spans. For span scoring, a baseline's span in a
file is what an agent would read first: a ±2-line window around each of the 3
lines that match the most distinct query terms, for its first 8 files.

## Metrics

- **Span overlap (headline):** a question counts at rank k only when one of the
  first k paths is an accepted answer *and* one of the spans the method
  returned for it overlaps an accepted line range. This rewards pointing at
  the right lines, not just the right file, which is what saves an agent a
  read. Reported as span R@1/3/5/8 and span MRR, for any accepted answer and
  for primary answers only.
- **Path recall:** R@1/3/5/8 for any accepted path and for the primary path
  only. This is the older, more lenient metric.
- **MRR** within the first 25 paths.
- Every proportion carries a 95% Wilson interval, MRR a 95% percentile-bootstrap
  interval (fixed seed). All of it is broken down per repo, kind, origin and
  Explore search completeness.

**Comparing two runs.** Use `--compare`, not the unpaired intervals: it pairs
questions by id and reports an exact McNemar test on the discordant questions
for each recall, and a paired bootstrap of the reciprocal-rank difference for
MRR. The minimal detectable paired difference on this set is about **18
points** of recall at N = 61 (80% power, α = 0.05, with about a quarter of the
questions discordant, as seen here). A change smaller than that will usually read as noise; do not
claim it as progress.

**Answer-rule precision** groups questions by
`observability.source_fallback.answer_safety` (`safe` and `rule`) and reports
how often the top hit is correct. The engine no longer promotes the top hit to
`answer[]`, but the verdict is still reported. A rule could be re-enabled only
if its precision clears the 95% bar.

## Cache isolation

Explore's source fallback has a 2 s wall-clock budget, and its symbol index is
cached on the host (`~/Library/Caches/Aethyme` on macOS). Cache state therefore
changes how many files a question scans: on XeenRemastered a cold run scanned
4,441 files and a warm one 22,218. Results that depend on what other sessions
left in that cache are not reproducible.

The runner sets `AETHYME_HOST_CACHE_DIR` (which the engine reads before
`XDG_CACHE_HOME` and `HOME`, see `symbol_index::default_location`) to a fresh
temp directory for every run, and deletes it afterwards. `--host-cache`
chooses what happens in it:

- `warm` (default): before the questions, a throwaway query runs per repo until
  a complete scan parses no new file. The history is stored as
  `warm_up_complete_history`.
- `cold`: the directory is emptied before every Explore call.
- `shared`: the old behaviour, using the host cache. Irreproducible.

It also sets `AETHYME_MEASURE_OUTPUT=0`. The chosen mode is stored as
`host_cache` in the result.

## Baseline (2026-09-24, after the audit)

`results/2026-09-24-main-53b0e884.{json,md}`: a release build of main at
`53b0e884` (the binary reports `build_commit=unknown` because it was built from
a `git archive` export), `--host-cache warm`, all four methods. Every warm-up
settled on its second query. All seven repos lack a materialized graph, so
every Explore call was `degraded` with `graph_store_missing`. Five
XeenRemastered questions (`xeen-01`, `-02`, `-03`, `-05`, `-09`) still reported
an incomplete search even with a warm cache.

Target repo HEADs: SP42 `9c4dea0c`, XeenRemastered `139ca214`, aedventure
`0d823c50`, aeptus-product-design `bdba5371`, aerie `4f811dd8`, blybot
`a2403184`, website `1b7828a7`.

The results measured before the audit (installed 0.8.2 and main `6581f7c2`)
were removed because they no longer match the question set; they are in the
history at `30c6bc05`.

Span overlap, any accepted answer (headline):

| Questions | Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- | --- |
| devset-v1 (36) | Explore, main 53b0e884 | 4/36 | 11/36 | 17/36 | 0.231 |
| devset-v1 (36) | naive rg | 0/36 | 1/36 | 2/36 | 0.013 |
| devset-v1 (36) | BM25 rg | 6/36 | 7/36 | 12/36 | 0.197 |
| devset-v1 (36) | BM25+ rg | 7/36 | 12/36 | 14/36 | 0.273 |
| all (61) | Explore, main 53b0e884 | 8/61 | 17/61 | 27/61 | 0.230 |
| all (61) | naive rg | 1/61 | 3/61 | 4/61 | 0.032 |
| all (61) | BM25 rg | 9/61 | 14/61 | 20/61 | 0.196 |
| all (61) | BM25+ rg | 15/61 | 22/61 | 24/61 | 0.306 |

Path, any accepted answer:

| Questions | Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- | --- |
| devset-v1 (36) | Explore, main 53b0e884 | 7/36 | 14/36 | 23/36 | 0.331 |
| devset-v1 (36) | naive rg | 0/36 | 2/36 | 7/36 | 0.053 |
| devset-v1 (36) | BM25 rg | 7/36 | 12/36 | 23/36 | 0.311 |
| devset-v1 (36) | BM25+ rg | 12/36 | 21/36 | 25/36 | 0.479 |
| all (61) | Explore, main 53b0e884 | 15/61 | 26/61 | 42/61 | 0.377 |
| all (61) | naive rg | 2/61 | 5/61 | 12/61 | 0.091 |
| all (61) | BM25 rg | 11/61 | 21/61 | 38/61 | 0.308 |
| all (61) | BM25+ rg | 23/61 | 35/61 | 44/61 | 0.500 |

Primary answers only, all 61: path R@1/3/8 and MRR are Explore 15/24/35 and
0.348, BM25+ 22/30/36 and 0.439; span are Explore 8/15/21 and 0.205, BM25+
14/19/22 and 0.275. The `.md` result file has every cut with intervals.

**Read this first.** BM25+ is ahead of Explore on every cut above except
span R@8 (24 against 27). Paired over all 61 (`--compare … --compare-methods
bm25plus,explore`), BM25+ leads on path R@3 (35 against 26, p = 0.022) and
path MRR (0.500 against 0.377, CI of the difference −0.21 to −0.04). Its span
leads (R@1 15 against 8, MRR 0.306 against 0.230) are below the detectable
difference and not significant. Against plain BM25, Explore is ahead on
path and span metrics except span R@1 (8 against 9), and none of those
differences is significant. Treat BM25+ as the bar to beat.

Answer-rule precision on main: the rule said `safe` on 9 of 61 questions, and
the top hit was correct on 2 of them (0.22, CI 0.06–0.55):
`dominant_term_coverage` 2 of 5, `exact_symbol_definition` 0 of 4. Promotion
should stay off.

Repo-write audit: in the Aethyme-enhanced repos, Explore still touches
`.aethyme/broker.db*` and `.aethyme/logs/command-metrics.jsonl` (broker hooks
and other sessions write the same files, so this is a tripwire, not an
attribution). No tracked file changed. See `repo_writes`.
