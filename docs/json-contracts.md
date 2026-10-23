# Aethyme Broker — Stable `--json` Command Outputs (v1)

Version: **1** — **FROZEN** (2026-07-17). Companion to
[events-contract.md](events-contract.md), which covers the event stream
itself; this file covers command outputs.

Seven command outputs are **stable v1 surfaces**. Scripts and integrations
may depend on their field names:

| Surface | Command | Shape (source of truth) |
|---|---|---|
| Status | `aethyme broker status --json` | `StatusView` (`src/broker.rs`) |
| Integration status | `aethyme broker advanced integration status --json` | `IntegrationStatusView` (`src/broker.rs`) |
| Events | `aethyme broker advanced events --json` | `Event` rows (`src/types.rs`), NDJSON |
| Metrics | `aethyme broker advanced metrics --json` | inline object (`src/cli.rs`) |
| Insights | `aethyme broker advanced insights --json` | `InsightsReport` (`src/insights.rs`) — see [insights-contract.md](insights-contract.md) |
| Submit outcome | `aethyme broker submit --json` | `SubmitOutcome` (`src/merge.rs`) |
| Report list | `aethyme broker advanced report list --json` | `ReportList` (`src/report.rs`) |
| Report show | `aethyme broker advanced report show <filename> --json` | `ReportInspection` (`src/report.rs`) |

Every other `--json` output (doctor, certify, quick-test, verify-loop,
agents, adopt, leases, gates, `pr check`, ...) is best-effort: useful, but not
yet frozen — do not build long-lived integrations on those without promoting
them here first. `quick-test` and `verify-loop` are public operator confidence
commands; their human-facing behavior is product surface, but their JSON shape
is still provisional.

## Change policy

Same discipline as the event stream: **additive only**. New fields may
appear at any time (consumers must ignore unknown fields); existing
fields are never renamed, removed, or re-typed without a versioned
break announced in this file. Enum-valued fields (`status`, `origin`,
`kind`) may gain new values — consumers must tolerate unknown values.
Object key order and `null`-vs-present for nullable fields are **not**
part of the contract.

## Error output (`aethyme.broker-error/v1`, introduced 2026-10-20)

When a broker command requested JSON with `--json` or the compatibility alias
`--json-output` fails before producing a report, it writes one JSON object to
stdout with the versioned shape below. The `code` is the command's numeric exit
code; `message` is human-readable and may vary between releases.

```json
{"schema":"aethyme.broker-error/v1","error":{"code":3,"message":"the request was refused"}}
```

When a command has already emitted a JSON report and then fails, the report
remains the only JSON document on stdout. The broker writes the failure
diagnostic to stderr; it includes any required recovery handoff, and the
process exits with the command's nonzero code. A blocked `finish --json` keeps
its existing report fields, adds an `error` object with `code` and `message`,
and exits with the matching nonzero code. If writing to stdout fails after
output begins, the broker does not append a second error document; it writes
the diagnostic to stderr and preserves the command's nonzero exit code (or
returns the environment-failure code for an otherwise successful command).
The stdout bytes may be incomplete if that write failed. Arguments after `--`
belong to a child and do not enable broker JSON mode. Commands that
deliberately stream raw output retain their documented output format.

## Field inventory (v1)

### `status --json`

```
{
  "agents": [ Session + { "activity_at", "derived_status", "pid_alive" } ],
  "overlaps": [ { "session_a", "session_b", "path" } ],
  "overlap_pairs": [
    { "session_a", "session_b", "severity": "low"|"high", "paths_count",
      "conflicting_paths", "sample_paths", "classified", "reason" }
  ],
  "promoted_conflicts": [
    { "session_id", "path", "session_path", "promoted_path" }
  ],
  "queue": [ MergeQueueEntry ],
  "integration_branch": "...",
  "integration_head": "<commit>",
  "review_refusals": [
    { "repository", "pull_request", "review_type", "head_commit",
      "class", "text", "refused_at" }
  ],
  "blockers": [
    { "id", "kind", "scope", "cause", "session_id", "clear",
      "safe_to_clear_automatically" }
  ],
  "blocker_sources_unavailable": [ { "source", "error" } ]
}
```

`blockers` (added 2026-09-23) is every current blocker across `broker.db`,
the host operation and resource ledgers, gate pidfiles, and conflict notices,
in one id namespace: `op:<n>`, `hostop:<32-hex>`, `resource:<lease-id>`,
`lease:<id>`, `gatecache:<gate>@<tree>`, `pidfile:<session>-<gate>`,
`action:<session>`. `kind` is `operation`, `host_operation`,
`resource_lease`, `path_lease`, `gate_cache`, `pidfile` or
`action_required`; `scope` is `repo` or `host`; `session_id` is omitted when
no session owns the blocker. `clear` is the exact command that clears it,
usually `aethyme broker unblock <id>` plus any flag an operator must supply.
`blocker_sources_unavailable` is omitted when every store was read; when
present, `blockers` is incomplete, never "nothing blocks". The same report is
`aethyme broker unblock --json`.

