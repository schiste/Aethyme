# Aethyme Broker — Stable `--json` Command Outputs (v1)

Version: **1** — **FROZEN** (2026-07-17). Companion to
[events-contract.md](events-contract.md), which covers the event stream
itself; this file covers command outputs.

Seven command outputs are **stable v1 surfaces**. Scripts and integrations
may depend on their field names:

| Surface | Command | Shape (source of truth) |
|---|---|---|
| Status | `aethyme broker status --json` | `StatusView` (`src/broker.rs`) |
| Integration status | `aethyme broker integration status --json` | `IntegrationStatusView` (`src/broker.rs`) |
| Events | `aethyme broker events --json` | `Event` rows (`src/types.rs`), NDJSON |
| Metrics | `aethyme broker metrics --json` | inline object (`src/cli.rs`) |
| Submit outcome | `aethyme broker submit --json` | `SubmitOutcome` (`src/merge.rs`) |
| Report list | `aethyme broker report list --json` | `ReportList` (`src/report.rs`) |
| Report show | `aethyme broker report show <filename> --json` | `ReportInspection` (`src/report.rs`) |

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

## Field inventory (v1)

### `status --json`

```
{
  "agents": [ Session + { "activity_at", "derived_status", "pid_alive" } ],
  "overlaps": [ { "session_a", "session_b", "path" } ],
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
`aethyme broker blockers --json`.

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

`aethyme broker worktrees --json` returns a read-only inventory of the
host's reported worktrees:

```
{
  "rows": [
    {
      "repository": "...",
      "path": "...",
      "branch": "...",
      "bytes": 0,
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
  "unique_work_bytes": 0,
  "unique_work_count": 0
}
```

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
  "gate_caches": [
    { "entry", "cache_key", "path", "estimated_bytes",
      "last_used_at_ms", "age_days", "reason" }
  ],
  "gate_cache": {
    "root", "repository_key", "budget_bytes",
    "total_bytes", "reclaimable_bytes", "held_bytes",
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
removes, interrupted rotations first and then least recently used first. They
are part of the authorization digest, sizes and `last_used_at_ms` included, so
a gate that runs between plan and apply invalidates the digest. The digest
omits the field when it is empty, so plans with no gate cache candidates keep
their earlier digests.

`gate_cache` is reporting only and outside the digest. It is `null` when the
per-user cache directory cannot be resolved. `disposition` is one of
`reclaimable`, `held`, `within_budget`, or `unmeasured`, and consumers must
tolerate new values. An entry is `held`, and is never proposed, while a
non-released host resource lease names it (`aethyme-gate-cache:<repository
key>:<cache_key>`), while a gate pidfile names a live process, while a gate
owner lock is held, or when the lease registry cannot be read. `holders` lists
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
  "gate_outcomes": [ { "gate", "status", "cached", "exit_code",
                       "duration_ms", "log_path" } ],
  "no_changes": true|false,
  "promoted": true|false
}
```

`submission_plan` preserves deterministic commit order and separates ownership
from integration state. Full SHAs are never abbreviated in JSON. `conflicts`
non-empty means the submission was rejected pre-gate; `conflict_details`
provides provenance and recovery for the same paths. `promoted: true` means the
integration branch advanced in this call. `no_changes: true` means replay left
the integration tree unchanged: the queue entry is `superseded`, no gates ran,
and no promotion commit or ref movement occurred.

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
