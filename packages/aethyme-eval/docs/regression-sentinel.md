# Internal Regression Sentinel

This package is the standalone evaluation layer for Aethyme's own regression
checks. The first consumer is the Aethyme team: after generic system changes,
we need a fixed-workload signal that token use, cost, and runtime did not drift
materially.

## Scope

The sentinel compares current eval artifacts against a tracked baseline. It is
not a public benchmark, a leaderboard, or a reason to tune the engine toward an
eval task. It should be used as a smoke detector:

- Did total tokens rise?
- Did cost rise?
- Did wall time rise?
- Was the change explained by control-condition drift?
- Was there enough quality improvement to treat the increase as a trade-off?

## Inputs

The parser accepts surviving artifacts from the previous harness:

- historical `runs.jsonl`
- a direct `complete-result.json`
- one run directory containing `complete-result.json`
- a directory of run directories

New runs must use Playground repositories. Historical self-runs can be read for
audit purposes, but tracked baselines exclude the `aethyme` target by default.

## Baseline Shape

Baselines are grouped by exact comparable identity:

```text
model + target + eval_type + scenario + condition
```

This intentionally avoids comparing a new MediaWiki bug-fix run against an old
GRC dead-code run just because both used the same condition.

## Regression Policy

The default policy is conservative:

- warn at `1.25x` tokens or cost
- fail at `1.50x` tokens without a quality gain
- fail at `1.75x` cost without a quality gain
- warn at `1.50x` duration
- fail at `2.00x` duration without a quality gain
- mark control token drift separately as environment drift
- fail the gate when a current row has no exact baseline

For tool conditions, if the matching control condition drifted, the sentinel
divides the tool token ratio by the control token ratio before deciding whether
to fail. The report still shows the raw ratio.

## Local Workflow

Rebuild the tracked baseline from historical data:

```bash
aethyme-eval summarize-history \
  --runs-jsonl ../aethyme/eval-runs/runs.jsonl \
  --output baselines/haiku-2026-05.json \
  --model haiku \
  --methodology-hash 4bc594383610
```

Compare a new run:

```bash
aethyme-eval compare ../aethyme/eval-runs/<run-dir> --fail-on-regression
```

The command exits with code `2` when any row fails, when any row is missing a
baseline, or when the input contains no comparable rows.
