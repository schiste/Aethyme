# Aethyme Eval

Internal regression sentinel for Aethyme evaluation cost. This package is
not a benchmark product. Its first job is to answer:

> Did recent Aethyme changes materially increase token use, cost, or
> runtime on fixed Playground evaluation tasks?

It compares new eval run records against a tracked historical baseline.
Quality is recorded and used to avoid false positives, but the package is
intentionally conservative about claims.

## Layout

- `src/aethyme_eval/` — the regression sentinel library and its
  `aethyme-eval` CLI.
- `scripts/run_codex_eval.py` — the headless Codex playground eval
  runner. Reads `AETHYME_EVAL_*` env vars, runs one arm, emits runner
  JSON on stdout.
- `scripts/check_regression_gate.py` — compares a Control/Aethyme result
  pair against the stable regression metrics.

Both scripts moved here from `packages/aethyme/scripts/eval/` in
python-retirement Phase 7 (2026-08-06), along with
`tests/test_codex_eval_runner_contract.py`. They are eval tooling, this
package owns eval tooling, and it stays Python by operator decision —
an arm's-length acceptance check should not share the measured system's
toolchain. `packages/aethyme` is now 100% Rust and carries no Python at
all, so leaving them there was not an option either.

They measure `packages/aethyme` from outside: `TOOL_PACKAGE_ROOT`
resolves to the sibling package, and `MONOREPO_ROOT` backs cardinal
rule 1 — an eval target inside this checkout is Aethyme itself and is
refused.

## Commands

Generate a baseline from historical run JSONL:

```bash
aethyme-eval summarize-history \
  --runs-jsonl ../aethyme/eval-runs/runs.jsonl \
  --output baselines/haiku-2026-05.json \
  --model haiku \
  --methodology-hash 4bc594383610
```

Baseline generation excludes the `aethyme` target by default. Use
`--include-self-targets` only for one-off historical audits, not for the
tracked regression sentinel baseline.

Compare new run records against the default baseline:

```bash
aethyme-eval compare ../aethyme/eval-runs/<run-dir> \
  --format markdown \
  --fail-on-regression
```

Accepted result inputs:

- historical `runs.jsonl`
- a single run directory containing `complete-result.json`
- a directory containing many child run directories
- a direct `complete-result.json` file

## Regression Policy

Defaults are intentionally broad:

- warn if tokens rise by more than 25%
- fail if tokens rise by more than 50% without at least a 5 point quality gain
- warn if duration rises by more than 50%
- fail if duration rises by more than 100% without at least a 5 point quality gain
- mark control-condition token growth separately as environment drift
- fail the gate when a current result has no exact baseline

When control drift is present, tool-condition token ratios are adjusted by
the control ratio before declaring a regression. This prevents model/runtime
drift from being misattributed to Aethyme.

Fresh evaluation inputs must be built from Playground repositories. The
package can read old self-run artifacts for auditability, but shipped
baselines and ongoing gates should not run against Aethyme itself.

With `--fail-on-regression`, the command exits with code `2` when any row
fails, when any row is missing a baseline, or when the input contains no
comparable result rows.
