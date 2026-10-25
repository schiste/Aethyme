# Broker latency budgets

Last Updated: 2026-10-25

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

By default, the runner creates a disposable, one-session synthetic Parcel
Playground (the checked-in multi-language fixture), isolated home, host state,
worktree root and local bare remote. This preserves the original four-command
timed workload. Pass `--sessions N` (1 through 64) to create a bounded
multi-session fixture; values above one automatically add full-status and doctor
diagnostics. Use `--status-diagnostics` to collect those diagnostics with one
session. Fake SSH serves the local remote and fake `gh` answers only PR listings.
The environment allowlist excludes inherited credentials, proxies and provider
settings. Expected Git/GitHub paths use local shims; this is not an OS network
sandbox, and unrelated egress is not instrumented. Fixture setup and initial
publication are outside timed samples. Every command has a 60-second timeout.
Complete CLI time uses a monotonic clock through process exit.

The Git transport is local; the runner does not claim an instrumented network
duration. Git transport, process startup and all
broker preflight/reporting remain charged to the budget: these conservative
numbers are **not pure broker CPU time**. The guarded child is `/usr/bin/true`;
its startup is retained rather than estimated and subtracted. No-op pushes do not
represent pushes with substantial new content. One-session timings do not prove
performance with many sessions or a large repository. Full status, doctor and
`Broker::open` phase timings are supplementary diagnostics without budgets; they
do not alter the four warning-budget actions. The runner retains complete command
stdout, stderr, exit codes and wall times, and records requested and successfully
created session/worktree counts in `workload.json`. Partial fixture setup remains
visible there if a later session creation fails.

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
four warning-budget measurements. The `diagnostics` object is empty by default.
With `--status-diagnostics` or `--sessions > 1`, it also contains
`status_full`, `doctor`, `status_open_phase_summary` and
`status_open_phase_full`; each entry has `samples_ms`, `median_ms`,
`budget_ms: null` and `verdict: "diagnostic_only"`. With nonzero
`--aged-released-resource-leases`, `diagnostics.resource_history_list` contains
the same fields. A schema-1-only consumer must recognize schema 2 before reading
the report.

The report embeds the same workload object saved as `workload.json`. It records
the requested session count, successful session/worktree creations and observed
active, idle and stale session states. It also records requested and actual aged
released counts, seeding status and, when enabled,
`aged_host_resource_history` with actual fixture lease counts and the seeding
method. `commands.json` retains setup and sample receipts, with the release
grant token redacted. These fields describe synthetic fixture state, not the
operator's repository or real production load.

## Lightweight checks for standalone Git config controls

The exact performance fast lane also accepts regular non-executable changes to
`git_worktree_config.py`, its `test_git_worktree_config.py` controls and the
worktree-config assessment, optionally with mirrored skill Markdown. Its bounded
Git Playgrounds verify three primary layouts, unknown-extension refusal, exact
rollback, a rejected modern-as-legacy claim and the command budget. These checks
run without building or invoking Aethyme; they do not qualify a legacy client,
change production Git settings or provide native runtime evidence.

The classifier and validator remain outside their own allowlist and are read
from the trusted PR base. Rust, manifests, locks, shared Git experiment helpers,
workflow changes outside the existing exact performance entry, deletions, mode
changes and unlisted paths still select the full suite. This extension must
itself land through full CI before later experiment-only PRs can use it.

## Contract-check build reuse

The PR contract check builds its native policy checker from the event's pinned
base commit before checking out the exact PR head. This matches the protected-main
Rust cache instead of recompiling changed product crates for every report edit.
The copied executable lives outside the checkout, is hashed before the head
switch, and is verified before execution. The comparison still uses the PR head,
its merge base, registry, body and commit decisions; product changes receive the
separate Rust lint and workspace checks. Both checkout steps disable persisted
credentials. This changes the checker policy source, not the compared source.

A cold or missing base cache can still require compilation. Record cache restore,
build step and complete job durations before claiming a timing gain; an unchanged
workflow or shared cache key alone is insufficient evidence. A body edit still
runs the required contract check. The workflow does not reuse an old PR verdict
or skip the exact head/body comparison.