`ownership_claims` (added 2026-10-03) lists the named operations live
sessions declared they are driving (`aethyme broker advanced ownership claim
<name>`): `id`, `name`, `session_id`, `purpose`, `claimed_at`,
`taken_over_from`, `holder_status`, `holder_agent`, `holder_short_name`,
`last_active_at`, `last_operation_at`, `last_operation_reason` and `working`
(the holder is active, or ran a coordinated operation within the idle window;
only then is a competing claim refused). Each claim also appears in `advice`
as `ownership.claimed`, so `status --summary` shows it.

`cleanup_retention.closed_worktrees` (introduced 2026-09-27) counts closed
sessions whose checkout is still on disk, which a state-only
`aethyme broker finish close` leaves behind: `count` (broker-created
checkouts), `estimated_bytes` (recorded sizes, never a fresh walk, so a floor
while `unmeasured_count` is non-zero), `unmeasured_count`, `adopted_count`
(closed adopted checkouts, which GC never removes) and `command` (the command
that shows which of them GC would reclaim, or `null` when there are none).
`aethyme broker status doctor --json` carries the same object as
`retention.closed_worktrees`, and `aethyme broker gc plan --json` as
`closed_worktrees`.

`cleanup_retention.host_available_bytes` and `cleanup_retention.host_volume_probe` (introduced 2026-10-01) report free space where this repository's gates run -- the lower of the broker worktree root and host state, each read at its nearest existing directory -- and the directory read, or `null` when it cannot be read. Below the 8 GiB a gate needs to start, `advice` carries a `host.gate-headroom` row with severity `blocked`, emitted whether or not the repository retains worktrees, with `aethyme broker gc plan` and `aethyme broker gc storage plan` as its commands.

