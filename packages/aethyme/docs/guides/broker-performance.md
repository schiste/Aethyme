# Broker latency budgets

Last Updated: 2026-10-22

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

## Opt-in released-resource history

Pass `--aged-released-resource-leases N` to the same runner to add between
0 and 50,000 aged released host-resource records. The default is zero. One
fixture-owned record is acquired and released through the public CLI; the
remaining records are inserted in a schema-checked SQLite transaction inside
the isolated Playground. This measures inventory history size without
pretending that thousands of live acquisitions ran. The runner verifies the
fixture repository identity, lease counts and database integrity before timing.
It does not seed the operator's host-resource store.

The four warning budgets remain unchanged. With a nonzero count, the runner also
measures the complete `broker advanced resources list --json` command as an
unbudgeted diagnostic. Released records remain excluded from that command's
normal output; the history still exercises its database read path. Setup and
seeding are outside the timing samples.

### Report schema 2

`report.json` now declares `schema_version: 2`. Existing `results` retain the
four warning-budget measurements. The added `diagnostics` object is empty when
history seeding is disabled; otherwise `diagnostics.resource_history_list`
contains `samples_ms`, `median_ms`, `budget_ms: null` and
`verdict: "diagnostic_only"`. A schema-1-only consumer must recognize schema 2
before reading the report.

The report embeds the same workload object saved as `workload.json`. It records
requested and actual aged released counts, seeding status and, when enabled,
`aged_host_resource_history` with actual fixture lease counts and the seeding
method. `commands.json` retains setup and sample receipts, with the release
grant token redacted. These fields describe synthetic fixture state, not the
operator's repository or real production load.
