# Development navigation eval

123 navigation questions a developer would ask about fifteen real repositories,
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
are included here with `"origin": "heldout-v1-spent"`. The 36 questions
written with them carry `"origin": "devset-v1"`, and the 62 added on
2026-09-24 over eight more repositories carry `"origin": "devset-v2"`
(below).

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
| map-generator | Rust | 130 | 7 (v2) |
| citogenesis | Rust, YAML | 90 | 5 (v2) |
| Aetower | Swift and Rust | 399 | 9 (v2) |
| Chau7 | Swift, some Rust and Go | 1,531 | 9 (v2) |
| aegowlg | TypeScript | 190 | 7 (v2) |
| safeskills | TypeScript, Astro | 180 | 7 (v2) |
| toolhub-evolved | Python and JavaScript | 607 | 9 (v2) |
| Mockup | Python and TypeScript (React); a large monorepo | 9,708 | 9 (v2) |
| **Total** | 15 repos | | **123** |

Tracked-file counts are `git ls-files`; the devset-v2 ones are at the HEADs
listed below.

By kind over all 123: `where_implemented` 22, `where_change` 22,
`config_read` 22, `which_file_handles` 21, `concept_to_code` 20, `callers` 16.
`concept_to_code` is a product-language question whose answer is code. 46 of
the 123 questions accept more than one file (27 of the first 61, 19 of the
62 devset-v2 questions).

The devset-v1 questions deliberately include three shapes, tagged in `notes`:

- `[B-product-language]`: phrased in user or product words that mostly only
  docs share (the vocabulary-gap failure class);
- `[A-hub]`: a large hub file (a README, a big CLI module, an engine main)
  mentions everything in the question, but the answer is a smaller file (the
  scoring failure class);
- `[small-file]`: the answer is a small, specific file.

### devset-v2 (62 questions, 2026-09-24)

Two Claude agents wrote the questions, one per group of four repositories:
group A (map-generator, citogenesis, Aetower, Chau7; 30 questions) and group
B (aegowlg, safeskills, toolhub-evolved, Mockup; 32). They read the source
with `cat`, `sed` and `rg` only; no Aethyme or ranking tool ran on any of
these questions before the ground truth was fixed. A script checked that
every answer path is tracked and clean at HEAD, that every range fits its
file, that each question has one primary answer, and that no answer file
stem leaks into the question.

A third agent then audited all 62 adversarially: it re-read every range at
HEAD, re-searched callers and duplicates, and checked each question for leaks
and ambiguity, again without running any ranking tool. It changed 25 of them,
recorded in each question's `audit` string (a free-text verdict, unlike the
v1 `audit` object):

- 16 rewordings: 13 questions echoed a word from the answer's symbol, enum
  variant or local variable (for example "verdict", "OTLP", "snooze",
  "risk tier"), which the authors' stem-only leak check missed; one
  (`aegowlg-01`) had a factual error; and two callers questions (`chau7-01`,
  `toolhub-05`) did not name the callee, so they admitted other valid
  answers.
- 6 answer fixes: three missing answers added (`mapgen-06`,
  `safeskills-01`, and the default constant for `toolhub-08`), and three
  lenient secondaries in large hub files dropped (`chau7-02`, `toolhub-06`,
  `mockup-09`).
- 6 range fixes (`mapgen-06`, `chau7-04`, `aegowlg-03`, `toolhub-07`,
  `toolhub-09`, `mockup-01`): whole-function primaries narrowed to the lines
  that answer, or widened where the answer started earlier.

Three questions had two kinds of fix, so the categories sum to 28 over 25
questions. None was dropped. devset-v2 has 12 `where_implemented`, 12 `concept_to_code`,
12 `config_read`, 9 `which_file_handles`, 9 `where_change` and 8 `callers`.

Target repo HEADs for devset-v2: map-generator `d23051cf`, citogenesis
`ce67dd45`, Aetower `f6326334`, Chau7 `d62f1f7d`, aegowlg `136f9faf`,
safeskills `b58a1c3a`, toolhub-evolved `bf1a2adc`, Mockup `5e4be565`. Some of
these checkouts had untracked or modified files at write time; no answer file
was among them. The runner records the HEADs it saw in `repo_heads`; if a
repository has moved, recheck the answers before trusting a result.

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
  audit for the 20 v1 questions it changed (an object), and the devset-v2
  audit's verdict for the 25 v2 questions it changed (a string). `reworded: "2026-09-24 audit"` marks
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
MRR. The minimal detectable paired difference on this set is about **13
points** of recall at N = 123 (12.6 points: δ = (z₀.₉₇₅ + z₀.₈₀) · √(p_d / N)
= 2.80 · √(0.25 / 123), for 80% power, α = 0.05, with about a quarter of the
questions discordant, as seen here). The same formula gave the earlier ~18
points at N = 61. On one origin alone it is about 18 points for devset-v2
(62) and larger for the smaller cuts. A change smaller than that will usually read as noise; do not
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

