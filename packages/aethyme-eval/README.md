# Aethyme Eval

Internal regression sentinel for Aethyme evaluation cost. This package is
not a benchmark product. Its first job is to answer:

> Did recent Aethyme changes materially increase token use, cost, or
> runtime on fixed Playground evaluation tasks?

It compares new eval run records against a tracked historical baseline.
Quality is recorded and used to avoid false positives, but the package is
intentionally conservative about claims.

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
aethyme-eval compare ../aethyme/eval-runs/20260521T121950-grc-bug-fix-haiku \
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
- mark control-condition token growth separately as environment drift

When control drift is present, tool-condition token ratios are adjusted by
the control ratio before declaring a regression. This prevents model/runtime
drift from being misattributed to Aethyme.

Fresh evaluation inputs must be built from Playground repositories. The
package can read old self-run artifacts for auditability, but shipped
baselines and ongoing gates should not run against Aethyme itself.
