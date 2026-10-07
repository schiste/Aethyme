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
aethyme broker advanced events --json --since <last-id>        # replay / catch up
aethyme broker advanced events --json --follow                 # live NDJSON, ~700ms poll
aethyme broker advanced events --json --kind merge.            # prefix filter
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
   `aethyme broker advanced events prune --keep-days <n>` is an explicit operator
   action; cursors survive it (rule 1).

## Event catalog

| Kind | session_id | Payload fields | Emitted when |
|---|---|---|---|
| `session.registered` | the session | `origin` (adopted\|spawned), `branch`, `worktree_path` | adopt / start-agent |
| `session.reused` | the session | `task`, `diff_base` (both nullable) | `adopt --reuse` pointed an existing session at a follow-up task (added 2026-07-14) |
| `session.holder_bound` | the session | `pid`, `started` (`ps` lstart text), `command`, `reason` (`registered`\|`first_use`\|`holder_gone`\|`take_over`), `previous_pid`, `previous_started` (both nullable) | the agent process that holds the session changed: `start`/`start --adopt` registered it, its first identified caller bound it, its previous holder stopped running, or `--take-over` moved it (added 2026-10-05, #393) |
| `session.holder_gone` | the session | `pid`, `started` (`ps` lstart text), `command` | the broker first saw the session's recorded holder process no longer running; recorded once per holder, it starts the lease stale grace (`[leases] stale_grace_minutes`) (added 2026-10-06, #360) |
| `session.install_recorded` | the session | `version`, `describe`, `commit`, `path` (the router build that ran `start`/`start --adopt`; `describe`, `commit` and `path` nullable), `engine_banner` (the `aethyme-engine-cli --version` banner on PATH, nullable) | the installed aethyme build that started or adopted the session, compared by `broker status` to raise `install.replaced` when the installed pair changes later (added 2026-10-06, #293) |
| `session.context_updated` | the session | `repository_name`, `tab_name`, `ai_provider`, `short_name` (all nullable) | registration or a host snapshot recorded human-facing Chau7/session context |
| `session.active` / `.idle` / `.stale` | the session | — | liveness transition persisted (once per transition) |
| `session.exited` | the session | `exit_code` (when known) | spawned PID died, or explicit transition |
| `session.cleaned` | the session | — | `cleanup` removed the worktree, `close` marked the session finished (state only), or `adopt --replace-stale` retired the previous session |
| `session.finished` | the session | `session_id`, `status`, `latest_queue_entry_id`, `latest_queue_status`, `delivery`, `pending_work`, `leases_held`, `last_gate`, `last_graph_integrity`, `cleanup_safe`, `recommended_next_action` | `finish` successfully closed the session; written atomically with `session.cleaned`. The payload is a redacted handoff: no worktree/task/command/log paths, warnings, diffs, hunks, or file contents. |
| `lease.claimed` / `lease.released` | claiming session | `path` | explicit lease operations |
| `lease.released` (reasoned release) | releasing session | `path`, `reason` (`finish`, `request_acked` or `request_granted`), `lease_id`, `created_at` (the released lease generation) | a verified terminal `finish` released every lease the session held, in the transaction that closed it and before any physical cleanup; one event per lease (added 2026-10-06, #358); an acknowledged or granted release request (#359) records the same payload for each lease it releases on the path |
| `lease.release_requested` | requesting session | `path`, `requester_session_id`, `holder_session_id`, `reason` | a session asked another to release its lease on `path`; the event id is the request id (added 2026-10-06, #359) |
| `lease.release_acked` / `lease.release_declined` / `lease.release_granted` | holding session | `request_id`, `path`, `reason` (null for an ack; the holder's reason for a decline; `holder_stale` for a grant) | the request's outcome: the holder released the path, declined, or was gone past `[leases] stale_grace_minutes` and the broker released it for them. Only the first outcome event for a request counts (added 2026-10-06, #359) |
| `lease.overlap` | lower session of the pair | `session_a`, `session_b`, `path` (first sample path), `severity` (`low`/`high`), `paths_count`, `conflicting_paths`, `sample_paths`, `classified`, `reason` | one event per session **pair**, when it starts overlapping and again only when its severity or conflicting paths change. Before 2026-09-30 this fired once per overlapping *path* (and concurrent refreshes could repeat it); `path` is kept for those readers. `high` means `git merge-tree` reports a conflict between the two sessions' current states on an overlapping path |
| `gate.pass` / `.fail` / `.error` | submitting session (nullable) | `gate`, `tree`, `failure_class` | a gate run concluded against tree `tree`; `failure_class` is nullable and classifies non-pass outcomes (`test_failure`, `environment`, `resource_contention`, `timeout`, `unknown`) |
| `gate.cancelled` | the session | `gate`, `tree`, `failure_class` | a superseded in-flight run was killed (`failure_class` is null) |
| `gate.cached` | requesting session (nullable) | `gate`, `tree`, `saved_ms`, `cached_status`, `failure_class` | a cache hit avoided executing a gate (`saved_ms` = the cached run's duration; cached failed outcomes report `cached_prior_fail`) |
| `graph.integrity_checked` | requesting session (nullable) | `status`, `enforced`, `tree`, `policy`, `engine_version`, `changed_paths[]` | an authoritative committed-graph check completed (submit, gate run, or a brokered `gh pr merge`'s head; advisory since #280); paths are repository-relative and the free-form diagnostic reason is deliberately not persisted |
| `merge.submitted` | the session | `head` | head commit entered the queue (idempotent: once per head) |
| `merge.simulating` | the session | — | merge-tree simulation started |
| `merge.conflict` | the session | `conflicts[]`, `conflict_details[]` (`path`, `originating_commit`, `ownership`, `integration_side_commits[]`, `remediation`, `commands[]`), `blocking_sessions[]`, `base` | normalized patch replay found textual conflicts (rejected pre-gate); blockers are limited to active leases overlapping surviving paths |
| `merge.verified` | the session | `merge_commit`, `base`, `gates[]` | gates passed on the merged tree |
| `merge.policy_deferred` | the session | `base`, `gates_changed`, `graph_policy_changed` | the submission changed `.aethyme/gates.toml` or the `[graph]` policy; it was judged by the base policy, and its change applies once it lands (added 2026-09-23) |
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
| `broker.blocker.cleared` | — | `id`, `kind`, `reason` (nullable), `detail` (kind-specific: `gate_name`, `tree_hash`, `removed_gate_result_ids[]` (the ids of the failing results marked cleared; since #416 the rows are retained with `cleared_at`/`cleared_reason`, the key name is kept for v1), `operator_reason` for a gate verdict; `remote_key`, `outcome` for a host operation; `generation` for a resource lease; `gate_name`, `pid` for a pidfile) | `aethyme broker unblock <id>` cleared a blocker that no older recovery path records (added 2026-09-23). Operations reconciled through `op:<n>`, or a host operation a repository row journals, are recorded by the existing `operation.reconciled_*` kinds instead; path-lease releases by `lease.released`. |
| `broker.command.succeeded` / `.failed` | the command's `--session`, else the session owning the current worktree (nullable) | `command_surface` (allowlisted subcommand words, e.g. `broker.start`), `exit_code`, `failure_class` (`command_failed`, `submission_failed`, `recovery_failed`, `coordinated_operation_failed`; null on success), `operation_id`, `queue_entry_id` (nullable), `message` (`.failed` only, when the command printed an error: that text with credentials cut and capped at 500 characters; a coordinated `git`/`gh` failure includes the end of the provider's stderr; added 2026-10-02) | a broker command that records telemetry finished; `aethyme broker status doctor` lists the last day's failures with their message |
| `broker.session.pushed` | the session | `session_id`, `branch`, `oid`, `pr_url` (nullable) | `broker push` published the session's own branch at `oid`; `pr_url` is set when `--pr` found or opened its draft pull request (added 2026-09-30) |
| `broker.session.synced` | the session | `session_id`, `branch`, `strategy` (`rebase`/`merge`), `from`, `to`, `default_ref`, `default_commit`, `behind_before` | `broker sync` brought the session from `from` to `to` by rebasing an unpublished branch onto, or merging into a published one, the fetched default branch (added 2026-10-01) |
| `broker.checkout.fast_forwarded` | — | `trigger` (`broker.<command>`), `branch`, `upstream_ref`, `from`, `to` | a normal-mode broker command run from the primary checkout fast-forwarded its clean branch from `from` to the already-fetched configured upstream commit `to`; this never fetches or moves a session worktree (added 2026-10-07, #232) |
| `broker.cleanup.archived` | the session | `session_id`, `archive`, `head`, `disposition`, `unlanded_commits` (count), `untracked_files` (count), `worktree_removed` | `finish cleanup resolve <id> --archive --confirm` wrote and verified a recovery archive; `worktree_removed` is false when the removal that follows failed and the worktree was kept (added 2026-09-30) |
| `broker.gc.recovery-archive-removed` | the session | `path`, `session_id`, `head`, `landed`, `bytes` | a reviewed `gc apply` removed one recovery archive after re-proving it belongs to this repository and has either landed with no uncommitted changes or passed `recovery_archive_days` (added 2026-09-30) |
| `broker.session.abandoned_unpushed` | the session | `session_id`, `branch`, `head`, `unpushed_commits` (count), `reason` | `finish` or `finish close` closed a session with `--abandon --reason` while it held commits no remote has, under `[delivery] push_session_branches = true` (added 2026-09-30) |

## Operational commands

- `aethyme broker status doctor [--json]` — database integrity, live sessions
  with missing worktrees, orphaned gate pidfiles (removed on sight).
  Exit code 0 = healthy; non-zero otherwise (scriptable).
- `aethyme broker advanced events prune --keep-days <n>` — retention.
- `aethyme broker advanced metrics [--json]` — cost/benefit accounting: gate
  executions vs cache hits (time saved), conflicts caught pre-gate,
  overlap warnings, and broker command latency. Command telemetry
  (`.aethyme/logs/command-metrics.jsonl`) is safe by construction: the
  command label is allowlisted subcommand words only — task text, paths,
  and ids can never appear in it.
- `aethyme broker advanced insights [--days <n>] [--session-limit <n>]
  [--pull-request-limit <n>] [--json]` — what the pipeline did rather than
  what it cost: the session funnel (registered → submitted → verified →
  landed → pull request), time between stages, outcomes, gate reliability
  split into execution vs coordination wait, and coordination counts.
  `--days 0` reads all recorded history. See
  [`insights-contract.md`](insights-contract.md) for the JSON contract and
  for the four rules the report follows about what it may claim.

## Rules for broker developers

Emit payloads **only** through `src/events.rs` constructors. Adding a
kind: add the constant + constructor there, a row here, and a golden
entry in `tests/contract_v1.rs` — never touch existing fields. This
file, that module, and the contract test must change in the same commit.
Breaking changes follow the bump procedure above.
