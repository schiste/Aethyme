# Aethyme Broker — Event Stream Contract

Version: schema_version **1** — **FROZEN** (2026-07-17; per-row field, see
rules below)
Source of truth: `rust/crates/aethyme-broker/src/events.rs` (kinds and
payload constructors) — this document describes what that module defines.
Enforcement: `rust/crates/aethyme-broker/tests/contract_v1.rs` locks the
v1 kind list and per-kind payload field names against golden expectations;
any unversioned change fails CI.
Stable `--json` command outputs are a separate surface, documented in
[json-contracts.md](json-contracts.md).

## Change policy (v1 frozen)

Allowed **without** a version bump (additive-only evolution):

- Adding a **new kind** (new dotted `<domain>.<what>` string).
- Adding a **new payload field** to an existing kind.
- Adding a new enum-derived kind by adding a variant to
  `SessionStatus`/`GateStatus`/`MergeStatus`/`OperationStatus` (variants
  are add-only).

Requires a **schema_version bump** (breaking — never do this silently):

- Renaming or removing a kind, or repurposing its meaning.
- Renaming or removing a payload field, or changing a field's type or
  semantics.
- Changing the envelope row shape (`id`, `schema_version`, `ts`, `kind`,
  `session_id`, `payload_json`).

### Bump procedure

1. Increment `EVENTS_SCHEMA_VERSION` in
   `rust/crates/aethyme-broker/src/schema.rs`. New rows carry the new
   number; existing rows keep theirs — the log is never rewritten, and
   consumers use the per-row value to pick the right interpretation.
2. Update the golden expectations in `tests/contract_v1.rs` to the new
   shape (the failing test is the checklist of what changed).
3. Update this document: bump the version header, describe the new shape,
   and keep a short "v(N-1) differences" note so mixed-version logs stay
   interpretable.
4. All four artifacts — `schema.rs`, `events.rs`, `contract_v1.rs`, and
   this file — change in the **same commit**.

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
| `gate.pass` / `.fail` / `.error` | submitting session (nullable) | `gate`, `tree`, `failure_class` | a gate run concluded against tree `tree`; `failure_class` is nullable and classifies non-pass outcomes (`test_failure`, `environment`, `resource_contention`, `timeout`, `unknown`) |
| `gate.cancelled` | the session | `gate`, `tree`, `failure_class` | a superseded in-flight run was killed (`failure_class` is null) |
| `gate.cached` | requesting session (nullable) | `gate`, `tree`, `saved_ms`, `cached_status`, `failure_class` | a cache hit avoided executing a gate (`saved_ms` = the cached run's duration; cached failed outcomes report `cached_prior_fail`) |
| `merge.submitted` | the session | `head` | head commit entered the queue (idempotent: once per head) |
| `merge.simulating` | the session | — | merge-tree simulation started |
| `merge.conflict` | the session | `conflicts[]`, `blocking_sessions[]`, `base` | simulation found textual conflicts (rejected pre-gate) |
| `merge.verified` | the session | `merge_commit`, `base`, `gates[]` | gates passed on the merged tree |
| `merge.rejected` | the session | `merge_commit`, `base`, `gates[]` | a gate failed on the merged tree |
| `merge.promoted` | the session | `branch`, `commit` | integration branch advanced |
| `merge.externally_landed` | the session | `branch`, `commit`, `externally_landed`, `classification`, `upstream_ref`, `upstream_landing`, `operator_resolution` (nullable; operator, reason, resolution file, bound upstream commit, old integration) | reconciliation found equivalent or operator-attested superseding content in the named upstream ref |
| `merge.superseded` | the session | — | a newer head from the same session replaced this entry |
| `merge.integration_branch_created` | — | `branch`, `at` | first submit created the local integration branch |
| `merge.integration_refreshed` | — | `branch`, `from`, `to` | integration fast-forwarded to main's HEAD (only when it held no unmerged promotions) |
| `operation.prepared` / `.running` | the session | `operation_id`, `provider`, `repository`, `scope`, `effect`, `status`, `exit_code` (nullable) | a redacted Git/GitHub operation intent was durably recorded, then began while holding the repository write lock when required |
| `operation.succeeded` / `.failed` | the session | same operation fields | the fixed `git` or `gh` subprocess exited and its definitive status was durably recorded (`failed` is used for reads or commands that never started) |
| `operation.outcome_unknown` | the session | same operation fields | a previous writer released its process lock without recording an outcome, or a write exited non-zero after possibly applying partial effects; overlapping writes fail closed |
| `operation.reconciled_succeeded` / `.reconciled_failed` | the session | same operation fields | an operator inspected external state and attested the crash-ambiguous outcome |

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
kind: add the constant + constructor there, a row here, and a golden
entry in `tests/contract_v1.rs` — never touch existing fields. This
file, that module, and the contract test must change in the same commit.
Breaking changes follow the bump procedure above.
