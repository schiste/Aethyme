# Broker latency budgets

Last Updated: 2026-10-21

The machine-readable source is
[`broker-budgets.json`](../../../aethyme-eval/benchmarks/performance/broker-budgets.json).
Budgets are **warnings only** while the broker is being optimized. Crossing a
budget must not refuse a user command or fail CI. Invalid measurements and command
failures remain errors; missing data must not look like a fast command.

| Action | Budget | Meaning |
|---|---:|---|
| Routine status | 500 ms | Complete `broker status --summary --json` process |
| Guard overhead | 500 ms | Complete guarded no-op process, including ownership audit |
| Unchanged sync | 1,000 ms | Complete unchanged sync process, local transport |
| Push | 500 ms | Complete push process, local transport; nightly profile uses an unchanged branch |

For real remote transport, the agreed targets allow separately measured network
time in addition to the sync and push budgets. The nightly profile below uses
local transport and compares the full wall time directly with these limits.

## Automatic reporting

The existing nightly/manual `product-performance.yml` workflow reuses its release
binary, then runs three samples of each broker action. No additional Cargo build
is introduced. Medians above the budget generate GitHub warning annotations.
The job summary displays all four medians, limits and verdicts; artifacts retain
raw command arguments, outputs, exit codes, individual timings, the budget digest
and executable digest for 14 days. The committed budget remains versioned in Git.

The runner creates a disposable, one-session synthetic Parcel Playground (the checked-in multi-language fixture), isolated home,
host state, worktree root and local bare remote. Fake SSH serves that remote and
fake `gh` answers only PR listings. The environment allowlist excludes inherited
credentials, proxies and provider settings. Expected Git/GitHub paths use local
shims; this is not an OS network sandbox, and unrelated egress is not instrumented. Fixture
setup and initial publication are outside timed samples. Every command has a
60-second timeout. Complete CLI time uses a monotonic clock through process exit.

The Git transport is local; the runner does not claim an instrumented network
duration. Git transport, process startup and all
broker preflight/reporting remain charged to the budget: these conservative
numbers are **not pure broker CPU time**. The guarded child is `/usr/bin/true`;
its startup is retained rather than estimated and subtracted. No-op pushes do not
represent pushes with substantial new content. One-session timings do not prove
performance with many sessions or a large repository.

For real network measurements, retain complete wall time and separately observed
network durations. Never subtract the whole coordinated-operation interval as
"network": it contains local work too. These budgets do not cover CI, coding,
approval or deployment time. Real-repository dogfooding measurements should be
reported separately from this reproducible Playground profile.

## Run without building

```sh
python3 -m unittest discover \
  -s packages/aethyme-eval/benchmarks/performance -p 'test_broker_latency.py'
python3 packages/aethyme-eval/benchmarks/performance/broker_latency.py \
  --binary /absolute/path/to/aethyme --output /tmp/unique-broker-timing --samples 3
```

The output directory must not already exist. Exit zero permits budget warnings;
an unsuccessful command or invalid sample is an error. Keep failed command
receipts when diagnosing setup or execution failures.
