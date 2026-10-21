# Bounded broker inspection JSON

Last Updated: 2026-10-21

The broker bounds interactive status, health and worktree reports so a large
session inventory cannot stall an agent indefinitely. When a report omits work,
its JSON names the omitted checks; an absent finding is not evidence of a clean
state.

## Status and doctor

Routine default text and `broker status --compact --json` use the compact
recorded-observation view. Its schema is
`aethyme.broker-status-compact/v1`; advice is grouped by `(id, severity)` with
observation and duplicate counts, and every distinct category and its
drilldown commands remain present. `omitted_details` names evidence and
per-item identity fields the view leaves out. `data_freshness.deferred_checks`
are explicitly unknown, not clean, and `leases_refreshed: false` means the
counts reflect the last recorded refresh. Null summary fields are listed in
`summary.unknown_fields`.

Compact output also retains critical recorded coordination context:

- `session_contexts` contains session IDs and repository/tab/provider labels.
- `collaboration_fence` is present when active or pending, using the same
  committed-config semantics as full status; it is absent otherwise.
- `lease_release_requests` contains pending request IDs, requester/holder IDs
  and paths. Resolved requests and request reasons require full status.
- `coordinated_operations` reports total unresolved, holding-lock and waiting
  counts, plus operation IDs, sessions, lock resources, scopes, states and
  blocker IDs. Liveness detail requires full status.
- `waiters.records` retains process/session, resource and holder observations.

Each list retains at most ten rows. `count` describes the complete observed
list, `omitted_count` names rows excluded by the limit, and `records` contains
those displayed. Operation counts use `holding_lock_count` and `waiting_count`;
only non-holder `prepared` operations count as waiting. `waiters.details_omitted`
is true exactly when its row limit excluded observations. Empty lists have
zero counts and empty records. These reads use broker records and existing
fence/waiter helpers; they do not scan retained worktrees or Git history. Required fence checks may
read committed config from Git.
Per-list omission counts also appear in default text. Waiting counts describe
recorded prepared rows, not verified process waits. An `outcome_unknown`
operation requires reconciliation and is never described as queued.
`phase_timings_ms.context_records` measures the additional metadata collection.
The row caps bound output, not storage reads: pending lease requests use the
existing filtered event-history replay. A required-but-unraised fence can use
the existing committed-config lookup; an active fence uses its persisted record.

The older `broker status --summary --json` shape remains available. Plain
`broker status --json` still returns the full recorded view. Use that full
shape for exact per-session, lease-path or evidence detail; use `--refresh
--json` for the explicit expensive audit.

`broker status --refresh --json` lists omitted work in `deferred_checks` and
the `status.inspection-budget` advisory. Sessions that were not reached by
the unpushed-commit scan appear in `unpushed_work.not_inspected_sessions`.
Consumers must preserve those sessions as unknown.

`broker status doctor --json` exposes `deferred_checks` and `budget_cut`.
A nonempty list means the health report is incomplete and `healthy` is false;
fields from skipped checks remain unknown.

The `retention` field is `null` when root-dependent retention inventory was
not run, and `retention_unavailable_reason` explains why. This occurs for an
unbound, reserved, or recovery-required worktree root. A null retention field
is unknown, never an empty inventory or zero-byte result, and makes `healthy`
false. The separate `integrity` field still reports the broker database
check. `certify` keeps `certify.broker-db` tied to that integrity result and
warns when retention is unavailable only because a fresh or legacy root is
unbound. It fails certification for reserved or recovery-required bindings.
Bound-root inspection failures and malformed binding rows remain errors; they
are not converted into an unavailable-health report.

## Worktree inventory

`broker advanced worktrees --json` names skipped discovery, inspection,
inventory or sizing work in `deferred_checks`. A row with
`state: "not_inspected"` was not classified; the broker treats that state as
holding unique work rather than clean or recoverable. Unmeasured size fields
are not zero-byte evidence.

The `reconciliation.complete` field is false when the inventory deadline
ends before every root is read. In that case its counts describe only the
observed prefix.

When root-dependent reconciliation is deferred because the worktree root is
unbound, `reconciliation.deferred_reason` is `fresh_root_unbound` or
`legacy_root_unbound`. The report has `complete: false`,
`scanned_root_count: 0`, `sized: false`, zero-valued counts and bytes, and an
empty `unclaimed` list. Those placeholders mean the root was not inspected;
they do not mean it contains no work or is empty. Read-only reporting does not
create or bind the root.

## Retention health

`gc plan --json` and the retention report in `status doctor --json` include
`deferred_checks` when a bounded scan did not finish. A partial floor may
prove the retained-byte budget is exceeded, but it cannot prove an
under-budget result: the verdict is `unknown` unless the measured floor
already proves `over`.
