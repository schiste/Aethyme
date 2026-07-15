# Aethyme Broker — Event Stream Contract

Version: schema_version **1** (per-row field; see rules below)
Source of truth: `rust/crates/aethyme-broker/src/events.rs` (kinds and
payload constructors) — this document describes what that module defines.

## Consuming the stream

```bash
aethyme broker events --json --since <last-id>        # replay / catch up
aethyme broker events --json --follow                 # live NDJSON, ~700ms poll
aethyme broker events --json --kind merge.            # prefix filter
```

One JSON object per line:

```json
{"id":15,"schema_version":1,"ts":1783927429595,"kind":"merge.promoted",
 "session_id":1,"payload_json":"{\"branch\":\"aethyme/integration\",\"commit\":\"9d4c…\"}"}
```

## Guarantees

1. **Ordering:** `id` is strictly increasing forever. Ids are never
   reused, even after `events prune` (SQLite AUTOINCREMENT). A consumer
   that persists its last seen `id` can always resume with `--since`.
2. **Atomicity:** events are written in the same transaction as the state
   change they describe — an event implies the change committed.
3. **At-least-once reading, exactly-once writing:** each state change
   emits exactly one event; consumers polling with overlapping windows
   must dedupe on `id` (trivial given ordering).
4. **Additive evolution:** kinds are never renamed or repurposed; payload
   fields are never renamed or removed, only added. Breaking changes bump
   the per-row `schema_version` so mixed-version logs stay readable.
5. **Liveness caveat:** events are recorded when broker commands run
   (there is no daemon). The stream sees everything, the moment it is
   recorded — but "happens" means "a session invoked the broker".
6. **Retention:** the log is append-only in normal operation.
   `aethyme broker events prune --keep-days <n>` is an explicit operator
   action; cursors survive it (rule 1).

## Event catalog

| Kind | session_id | Payload fields | Emitted when |
|---|---|---|---|
| `session.registered` | the session | `origin` (adopted\|spawned), `branch`, `worktree_path` | adopt / start-agent |
| `session.reused` | the session | `task`, `diff_base` (both nullable) | `adopt --reuse` pointed an existing session at a follow-up task (added 2026-07-14) |
| `session.active` / `.idle` / `.stale` | the session | — | liveness transition persisted (once per transition) |
| `session.exited` | the session | `exit_code` (when known) | spawned PID died, or explicit transition |
| `session.cleaned` | the session | — | `cleanup` removed the worktree, `close` marked the session finished (state only), or `adopt --replace-stale` retired the previous session |
| `lease.claimed` / `lease.released` | claiming session | `path` | explicit lease operations |
| `lease.overlap` | lower session of the pair | `session_a`, `session_b`, `path` | a NEW overlapping-edit pair is detected (never re-announced) |
| `gate.pass` / `.fail` / `.error` | submitting session (nullable) | `gate`, `tree` | a gate run concluded against tree `tree` |
| `gate.cancelled` | the session | `gate`, `tree` | a superseded in-flight run was killed |
| `gate.cached` | requesting session (nullable) | `gate`, `tree`, `saved_ms` | a cache hit avoided executing a gate (`saved_ms` = the cached run's duration) |
| `merge.submitted` | the session | `head` | head commit entered the queue (idempotent: once per head) |
| `merge.simulating` | the session | — | merge-tree simulation started |
| `merge.conflict` | the session | `conflicts[]`, `blocking_sessions[]`, `base` | simulation found textual conflicts (rejected pre-gate) |
| `merge.verified` | the session | `merge_commit`, `base`, `gates[]` | gates passed on the merged tree |
| `merge.rejected` | the session | `merge_commit`, `base`, `gates[]` | a gate failed on the merged tree |
| `merge.promoted` | the session | `branch`, `commit` | integration branch advanced |
| `merge.superseded` | the session | — | a newer head from the same session replaced this entry |
| `merge.integration_branch_created` | — | `branch`, `at` | first submit created the local integration branch |
| `merge.integration_refreshed` | — | `branch`, `from`, `to` | integration fast-forwarded to main's HEAD (only when it held no unmerged promotions) |

## Operational commands

- `aethyme broker doctor [--json]` — database integrity, live sessions
  with missing worktrees, orphaned gate pidfiles (removed on sight).
  Exit code 0 = healthy; non-zero otherwise (scriptable).
- `aethyme broker events prune --keep-days <n>` — retention.
- `aethyme broker metrics [--json]` — cost/benefit accounting: gate
  executions vs cache hits (time saved), conflicts caught pre-gate,
  overlap warnings, and broker command latency. Command telemetry
  (`.aethyme/logs/command-metrics.jsonl`) is safe by construction: the
  command label is allowlisted subcommand words only — task text, paths,
  and ids can never appear in it.

## Rules for broker developers

Emit payloads **only** through `src/events.rs` constructors. Adding a
kind: add the constant + constructor there, a row here, and never touch
existing fields. This file and that module must change in the same commit.
