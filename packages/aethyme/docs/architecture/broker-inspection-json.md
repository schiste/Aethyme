# Bounded broker inspection JSON

Last Updated: 2026-10-08

The broker bounds interactive status, health and worktree reports so a large
session inventory cannot stall an agent indefinitely. When a report omits work,
its JSON names the omitted checks; an absent finding is not evidence of a clean
state.

## Status and doctor

`broker status --refresh --json` lists omitted work in `deferred_checks` and
the `status.inspection-budget` advisory. Sessions that were not reached by
the unpushed-commit scan appear in `unpushed_work.not_inspected_sessions`.
Consumers must preserve those sessions as unknown.

Routine `broker status --summary --json` uses recorded observations. Its
`deferred_checks` names Git and retention checks it intentionally skipped.
`leases_refreshed: false` means the report did not derive fresh leases.

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