## Baseline (2026-09-24, 123 questions)

`results/2026-09-24-main-a520b0ca.{json,md}`: a release build of main at
`a520b0ca` (`cargo build --locked --release`; the binary reports
`v0.8.3-4-ga520b0ca-dirty` because the worktree held these devset edits,
which the build does not read), `--host-cache warm`, all four methods, all
123 questions. Every warm-up settled on its second query and every Explore
search was complete, including the XeenRemastered questions that were
incomplete in the 53b0e884 run. No repository has a materialized graph, so
every Explore call was `degraded` with `graph_store_missing`.

Target repo HEADs: the devset-v2 HEADs above, and SP42 `9c4dea0c`,
XeenRemastered `139ca214`, blybot `a2403184`, website `1b7828a7` as before.
Three v1 repositories had moved since the 53b0e884 run: aedventure
`2b57c34c` (no answer file touched), aerie `23c570af` (no answer file
touched), and aeptus-product-design `63177adf`, where
`resources/obligations/artifact.py` grew by 78 lines. The answer ranges of
`apd-05` (427-431) and `apd-09` (534-669) in that file now point about 11
lines early, so their span scores in this run are not trustworthy; their
path scores are. They were left as audited; re-anchor them, or check out
`bdba5371`, before relying on those two questions.

Span overlap, any accepted answer:

| Questions | Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- | --- |
| all (123) | Explore, main a520b0ca | 29/123 [0.17–0.32] | 45/123 [0.29–0.45] | 62/123 [0.42–0.59] | 0.319 [0.25–0.39] |
| all (123) | naive rg | 1/123 [0.00–0.04] | 7/123 [0.03–0.11] | 15/123 [0.07–0.19] | 0.039 [0.02–0.06] |
| all (123) | BM25 rg | 18/123 [0.10–0.22] | 32/123 [0.19–0.34] | 45/123 [0.29–0.45] | 0.210 [0.15–0.27] |
| all (123) | BM25+ rg | 29/123 [0.17–0.32] | 44/123 [0.28–0.45] | 57/123 [0.38–0.55] | 0.310 [0.24–0.38] |
| devset-v2 (62) | Explore, main a520b0ca | 14/62 [0.14–0.34] | 21/62 [0.23–0.46] | 32/62 [0.39–0.64] | 0.311 [0.21–0.41] |
| devset-v2 (62) | naive rg | 0/62 [0.00–0.06] | 4/62 [0.03–0.15] | 11/62 [0.10–0.29] | 0.046 [0.02–0.08] |
| devset-v2 (62) | BM25 rg | 9/62 [0.08–0.25] | 18/62 [0.19–0.41] | 25/62 [0.29–0.53] | 0.223 [0.14–0.31] |
| devset-v2 (62) | BM25+ rg | 14/62 [0.14–0.34] | 22/62 [0.25–0.48] | 32/62 [0.39–0.64] | 0.313 [0.22–0.42] |
| devset-v1 (36) | Explore, main a520b0ca | 6/36 [0.08–0.32] | 13/36 [0.23–0.52] | 17/36 [0.32–0.63] | 0.274 [0.16–0.40] |
| devset-v1 (36) | naive rg | 0/36 [0.00–0.10] | 1/36 [0.01–0.14] | 2/36 [0.01–0.18] | 0.013 [0.00–0.04] |
| devset-v1 (36) | BM25 rg | 6/36 [0.08–0.32] | 7/36 [0.10–0.35] | 12/36 [0.20–0.50] | 0.197 [0.08–0.32] |
| devset-v1 (36) | BM25+ rg | 7/36 [0.10–0.35] | 12/36 [0.20–0.50] | 15/36 [0.27–0.58] | 0.276 [0.15–0.41] |
| spent (25) | Explore, main a520b0ca | 9/25 [0.20–0.56] | 11/25 [0.27–0.63] | 13/25 [0.34–0.70] | 0.407 [0.22–0.59] |
| spent (25) | naive rg | 1/25 [0.01–0.20] | 2/25 [0.02–0.25] | 2/25 [0.02–0.25] | 0.060 [0.00–0.16] |
| spent (25) | BM25 rg | 3/25 [0.04–0.30] | 7/25 [0.14–0.48] | 8/25 [0.17–0.52] | 0.193 [0.07–0.33] |
| spent (25) | BM25+ rg | 8/25 [0.17–0.52] | 10/25 [0.23–0.59] | 10/25 [0.23–0.59] | 0.353 [0.19–0.52] |

Path, any accepted answer:

