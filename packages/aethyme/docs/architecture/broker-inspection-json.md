# Bounded broker inspection JSON

Last Updated: 2026-10-21

The broker bounds interactive status, health and worktree reports so a large
session inventory cannot stall an agent indefinitely. When a report omits work,
its JSON names the omitted checks; an absent finding is not evidence of a clean
state.

## Status and doctor

In a registered worktree, routine status identifies the current session. Use
`--session <id>` to select it explicitly from elsewhere. JSON uses
`aethyme.broker-session-view/v2` for either selection. Its `session` block
carries the next safe action, preparation and publication state, and
session-owned recovery observations and live waits. When a wait needs no new
command, the next action carries an instruction and a null command. `peer_state` includes recorded overlaps on
the session's leased paths and declared scopes. Its `pre_edit` recommendation
is also delivered in the agent's startup brief and on relevant peer changes.
`continue` follows the session's next action. `wait` leaves disjoint work
available while the agent coordinates an overlap with the earlier session.
`yield` asks the later session to confirm matching assignments and offers the
guarded duplicate close for an unchanged checkout. The compact view names the
relevant peer and shows a bounded assignment preview. These recommendations use
recorded evidence. Operation admission still checks live ownership and policy.
Lease release request details
are limited to requests involving the session. Repository information reports
counts by warning kind, stale sessions as recorded, and unavailable sources.
Other sessions' recovery commands and evidence appear in explicit drilldowns.

Outside a registered worktree, routine JSON uses
`aethyme.broker-status-compact/v1`. Advice categories keep severity, count and
omission fields. `session_contexts`, `lease_release_requests`,
`coordinated_operations` and `waiters` expose counts with empty `records`.
Deferred checks are unknown and `leases_refreshed: false` identifies recorded
lease observations. In either scope, `--since-revision` returns only changed
sections. Use `--full --json` for exact evidence, `broker unblock` for current
recovery inspection, and `--refresh --json` for a fresh audit.
`--summary --json` retains its brief shape.

`broker status --refresh --json` lists omitted work in `deferred_checks` and
the `status.inspection-budget` advisory. Sessions that were not reached by
the unpushed-commit scan appear in `unpushed_work.not_inspected_sessions`.
Consumers must preserve those sessions as unknown.

`broker status doctor --json` exposes `deferred_checks` and `budget_cut`.
A nonempty list means the health report is incomplete and `healthy` is false;
fields from skipped checks remain unknown.

## Worktree inventory

`broker advanced worktrees --json` names skipped discovery, inspection,
inventory or sizing work in `deferred_checks`. A row with
`state: "not_inspected"` was not classified; the broker treats that state as
holding unique work rather than clean or recoverable. Unmeasured size fields
are not zero-byte evidence.

The `reconciliation.complete` field is false when the inventory deadline
ends before every root is read. In that case its counts describe only the
observed prefix.

## Retention health

`gc plan --json` and the retention report in `status doctor --json` include
`deferred_checks` when a bounded scan did not finish. A partial floor may
prove the retained-byte budget is exceeded, but it cannot prove an
under-budget result: the verdict is `unknown` unless the measured floor
already proves `over`.
