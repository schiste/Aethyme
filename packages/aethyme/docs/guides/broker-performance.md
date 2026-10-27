# Broker latency budgets

Last Updated: 2026-10-27

The machine-readable source is
[`broker-budgets.json`](../../../aethyme-eval/benchmarks/performance/broker-budgets.json).
Budgets are **warnings only** while the broker is being optimized. Crossing a
budget must not refuse a user command or fail CI. Invalid measurements and command
failures remain errors; missing data must not look like a fast command.

| Action | Budget | Meaning |
|---|---:|---|
| Routine status | 150 ms | Complete `broker status --summary --json` process |
| Guard overhead | 500 ms | Complete guarded no-op process, including ownership audit |
| Unchanged sync | 1,000 ms | Complete unchanged sync process, local transport |
| Push | 500 ms | Complete push process, local transport; nightly profile uses an unchanged branch |

For real remote transport, the agreed targets allow separately measured network
time in addition to the sync and push budgets. The nightly profile below uses
local transport and compares the full wall time directly with these limits.

## Routine observation

Default compact text, `status --compact --json` and `status --summary --json`
observe current coordination state without fast-forwarding the main checkout,
running startup recovery, resuming GC, or persisting liveness transitions.
PID checks, activity metadata, pending operations, locks and source-unavailable
warnings remain current observations. Deferred checks are unknown, not clean;
`startup_recovery`, `checkout_sync` and `gc_resume` are explicitly listed.
Full `status --json` and explicit `status --refresh` retain their existing paths.

Repository identity and committed policy reads are shared only during one routine
invocation, including router compatibility preflight. Nothing is reused across
invocations. Routine observation does not create broker state or record outcome
telemetry. `AETHYME_MEASURE_OUTPUT=1` retains the existing explicit opt-in for a
metrics line. A pending committed capture fence remains visible as a warning;
the ordinary mutation/audit paths continue to enforce or raise it.

The 150 ms status target is warning-only. Deterministic entrypoint controls
ratchet Git process count and observation behavior without timing assertions.
Measure complete uninstrumented process wall time separately, including router
preflight and exit work; `phase_timings_ms.command_total` covers only its handler.

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

## Paired broker action matrix

`broker-action-matrix-v2.json` defines a separate diagnostic profile. It keeps
the schema-v1 absolute warning budgets in `broker-budgets.json` unchanged and
records that file's digest in each run. The scenario inventory is versioned in
`broker-action-scenarios-v1.json`.

The matrix requires the signed v0.8.41 CLI and sibling engine pair as its
baseline, verifies both hashes against the retained v0.8.41 rollout receipt,
and runs the candidate CLI and sibling engine as the comparison pair. The old
rollout timings are recorded as context only: they were not measured as paired
runs on the same controlled fixture. The default is five repetitions with
baseline/candidate order alternating. It compares medians and emits a warning
only when the candidate increase exceeds the larger of 100 ms or 10% of the
baseline median. The candidate's routine status median is also checked against
the existing v1 absolute warning budget. None of these comparisons refuses a
command or fails CI.

Each repetition gets a fresh synthetic repository, local bare remote and six
broker sessions. The v1 routine-status comparison is measured immediately after
creating the first session, before the other five are added. The matrix also
measures session start, lease plan, claim and release, exact overlap detail,
an independently completing commit while another session waits in a
pre-commit hook, a constrained local fetch and its read-only operation recovery
view, generator preview, status with
1,000 aged released resource rows, and session finish. The generator preview
records source/ref identity, plan digest, ownership sections, full-diff hash
and byte count, UTF-8 completeness and truncation notice. Older versions that
can return inventory without generated content are reported as such rather
than called unsupported.

Every binary role and repetition gets a separate fixture-owned host-state
directory and resource database. The baseline and candidate never share a
ledger, so versioned storage compatibility floors are measured independently;
the report records each isolated state root and its final inventory identity.

The unprovable-fetch outcome remains explicitly unsupported: the harness does
not fabricate an operation record or claim a provider-exit result it cannot
prove. A separate bounded-output control checks that the runner refuses an
oversized child output and confirms its fixture tree is unchanged. Grant
tokens used while seeding history are redacted from retained output before
receipt persistence; command hashes and byte counts describe the original
captured bytes.

Run it without compiling Aethyme, using an already-installed signed baseline
pair, candidate pair and retained rollout receipt:

```sh
python3 packages/aethyme-eval/benchmarks/performance/broker_action_matrix.py \
  --baseline-cli /absolute/path/to/v0.8.41/bin/aethyme \
  --candidate-cli /absolute/path/to/candidate/bin/aethyme \
  --v41-live-reference /absolute/path/to/aethyme-v0.8.41-rollout-report.json \
  --git /absolute/path/to/git \
  --output /tmp/unique-broker-action-matrix
```

The engine executable must be adjacent to each CLI as `aethyme-engine-cli`.
`--repetitions` can select 3 through 15 samples; otherwise the versioned
five-sample default is used. The new absolute output directory must be outside
the source checkout. It retains the complete bounded command receipts, sample
workload identities, binary and helper hashes, scenario digests and report.
Treat the output as local diagnostic data because commands, output and local
paths are retained. The local SSH shim serves only the fixture's bare remote,
but the runner does not claim an OS-level network sandbox or instrument other
egress. An incomplete run retains its partial report and receipts and exits
unsuccessfully; unsupported scenarios are never converted into zero timings.

## Distinguish dependency cache hits from workspace reuse

The pinned `Swatinem/rust-cache` action defaults `cache-workspace-crates` to
false. The current main warmer and read-only PR consumers do not override it.
A shared-key cache hit therefore does not establish that Aethyme workspace
crates or its CLI executable were restored. Inspect the exact action version's
input defaults, effective producer/consumer settings and hosted build logs.

A synthetic workflow trial compiled the checker from the pinned default base,
then ran it against the exact PR head and body. Its hosted cache hit restored
about 253 MiB, but the build still took 70 seconds; the policy execution took
about four seconds. This is one diagnostic observation, not a controlled speed
comparison. The proposed two-checkout workflow was withdrawn rather than
claiming a timing benefit. The current workflow still builds its checker from
the PR source and does not reuse a cached verdict.

Before changing cache policy, measure archive size, restore/compile/upload time,
cache writer trust and bounded retention. Workspace artifact caching can trade
compilation cost for much larger archives; enabling it is not established as an
improvement by a dependency-cache hit. A future lightweight checker should share
the existing policy implementation and qualify both verdict parity and complete
job timing before replacing the current path.