`collaboration_fence` (introduced 2026-10-10, #660) is present only when this
repository's committed config requires collaboration capture:
`{"min_compatible_schema": 50, "reason": "collaboration capture required",
"source": "committed", "state": "active" | "pending"}`. `active`: the
`broker.db` floor is raised and binaries older than that schema cannot open the
repository. `pending`: required but not yet raised, because this status ran on
a read-only snapshot or the write failed. The advice row
`collaboration.fence-pending` then names the next action. The floor is never
lowered, so an `active` fence stays after `required` is turned off.

`in_flight_submits` (introduced 2026-10-02) lists the `broker submit` runs in
progress in this repository, oldest first, and is omitted when none is
running. Each has `session_id`, `pid`, `phase` (`starting`, `auditing lease
ownership`, `planning the submission`, `simulating the merge`, `waiting for a
verification slot`, `verifying the merged tree`, `recording the verdict`; may
gain values), `elapsed_ms`, `phase_elapsed_ms`, `last_progress` (the last gate
or wait line), `last_progress_age_ms`, `alive` (the submitting process still
exists), `possibly_stalled` (gone, or no progress for `stall_after_ms`,
currently five minutes) and, while waiting for a verification slot,
`position` (`position`, `waiting`, `holders[]` with `session_id`,
`held_for_ms`, `last_progress`). A submit that is only queued keeps reporting
and is not stalled. A possibly stalled submit also gets a
`submit.possibly-stalled` advice row (`warning`). Records live in
`.aethyme/run/submits/` and a finished submit removes its own.

`waiters` (introduced 2026-10-08, #494) lists the processes blocked right now
on a contested broker lock or lease, longest wait first, in both
`status --json` and `status --summary --json`; it is omitted when nobody
waits. Each has `pid`, optional `session_id`, `kind`
(`coordinated_write_lock`, `gate_owner_lock` or `lease`; may gain values),
`resource` (the repository, the gate and its lock file, or the lease path),
`holder` (the holder as last observed, with its age), `waited_ms` and `alive`
(false when the waiting process is gone without removing its record).
Records live in `.aethyme/run/waits/`; a wait writes one when it first meets
contention and removes it when it ends. A record whose process is gone stays
listed until the next wait starts, which removes it; `status` never deletes.

`review_refusals` lists reviews a provider declined and nothing has re-asked
for since. `class` is `quota_exhausted`, `rate_limited`, `provider_error` or
`unknown`, and answers whether waiting helps; `text` is the provider's own
words, kept beside the classification rather than replaced by it, because the
refusal surface is scraped from prose and a misfire must cost precision and
not evidence. `unknown` is an ordinary value, not a defect.

`Session` fields: `id`, `worktree_path`, `branch`, `origin`, `status`,
`task`, `diff_base`, `pid`, `command`, `log_path`, `exit_code`,
`created_at`, `updated_at`, `last_activity_at`, `repository_name`, `tab_name`,
`ai_provider`. The last three are nullable, human-facing host context fields;
they do not participate in ownership or liveness. Display `derived_status`
(liveness-adjusted), not raw `status`.

`MergeQueueEntry` fields: `id`, `session_id`, `head_commit`,
`base_commit`, `status`, `merged_tree`, `details_json`, `created_at`,
`updated_at`.

### `integration status --json`

```
{
  "branch": "aethyme/integration",
  "head": "<commit>",
  "main_head": "<commit>",
  "main_is_ancestor": true|false,
  "commits_ahead_main": n,
  "changed_files": [ "path", ... ],
  "promoted_entries": [
    {
      "queue_entry_id": n,
      "session_id": n,
      "branch": "...",
      "task": "...",
      "base_commit": "<commit>",
      "head_commit": "<commit>",
      "merge_commit": "<commit>",
      "files": [ "path", ... ]
    }
  ],
  "conflicts": [
    { "session_id", "path", "session_path", "promoted_path" }
  ],
  "next_action": { "summary": "...", "commands": [ "..." ] }
}
```

This is the focused promoted-but-unmerged view: only work present on the
local integration branch and absent from the main checkout is considered
pending. `conflicts` are scoped to that pending layer, not every change
between an old live session and current main.

### `events --json`

One JSON object per line (NDJSON): `id`, `schema_version`, `ts`, `kind`,
`session_id`, `payload_json`. Kinds and payload field names are the
event-stream contract — see [events-contract.md](events-contract.md).

### `worktrees --json`

`aethyme broker advanced worktrees --json` returns a read-only inventory of the
host's reported worktrees:

```
{
  "rows": [
    {
      "repository": "...",
      "path": "...",
      "branch": "...",
      "bytes": 0,
      "inodes": 0,
      "size": "measured",
      "size_measured_at_ms": 0,
      "idle_days": 0,
      "state": "recoverable",
      "live": false,
      "git": {
        "head": "<commit>",
        "detached": false,
        "locked": false,
        "lock_reason": "...",
        "prunable": false,
        "prunable_reason": "..."
      },
      "git_registered": true,
      "git_error": "..."
    }
  ],
  "total_bytes": 0,
  "total_inodes": 0,
  "unique_work_bytes": 0,
  "unique_work_inodes": 0,
  "unique_work_count": 0,
  "size_scan": "bounded",
  "unmeasured_count": 0
}
```

Sizing is bounded by default (#559). A row's `size` says where `bytes` and
`inodes` came from: `measured` (walked by this report), `recorded` (the
repository's `.aethyme/worktree-sizes.json`; `size_measured_at_ms` gives its
age), or `unmeasured` (not walked within the 10 s budget; `bytes` and `inodes`
are 0 and mean nothing). While `unmeasured_count` is non-zero the totals are
floors. `size_scan` is `measure` under `--measure`, which walks every checkout
and never reports `unmeasured`. Work classification (`state`) always completes.

`branch`, `idle_days`, and optional strings inside `git` are omitted when
unknown. `state` is flattened into each row: `uncommitted` carries `files`,
`unpushed` carries `commits`, and the other values are `recoverable`,
`not_a_checkout`, and `prunable_registration`. `git` is `null` when no
registration details are available. `git_registered` is `true` or `false`
when inventory succeeded, and omitted when Git state could not be confirmed;
`git_error` explains an inventory failure. A prunable row may describe a path
that no longer exists and therefore has zero bytes. The report is diagnostic:
neither `prunable` nor any other row state authorizes deletion.

### `gc plan --json` and `gc apply --json`

Best-effort, not frozen, but additive under the change policy above. Since
#295 the plan covers this repository's managed gate cache, which lives under
the per-user cache directory (`<host cache>/gates/<repository key>/`) rather
than beside any worktree:

```
{
  ...,
  "age_anchor_ms": n,
  "gate_caches": [
    { "entry", "cache_key", "path", "estimated_bytes",
      "last_used_at_ms", "age_days", "reason" }
  ],
  "gate_cache": {
    "root", "repository_key", "budget_bytes",
    "total_bytes", "reclaimable_bytes", "held_bytes", "active_bytes",
    "include_active",
    "holders": [ "..." ],
    "entries": [
      { "entry", "cache_key", "path", "estimated_bytes", "last_used_at_ms",
        "age_days", "disposition", "reason" }
    ]
  },
  "estimated_build_output_reclaimable_bytes": n,
  "policy": { ..., "gate_cache_bytes_budget": n }
}
```

`gate_caches` are the candidates a reviewed `gc apply --confirm <digest>`
removes, interrupted rotations first and then least recently used first. The
active entry of each cache kind (the kind is the name without a trailing
`-v<N>`) is the one the next gate reuses. That is the key the repository's
gate configuration names, or, for a kind the configuration does not name, the
most recently used entry. It is kept whatever the budget, as disposition
`active`, unless the plan was made with `--include-active-gate-cache`. Such a
digest must be applied with the same flag, and `include_active` records it.
`gate_cache_bytes_budget` applies only to the older entries. Held and active
entries are outside the budget, so a running gate never pushes an idle entry
out of it.

Schema 4 plans add `age_anchor_ms`, the UTC-day floor of `evaluated_at`. It is
part of the authorization digest and supplies one stable reference for all
age-derived candidate fields and cutoff reasons. Exact `evaluated_at` remains
available for reporting; apply-time liveness checks use current time.

Gate cache candidates are part of the authorization digest, sizes and
`last_used_at_ms` included, so a gate that runs between plan and apply
invalidates the digest. A plan resumed from its journal has no fresh digest to
compare, so `gc apply` re-checks each entry's size and `last_used_at_ms`
against the plan under the entry's lease. It leaves an entry that changed in
place and reports it in `failures`. The unattended resume on broker open never
reclaims gate cache entries: the journal stays incomplete and
`recovery_action` names the `gc apply --confirm` that finishes it. The digest
omits `gate_caches` and `policy.gate_cache_bytes_budget` when there are no
gate cache candidates, so such a plan keeps the digest it had before gate
caches were inventoried.

`gate_cache` is reporting only and outside the digest. It is `null` when the
per-user cache directory cannot be resolved. `disposition` is one of
`reclaimable`, `held`, `active`, `within_budget`, or `unmeasured`, and consumers must
tolerate new values. An entry is `held`, and is never proposed, while a
non-released host resource lease names it (`aethyme-gate-cache:<repository
key>:<cache_key>`), while a gate pidfile names a live process, while a gate
owner lock is held, or when the lease registry is absent or cannot be read.
`gc apply` never creates a registry. `holders` lists
the repository-wide witnesses, which are pidfiles and owner locks. Other
repositories' gate caches are never inventoried. `estimated_bytes`,
`last_used_at_ms`, and `age_days` are omitted for an entry that was not
measured.

`estimated_build_output_reclaimable_bytes` is the sum of `artifacts[].
estimated_bytes` (build caches in finished sessions' worktrees) and
`gate_caches[].estimated_bytes`. It is already included in
`estimated_reclaimable_bytes`.

`gc apply --json` gains `gate_caches_reclaimed`, a list of removed paths. Its
`reclaimed_bytes` counts each gate cache entry by the size measured just
before it was removed. The `broker.gc.applied` event payload gains the
matching `gate_caches_reclaimed` count.

### `metrics --json`

```
{
  "gates_executed": [ { "gate", "runs", "total_ms" } ],
  "gate_cache_hits": n,
  "gate_time_saved_ms": n,
  "conflicts_caught_pre_gate": n,
  "overlaps_warned": n,
  "commands": [ { "command", "count", "total_ms" } ]
}
```

`overlaps_warned` counts `lease.overlap` events. Since 2026-09-30 one event
is one session pair starting to overlap or changing severity; earlier events
were one per overlapping path, so totals spanning that date mix both units.

### `submit --json`

```
{
  "entry": MergeQueueEntry,
  "submission_plan": {
    "session_id": n,
    "recorded_baseline": "<full commit>" | null,
    "session_head": "<full commit>",
    "integration_head": "<full commit>",
    "safe": true|false,
    "commits": [
      {
        "commit": "<full commit>",
        "parents": [ "<full commit>", ... ],
        "ownership": "session_owned" | "inherited_from_recorded_baseline" | "ambiguous",
        "integration_state": "pending" | "already_integrated_by_ancestry" |
          "already_integrated_by_stable_patch_identity" | "ambiguous",
        "patch_id": "<stable patch id>" | null,
        "matching_integration_commits": [ "<full commit>", ... ]
      }
    ],
    "warnings": [ "...", ... ]
  },
  "conflicts": [ "path", ... ],
  "conflict_details": [
    {
      "path": "path",
      "originating_commit": "<full session commit>",
      "ownership": "session_owned" | "inherited_from_recorded_baseline" | "ambiguous",
      "integration_side_commits": [ "<full commit>", ... ],
      "remediation": "...",
      "commands": [ "...", ... ]
    }
  ],
  "gate_outcomes": [ { "gate", "run_id", "status", "cached", "exit_code",
                       "duration_ms", "log_path", "load_avg_1m_start",
                       "load_avg_1m_end", "cpu_count",
                       "free_disk_bytes_start", "cache_provenance" } ],
  "no_changes": true|false,
  "promoted": true|false,
  "verified_against": {                      // omitted when not recorded
    "source": "upstream" | "integration",
    "reference": "origin/main" | "aethyme/integration",
    "commit": "<full commit>",
    "fallback_reason": "..."                 // omitted unless a verify-only
  }                                          // repository fell back to integration
}
```

`verified_against` (introduced 2026-10-01) names what the submission was
simulated and gated against. A `[promote] mode = "verify-only"` repository
verifies against the fetched default branch (`source: "upstream"`) and does not
touch the integration branch; a promoting repository keeps verifying against
integration. `submission_plan.integration_head` holds the same commit.

The four machine-environment fields in `gate_outcomes` (introduced
2026-09-26, schema 43) describe the machine an executed gate ran on:
`load_avg_1m_start` and `load_avg_1m_end` are the one-minute load average
(number) just before the command started and once it finished, `cpu_count`
is logical CPUs online (integer, to normalise load), and
`free_disk_bytes_start` is the free space (integer bytes) the disk-headroom
check measured on the gate's checkout filesystem. Each is `null` when the
platform could not report it; all four are `null` for a cache hit (the
reused verdict's machine state is not re-reported) and for a result recorded
before the command stage. `broker advanced gates run --json` and
`broker advanced gates pre-push --json` outcomes carry the same fields.

When a result row was stored, `run_id` identifies it; a cache hit keeps the
source run's ID. Cacheable outcomes include `cache_provenance`,
with the execution-profile digest and a declared scope. A missing digest means
profile capture was incomplete, so the executed result is not eligible for
cache reuse.

A gate outcome may also carry `"host_fault": true` (introduced 2026-10-01,
absent when false): the broker itself observed that the host, not the change,
stopped the gate -- host resources refused, the command never started, a host
resource error, the command ran out of disk, or the broker's deadline killed it
for the first time on that tree. When every gate that did not pass carries it,
`gate_verification.status` is `deferred`: the entry stays `submitted`
(`entry.details_json` gains `"deferred": true` and a per-gate `host_fault`),
nothing is promoted, and the command exits 6. Text in a gate's own log never
sets the flag.

`submission_plan` preserves deterministic commit order and separates ownership
from integration state. Full SHAs are never abbreviated in JSON. `conflicts`
non-empty means the submission was rejected pre-gate; `conflict_details`
provides provenance and recovery for the same paths. `promoted: true` means the
integration branch advanced in this call. `no_changes: true` means replay left
the integration tree unchanged: the queue entry is `superseded`, no gates ran,
and no promotion commit or ref movement occurred.

`collaboration_capture` (introduced 2026-10-09, #660, experimental) is present
only when the repository opts in with `[collaboration] capture = "advisory"`
or `"required"` in `.aethyme/config.toml`. It is the last field; every legacy
field keeps its name, value and order. Without the opt-in the output is
unchanged.

```
"collaboration_capture": {
  "schema": "aethyme.submit-capture/experimental-v0",
  "policy": "advisory" | "required" | "unsupported",
  "config_source": "committed" | "working_copy",
  "status": "acknowledged" | "incomplete" | "refused" | "failed" | "in_progress"
            | "not_configured",
  "operation_id": "submit:<40 hex>",       // omitted before inputs are known
  "base_commit": "<full commit>",          // merge base of the head and the verification base
  "result_commit": "<full commit>",        // session head
  "receipt": {                             // only when acknowledged
    "status": "retained_local",
    "durability": "local_durable" | "local_unverified",
    "contribution": "sha256:...",
    "base_snapshot": "sha256:...",
    "result_snapshot": "sha256:...",
    "retention": "until_released" | "until_ms:<ms>",
    "receipt_record": "sha256:..."
  },
  "code": "...", "detail": "<path-free>", "next_action": "..."   // when not acknowledged
}
```

An advisory capture never changes the legacy verdict, `promoted` or the exit
code. A required capture runs before anything is queued; when it is not
acknowledged, submit prints `{"submitted": false, "collaboration_capture": ...}`
and exits 3 with no queue entry, gate run or promotion. If the session head
moves after a required capture, submit refuses with `CapturedHeadMoved` (exit
3) before any queue entry and prints `{"submitted": false, "error": {"code":
"captured_head_moved", "message": "..."}, "collaboration_capture": ...}`, so a
receipt always names the submitted commit. An advisory receipt names the
`entry.head_commit` that was submitted. Under `required`, a promotion of a head
without an acknowledged required capture is refused with
`CaptureRequiredForPromotion` (exit 3). A `capture` value this
binary does not implement is reported with `policy: "unsupported"` and code
`unsupported_policy`, and treated as a required capture that failed.

### `report list --json`

```
{
  "schema_version": 1,
  "reports": [
    {
      "path": ".aethyme/reports/<filename>",
      "title": "...",
      "captured_at": 1234567890,
      "kind": "bug" | "improvement",
      "version": "<capturing Aethyme version>",
      "report_schema_version": 1,
      "digest": "<lowercase SHA-256 of exact current bytes>",
      "filing_state": "filed" | "unfiled"
    }
  ],
  "invalid": [
    { "path": ".aethyme/reports/<filename>", "error": "..." }
  ]
}
```

Valid reports are ordered by `captured_at` descending, then `path` ascending.
Invalid entries are ordered by `path`. A damaged artifact does not suppress
valid summaries. Paths are always repository-relative. Filing state is keyed
by the current digest; changing report bytes can only move an existing filed
artifact to `unfiled`, never silently retain filed state.

The local filing index is `.aethyme/reports/.filings.json`:

```
{ "schema_version": 1, "filings": { "<sha256>": {} } }
```

Filing-record objects are additive and reserved for the filing command's
provider metadata. Inventory readers determine state from map membership and
do not expose filing-record contents.

### `report show <filename> --json`

```
{
  "schema_version": 1,
  "summary": ReportSummary,
  "report": ReportDocument
}
```

`summary` has exactly the report-list summary fields above. `report` is the
parsed allowlist-only capture artifact (`schema_version`, `kind`, `title`,
`captured_at`, and `snapshot`). Invalid JSON, unsupported report/snapshot
schemas, symlinks, oversized artifacts, and paths outside
`.aethyme/reports/` fail closed.

### `collab <command> --json` (experimental)

Introduced 2026-10-10 (#680, experimental). `aethyme collab` is the opt-in
Local collaboration surface. Every subcommand prints exactly one JSON object
whose `schema` names its shape; the field set may change while the schema
says `experimental-v0`, and a consumer must check `schema` first.

| Command | `schema` |
|---|---|
| `collab status` | `aethyme.collab-status/experimental-v0` |
| `collab enroll` | `aethyme.collab-enroll/experimental-v0` |
| `collab project association propose` | `aethyme.collab-project-association-propose/experimental-v0` |
| `collab project association confirm` | `aethyme.collab-project-association-confirm/experimental-v0` |
| `collab project association revoke` | `aethyme.collab-project-association-revoke/experimental-v0` |
| `collab capture recover` | `aethyme.collab-capture-recover/experimental-v0` |
| `collab capture abort` | `aethyme.collab-capture-abort/experimental-v0` |
| `collab capture receipt` | `aethyme.collab-capture-receipt/experimental-v0` |
| `collab gc plan` | `aethyme.collab-gc-plan/experimental-v0` |
| `collab gc apply` | `aethyme.collab-gc-apply/experimental-v0` |
| `collab gc resume` | `aethyme.collab-gc-resume/experimental-v0` |
| `collab context` | `aethyme.collab-context/experimental-v0` |
| `collab brief attach` | `aethyme.collab-brief-attach/experimental-v0` |
| `collab resolution prepare` | `aethyme.collab-resolution-prepare/experimental-v0` |
| `collab resolution status` | `aethyme.collab-resolution-status/experimental-v0` |
| `collab resolution mark-dispatching` | `aethyme.collab-resolution-mark-dispatching/experimental-v0` |
| `collab resolution mark-unknown` | `aethyme.collab-resolution-mark-unknown/experimental-v0` |
| `collab resolution packet` | `aethyme.collab-resolution-packet/experimental-v0` |
| `collab resolution input` | `aethyme.collab-resolution-input/experimental-v0` |
| `collab resolution import` | `aethyme.collab-resolution-import/experimental-v0` |
| `collab resolution attach` | `aethyme.collab-resolution-attach/experimental-v0` |
| any refusal or failure | `aethyme.collab-error/experimental-v0` |

Project association proposals name two independently approved project
bindings and require distinct `ProjectId`s. `propose --write` creates the
canonical `.aethyme/project-association.json` proposal without changing either
store. The returned descriptor includes a required `proposal_nonce` field and
the `project-association-proposal-nonce-v1` capability in `requires`. Its
`sha256:<64 lowercase hex>` value is non-secret and gives each deliberate
proposal fresh canonical identity; older readers refuse the capability.
`confirm` requires the exact proposal `RecordId`, rechecks both
committed descriptors and both retained local bindings, then writes the
canonical object and confirmation row only to the invoking project's existing
store. The peer store is opened read-only. `revoke` changes only that local
row and does not open the peer store. These records grant no execution,
acceptance, or source permission authority. Schema 31 stores the local record
and its canonical object root; binaries that do not preserve association roots
cannot write the migrated store.

A revoked `RecordId` remains a tombstone. To associate the pair again, move or
remove the prior proposal file, create a fresh proposal, and confirm its new
exact `RecordId`; the old record remains revoked.

Resolution handoff commands are local and experimental. Every success object
states `dispatches_agent: false` and `grants_acceptance: false`. `prepare`
requires explicit strict JSON byte/entry limits and commits the attempt charge
before returning the metadata-only packet. Reusing its client key returns
`started: false` and no packet. `packet` can recover a still-exported packet;
it cannot replenish the allowance. `status` opens a read-only snapshot and
does not create state or migrate it. Session labels are provenance only.

`prepare` and `mark-dispatching` require `--target`, `--issuer`, `--wave`
and `--version`, acquire the appointed local controller, and check that the
exact request belongs to the current active wave at that target version.
Stale, terminal or stopped waves cannot prepare or mark new handoff intent.
Status, unknown-delivery bookkeeping and response import remain usable for
historical charged attempts; they grant no current-wave attachment authority.

`mark-dispatching` records handoff intent before an operator supplies the
packet to a local agent. It returns `status` and starts no process. A response
cannot be imported while the attempt is merely exported. `mark-unknown`
records uncertainty from a capped UTF-8 detail file, preserving the charge
and permitting a late bound response. Its `automatic_replay` is always false.
Both commands grant no dispatch or acceptance authority.

`input` returns one exact manifested snapshot/path as a `file` object with
`kind`, `bytes`, `sha256` and lowercase `bytes_hex`, or `null` for an absent
path. Text mode also prints versioned JSON, preserving arbitrary input bytes.
`import` accepts only a capped regular response file with a strict
`aethyme.resolution-attempt-response/experimental-v0` envelope bound to the
attempt, session label, request record/object and input-manifest digest.
Its `outcome` describes the resolution result; successful command execution
does not mean synthesis succeeded. Synthesized results require fresh
verification and are retained without wave attachment or acceptance.

`attach` requires `--attempt`, the exact request record/object, and current
`--target`, `--issuer`, `--wave`, `--version` authority. It links only a settled
synthesized result covering every selected wave contribution. Partial groups
remain held. The returned `wave_id`, `phase`, `candidate_snapshot`,
`candidate_commit` and exact `result` reference describe unverified source;
`attached_to_wave` and `requires_fresh_verification` are true. No gate receipt,
publication or acceptance is created. Repeating an identical attachment with
the original expected version is idempotent only while authority and wave
currentness still hold. Historical import alone grants no attachment authority.

A refusal or failure is printed on stdout in `--json` mode:

```
{
  "schema": "aethyme.collab-error/experimental-v0",
  "command": "gc plan",
  "code": "collaboration_disabled",
  "message": "collaboration is disabled for this repository: ...",
  "next_action": "run `aethyme collab enroll` to get the section that enables it, ..."
}
```

Exit codes: 0 done, 1 failed (I/O, database, Git), 2 usage, 3 refused
(collaboration disabled, a refused or unknown policy, a refused state root, or
a state that forbids the action). `next_action` is the safe next step and may
be `null` for a failure with no known remedy.

`collab status` reports `enabled`, `policy` (`off`, `advisory`, `required` or
`unsupported` with `policy_error.code`), `config_source`, and, when enabled,
`state` (`root`, `root_source`, `project_dir`, `initialized`, and once the
store exists `durability`, `receipt_label`, `schema_version`,
`min_compatible_schema`; or `refusal` with `code` and `next_action`),
`captures` (`by_state`, `reserved_bytes`, `attention`), `gc`
(`unfinished_generations`), `collaboration_fence` (as `broker status` reports
it: `state` `pending` or `active`, `source`, `min_compatible_schema`,
`reason`; or `null`), `collaboration_fence_error` (`code`, `message`) when the
fence cannot be read, `ignored_settings` when advisory capture ignores
unknown `[collaboration]` keys, `state.needs_migration`, `state.refusal` for a
store that cannot be read, and `next_actions`. It writes nothing: the store
and broker.db are read through read-only snapshots.

`collab context` wraps the `aethyme.contribution-context/experimental-v0`
record as `context`, with `served` (`fresh` or `cache`), `stored`,
`cache_key`, `context_id` and `absence_is_evidence`. Briefs inside it carry
`role: untrusted_data`. `collab gc plan` adds `recorded`: only a recorded
plan can be applied. `collab enroll` adds `path`, the file written with
`--write` (`null` otherwise).

`command` in the error object holds only subcommand verbs, never a path or
an argument value. Integrity failures (`corrupt_receipt`, `corrupt_object`,
`missing_object`, `failed`) exit 1; exit 3 means refused. On Windows, where
the broker is not yet supported, `collab --json` prints this error object
with `code` `unsupported_platform` and exits 3.

Under `capture = "advisory"`, `broker submit --json`'s `collaboration_capture`
gains `ignored_settings` (sorted) when the config carries `[collaboration]`
keys this binary does not implement; text output adds one warning line.
Under `required` such a key is refused as `unknown_setting`.


## Provisional push size report

Added 2026-10-20. The optional `pr_size` object in
`aethyme broker push --pr --json` remains a provisional output. Its existing
`files` and `changed_lines` fields count the review-policy eligible diff;
thresholds and `over_threshold` continue to use those counts.

Two additive fields explain the scope:

- `excluded`, omitted when empty, lists excluded paths with their `path` and
  `why` reason (`generated` or `excluded_path`; lockfiles use the latter).
- `diff_totals`, omitted when the complete local diff is unavailable, contains
  unfiltered `files` and `changed_lines` from the same Git diff. Paths are
  counted once. A binary file counts as a file and contributes zero lines.

The human warning labels the policy-filtered counts and prints the unfiltered
totals when exclusions explain a difference. These totals do not change review
classification or make an advisory threshold a gate.

### Guarded execution outcomes

`broker advanced exec --json` reports a command that has already run. Its
post-execution ownership audit does not undo filesystem changes. The existing
`exit_code` and `command_success` describe the child. In JSON mode, child stdout
is inherited on stderr alongside child stderr, leaving stdout for exactly one
completed report; output is neither buffered nor discarded. Text mode retains
inherited child stdout. The CLI exits 1 when either
the command or audit fails and retains the completed report on stdout.

Additive fields:

- `ownership_ok`: the actual ownership audit result, independent of the child.
- `command_timed_out`: always present as a boolean. `true` means supervised
  execution reached its command deadline and process-group cleanup was verified;
  normal child exits and legacy terminal execution report `false`. A verified
  timeout reports `exit_code: null` and `command_success: false`, then still
  runs the post-execution ownership audit. If cleanup cannot be verified, the
  command returns an unknown-outcome error instead of a report and skips audit.
- `conflicting_leases`: blocking leases retained from that audit, including the
  holder session and path. An edited file being dirty before execution does not
  itself imply an ownership violation.
- `outcome`: `passed`, `command_failed`, `ownership_failed`,
  `command_and_ownership_failed`, `timed_out`, or
  `timed_out_and_ownership_failed`. Timeout outcomes reflect verified command
  termination plus the independent ownership audit.
- `next_actions`: explicit lease claim commands for missing owned paths, or
  coordination guidance for conflicting leases and adoption-time foreign edits.
  Paths covered by a blocking or foreign directory receive coordination only.
  Claim commands shell-quote the path. Leading-dash paths receive coordination
  guidance rather than a command that the lease CLI cannot parse. Commands are
  suggestions; their normal broker authorization and conflict checks still apply.
  Foreign changes are preserved. Human output shows at most eight next actions;
  JSON retains the full list.

The existing `command_ran`, `edits_remain`, and `changed_paths_remaining` fields
remain available. Human output likewise states that the audit happens after the
command and prints recovery guidance. Arbitrary argv does not declare a write
set, so the guard cannot infer every generated path before running it.

### `broker advanced resources acquire --json` (stable conflict codes)

The acquire failure object keeps its existing fields: `code`, `request_id`,
`retryable`, `waited_ms`, and `conflicts`. Each conflict keeps `code`,
`resource_key`, `kind`, `requested`, `reason`, and optional `owning_lease`.
The following codes are stable automation values:

| Code | `retryable` | Meaning |
|---|---:|---|
| `resource_contention` | `true` | An active owner or occupied TCP range may release enough resources for the request. This includes capacity and TCP conflicts even when one recorded holder is known to be gone. |
| `capacity_policy_mismatch` | `false` | Existing leases use a different limit for the same capacity pool; waiting cannot change that policy. |
| `orphaned_holder` (introduced 2026-10-20, #876) | `false` | A requested exclusive key is still reserved by a lease whose recorded holder process is confirmed gone. The lease and key remain quarantined; the conflict identifies its lease and gives the generation-bound reconciliation command. Waiting does not release it. |

`orphaned_holder` is emitted for an exact exclusive-key conflict only. It does
not mean that every dead resource holder is unrecoverable: capacity and TCP
range conflicts remain `resource_contention` because other owners can still
make the request feasible. `resources explain --json` reports the matching
`wait` as `{"waitable": false, "reason": "orphaned_holder"}`. Reconciliation
is an explicit operator action and is required before the quarantined key can
be reused.
