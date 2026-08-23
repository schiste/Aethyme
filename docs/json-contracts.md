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
  "integration_head": "<commit>"
}
```

`Session` fields: `id`, `worktree_path`, `branch`, `origin`, `status`,
`task`, `diff_base`, `pid`, `command`, `log_path`, `exit_code`,
`created_at`, `updated_at`, `last_activity_at`. Display `derived_status`
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
  "promoted": true|false
}
```

`submission_plan` preserves deterministic commit order and separates ownership
from integration state. Full SHAs are never abbreviated in JSON. `conflicts`
non-empty means the submission was rejected pre-gate; `conflict_details`
provides provenance and recovery for the same paths. `promoted: true` means the
integration branch advanced in this call.

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
