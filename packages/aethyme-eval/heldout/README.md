# Held-out navigation eval

Twenty navigation questions a developer would ask about three real
repositories. Each has hand-verified ground truth. The runner scores
`aethyme explore` against a plain ripgrep baseline. This is recovery plan item
P3.1: the fixed yardstick for Phase 3's generic Explore work.

## Rules

1. **Held out.** The target repositories (`~/Repositories/SP42` (Rust),
   `~/Repositories/aedventure` (TypeScript, with some Rust),
   `~/Repositories/blybot` (Python)) have never been Aethyme playgrounds or
   tuning targets. Keep it that way: do not use them to develop, debug or
   demo Explore.
2. **Do not tune against it.** Cardinal rule 2 applies in full. Never change
   the engine, the ranking, the stopwords or the output shape because of a
   specific question here. If a failure shows a weakness, fix the generic
   mechanism, and ask yourself whether you would make that change if this
   eval did not exist. Do not read individual questions while designing a
   fix. Read the aggregate numbers and the failure patterns below instead.
3. **Answers come first.** Every question's ground truth (paths and line
   ranges) was written and verified by reading the code before any tool ran
   on it. New or rotated questions must follow the same order. Scoring is by
   path. The line ranges document the answer and allow finer scoring later.
4. **Rotate 5 per Phase 3 run.** Before each Phase 3 measurement, swap 5
   questions in `questions.jsonl` for the 5 in `questions-spare.jsonl`. Keep
   the per-repo balance close to 7/7/6 and the mix of kinds. Then write 5 new
   spares, answers first, from code you have not run Explore against. Record
   which ids rotated in the commit body. Rotation keeps the set from becoming
   an implicit target.
5. **The target repositories are read-only.** The runner only reads them.
   It snapshots `git status` and every `.aethyme/` file before and after the
   run, and records any difference under `repo_writes` in the results.

## Question kinds

`where_implemented`, `which_file_handles`, `config_read` (an env var or
setting), `callers` ("what calls X") and `where_change` ("where would I
change X"). Questions are phrased the way a developer would ask them, without
the answer's file name or symbol.

## Running it

```bash
cd packages/aethyme-eval/heldout
AETHYME_HELDOUT_RG=/opt/homebrew/bin/rg python3 -B run.py
# options: --only <id> (repeatable), --questions questions-spare.jsonl,
#          --aethyme <binary>, --date <YYYY-MM-DD>
```

The script needs Python 3.11 or later and uses only the standard library.
Set `AETHYME_HELDOUT_RG` when `rg` on `PATH` is a shim.

For each question the runner runs:

- **Explore:** `aethyme explore --repo <repo> --request "<question>" --format answer-json --depth 0`.
  The ranked list is the `answer[]` paths, then the `navigation_hints[]`
  paths, then each subsystem's `top_verification_targets[]` paths in rank
  order, with duplicates removed.
- **rg baseline:** the question's terms, lowercased, with generic stopwords
  and words under 3 characters removed. Each term runs as
  `rg -n -i -c -F <term>` from the repo root, which respects `.gitignore`.
  Files are ranked by the number of distinct terms they hit, then by total
  matches.

A top-k hit means any ground-truth path is among the first k paths.
`first_hit_rank` is also recorded, and wall time is measured for each method.
Output goes to `results/<date>.json` (full records, repo HEADs and the
`repo_writes` audit) and `results/<date>.md` (summary tables).

## Baseline (2026-09-24)

Build: `aethyme 0.8.1 (v0.8.1 build_commit=fa40ad5f)`. Target heads:
SP42 `9c4dea0c`, aedventure `59eba247`, blybot `a2403184`. None of the three
repos has a materialized graph, so every Explore call came back `degraded`
with `graph_store_missing`. That is the condition this baseline is meant to
record.

| Repo | N | Explore top-1 | Explore top-3 | rg top-1 | rg top-3 |
| --- | --- | --- | --- | --- | --- |
| SP42 | 7 | 1 | 1 | 0 | 1 |
| aedventure | 7 | 1 | 2 | 1 | 1 |
| blybot | 6 | 0 | 0 | 0 | 0 |
| **total** | 20 | **2** | **3** | **1** | **2** |

Median wall time was 0.015 s for Explore and 0.131 s for the rg baseline, which
runs all terms in sequence. Explore ranked a ground-truth file anywhere in its
list for 4 of 20 questions. rg ranked one in its top 25 for 13 of 20.

Failure patterns in the degraded path, stated generically:

- **The candidate set is truncated before ranking.** The source fallback
  reads at most 128 tracked files at 8 KiB each, and it reported
  `complete: false` on every question. SP42 has 512 tracked files and
  aedventure has 1,845, so most files can never become candidates. Even in
  blybot (175 files), any module longer than 8 KiB is only partly read.
- **At most 4 paths come back.** Each response held exactly 4 hints. An
  answer ranked fifth scores the same as no answer.
- **Prose and archived code outrank live code.** Design plans (`attic/`),
  specs (`docs/`) and a vendored legacy tree (`legacy/`) fill the top slots
  because they share the question's vocabulary.
- **The fallback drifts to path order when nothing matches.** Several
  blybot answers are `__init__.py`, `__main__.py` and `README.md`.
- **The baseline's own weakness:** ranking by distinct terms rewards long
  documents, so rg's top slots also go to specs and progress logs. It
  usually finds the answer, but typically at rank 9 to 25.

## Phase 3 exit bar

**Explore top-3 ≥ 14/20 with no graph, and better than the rg baseline in
agent turns.** This harness measures the first half. Its `first_hit_rank`
gives a rough proxy for the second: how many files an agent would open
before reaching the answer. Measure turns themselves with an agent-driven
run on the same questions.
