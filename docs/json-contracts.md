# Aethyme Broker — Stable `--json` Command Outputs (v1)

Version: **1** — **FROZEN** (2026-07-17). Companion to
[events-contract.md](events-contract.md), which covers the event stream
itself; this file covers command outputs.

Four command outputs are **stable v1 surfaces**. Scripts and integrations
may depend on their field names:

| Surface | Command | Shape (source of truth) |
|---|---|---|
| Status | `aethyme broker status --json` | `StatusView` (`src/broker.rs`) |
| Events | `aethyme broker events --json` | `Event` rows (`src/types.rs`), NDJSON |
| Metrics | `aethyme broker metrics --json` | inline object (`src/cli.rs`) |
| Submit outcome | `aethyme broker submit --json` | `SubmitOutcome` (`src/merge.rs`) |

Every other `--json` output (doctor, certify, agents, adopt, leases,
gates, …) is best-effort: useful, but not yet frozen — do not build
long-lived integrations on those without promoting them here first.

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
  "conflicts": [ "path", ... ],
  "gate_outcomes": [ { "gate", "status", "cached", "exit_code",
                       "duration_ms", "log_path" } ],
  "promoted": true|false
}
```

`conflicts` non-empty means the submission was rejected pre-gate;
`promoted: true` means the integration branch advanced in this call.