| Questions | Method | R@1 | R@3 | R@8 | MRR |
| --- | --- | --- | --- | --- | --- |
| all (123) | Explore, main a520b0ca | 42/123 [0.26–0.43] | 66/123 [0.45–0.62] | 89/123 [0.64–0.80] | 0.464 [0.39–0.54] |
| all (123) | naive rg | 4/123 [0.01–0.08] | 12/123 [0.06–0.16] | 34/123 [0.20–0.36] | 0.112 [0.08–0.15] |
| all (123) | BM25 rg | 24/123 [0.14–0.27] | 50/123 [0.32–0.49] | 80/123 [0.56–0.73] | 0.339 [0.28–0.41] |
| all (123) | BM25+ rg | 48/123 [0.31–0.48] | 74/123 [0.51–0.68] | 96/123 [0.70–0.84] | 0.522 [0.45–0.59] |
| devset-v2 (62) | Explore, main a520b0ca | 22/62 [0.25–0.48] | 32/62 [0.39–0.64] | 47/62 [0.64–0.85] | 0.473 [0.38–0.58] |
| devset-v2 (62) | naive rg | 2/62 [0.01–0.11] | 7/62 [0.06–0.21] | 22/62 [0.25–0.48] | 0.133 [0.09–0.18] |
| devset-v2 (62) | BM25 rg | 13/62 [0.13–0.33] | 29/62 [0.35–0.59] | 42/62 [0.55–0.78] | 0.371 [0.28–0.46] |
| devset-v2 (62) | BM25+ rg | 25/62 [0.29–0.53] | 39/62 [0.51–0.74] | 51/62 [0.71–0.90] | 0.544 [0.44–0.64] |
| devset-v1 (36) | Explore, main a520b0ca | 9/36 [0.14–0.41] | 18/36 [0.34–0.66] | 23/36 [0.48–0.78] | 0.390 [0.27–0.53] |
| devset-v1 (36) | naive rg | 0/36 [0.00–0.10] | 2/36 [0.01–0.18] | 7/36 [0.10–0.35] | 0.052 [0.02–0.09] |
| devset-v1 (36) | BM25 rg | 7/36 [0.10–0.35] | 12/36 [0.20–0.50] | 23/36 [0.48–0.78] | 0.310 [0.20–0.43] |
| devset-v1 (36) | BM25+ rg | 12/36 [0.20–0.50] | 21/36 [0.42–0.73] | 26/36 [0.56–0.84] | 0.479 [0.35–0.61] |
| spent (25) | Explore, main a520b0ca | 11/25 [0.27–0.63] | 16/25 [0.45–0.80] | 19/25 [0.57–0.89] | 0.548 [0.38–0.71] |
| spent (25) | naive rg | 2/25 [0.02–0.25] | 3/25 [0.04–0.30] | 5/25 [0.09–0.39] | 0.144 [0.05–0.27] |
| spent (25) | BM25 rg | 4/25 [0.06–0.35] | 9/25 [0.20–0.56] | 15/25 [0.41–0.77] | 0.303 [0.18–0.45] |
| spent (25) | BM25+ rg | 11/25 [0.27–0.63] | 14/25 [0.37–0.73] | 19/25 [0.57–0.89] | 0.531 [0.36–0.71] |

Primary answers only, all 123: path R@1/3/8 and MRR are Explore 40/59/82
and 0.429, BM25+ 46/66/85 and 0.479; span are Explore 28/40/55 and 0.294,
BM25+ 27/39/51 and 0.280. The `.md` result file has every cut with
intervals.

**Read this first.** Over all 123, Explore and BM25+ are level on span
overlap (R@1 29 against 29, R@3 45 against 44, MRR 0.319 against 0.310) and
BM25+ is still ahead on path (R@3 74 against 66, MRR 0.522 against 0.464).
Paired (`--compare … --compare-methods bm25plus,explore`), none of those
differences is significant: the closest is path MRR, −0.058 (CI −0.126 to
+0.007, p = 0.076). Against plain BM25, Explore is ahead on every path and
span cut, significantly (p < 0.05) on path R@1, R@3, R@5 and MRR (+0.125,
CI +0.06 to +0.19) and on span R@1, R@5, R@8 and MRR (+0.110). BM25+
remains the bar to beat.

**Installed 0.8.3, devset-v2 only (reference).**
`results/2026-09-24-installed-0.8.3-devset-v2.{json,md}` ran the released
0.8.3 binary (`build_commit=e36098d5`), Explore only, on the 62 devset-v2
questions: span R@1/3/8 13/20/31 and MRR 0.298, path 19/30/46 and 0.443,
against main's 14/21/32 and 0.311, and 22/32/47 and 0.473.

Answer-rule precision on main: the rule said `safe` on 3 of 123 questions
(all `exact_symbol_definition`), and the top hit was correct on 1 (0.33, CI
0.06–0.79). Promotion should stay off.

Repo-write audit: in the Aethyme-enhanced repos, Explore still touches
`.aethyme/broker.db*` and `.aethyme/logs/command-metrics.jsonl` (a tripwire,
not an attribution, as before). No tracked file changed. See `repo_writes`.

## Superseded baseline (2026-09-24, 61 questions)

**Superseded** by the 123-question baseline above; kept for history.
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
