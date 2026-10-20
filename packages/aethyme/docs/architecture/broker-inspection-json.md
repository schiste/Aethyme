# Bounded broker inspection JSON

Last Updated: 2026-10-20

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
