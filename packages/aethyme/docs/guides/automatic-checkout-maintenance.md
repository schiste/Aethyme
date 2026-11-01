# Automatic checkout maintenance

Last Updated: 2026-11-01

A session becoming closed and its checkout being removed are separate steps.
Idle-session retirement makes a checkout eligible for later inspection; it does
not authorize deletion. Checkout removal still checks ownership, other session
records, content delivery, protected files, process use, and current policy.

## Retention windows

The default open-session abandonment window is five days (120 hours).
Maintenance uses the latest recorded activity and worktree activity. A live
recorded task holder remains protected unless the repository explicitly opts
into the finite task-lease protocol described below; unknown identity remains
protected. A shared agent service is not evidence that every task it once
hosted remains active.
Implicit file leases describe the session's diff and are released atomically
with an otherwise eligible retirement; they do not keep an abandoned session
open forever. Explicit claims with active or unknown holder liveness still
protect their session unless the exact live holder's opted-in task lease has
expired and its stale grace has elapsed.

Closed checkouts have a 15-day grace period. Configure retention in the primary
repository's `.aethyme/broker.toml`:

```toml
[retention]
session_abandoned_after_hours = 120
closed_worktree_grace_hours = 360
# Off by default; requires a coordinated broker upgrade and client drain.
expire_abandoned_task_leases = false
```

### Finite task leases

Task-lease expiry is off by default. First upgrade every broker writer to
schema 60, stop and drain all older clients with open database connections,
then set `expire_abandoned_task_leases = true` in the primary repository's
`.aethyme/broker.toml`. The schema floor blocks new old writers; it cannot revoke
an already-open database handle. Do not enable this setting while an older
broker process may still be writing.

When a guarded session command first runs, its exact current holder can upgrade
the holder binding to task-lease version 1. The lease lasts 24 hours and
recognized guarded work renews it at most once every five minutes. A quiet
long-running task should send an explicit heartbeat at least once every
24 hours:

```sh
aethyme broker advanced heartbeat --session <id>
```

Expiry alone never closes a session. The normal five-day inactivity window,
the exact holder binding, worktree identity and metadata, operation guard, gate,
preparation, artifact-use and process-use witnesses, and the final database
compare-and-swap must also allow retirement. A live `session.pid`, active child,
unresolved operation, in-progress preparation, unreadable inventory, or unknown
evidence keeps the session open. Eligible retirement closes session metadata;
it does not authorize physical checkout removal.

Legacy and malformed holder bindings do not gain finite age-expiry authority.
Known session work without a durable child witness first downgrades the binding
to the existing process-lifetime protection. In particular, the managed
`hooks pre-commit` path still runs raw shell gates without a durable child
marker, so it downgrades a finite binding before running them. A later guarded
command can upgrade it again. Until that hook path gains its own child witness,
a quiet session that only runs commits can remain protected while its recorded
holder process stays alive.

Retirement runs in bounded pages with a durable cursor, so an old backlog does
not require one unbounded foreground pass. Unreadable retention configuration
keeps sessions open and reports a deferral. Recent activity and active holders
remain protected regardless of a session's position in the page.

## Compact closed checkouts

Compacting a closed checkout writes and verifies a recovery archive before
removing the checkout. It is a separate opt-in from ordinary grace-based
removal and is disabled by default:

```toml
[retention]
compact_closed_worktrees = true
```

The background lane only considers a closed, broker-spawned checkout with a
current ownership witness and a complete, unchanged checkout cohort. Live
session records, explicit keep markers, active use, or incomplete inventory
defer the attempt. Adopted and legacy rows without an original ownership
witness require an explicit reviewed retirement plan; the broker does not
invent a witness for an existing directory.

This opt-in can compact a checkout during its ordinary 15-day grace period.
Every retirement archive records a minimum retention floor of at least 15 days
and at least the configured `closed_worktree_grace_hours`, measured from the
archive's recorded creation time. The floor is stored in the archive receipt
and manifest, so lowering policy later or landing the commits does not shorten
an existing archive's protection. Keep archives until their recovery purpose
has been fulfilled. A bounded background attempt may defer when it lacks enough
remaining time; an incomplete archive is never permission to remove a checkout.

## Ignored folders

Normal whole-checkout cleanup treats Git-ignored directories as disposable by
default. That includes local datasets and saved records. Preserve anything
irreplaceable outside the checkout before closing it. Standalone ignored files
remain protected by the cleanup policy.

The background artifact sweep historically recognizes a catalog of generated
folders. Broader in-place reclamation is separately opt-in in the primary
repository's `.aethyme/config.toml`:

```toml
[cleanup]
ignored_directory_sweep = true
```

Keep this setting off during a runtime transition until old broker clients have
exited and the new gate-use protocol is active. Before the new binary first
opens live state, back up the database consistently and preserve configuration.
In `.aethyme/broker.toml`, hold retirement with
`session_abandoned_after_hours = 0`, finish-time checkout removal with
`auto_cleanup_worktrees_on_finish = false`, and automatic artifact work with
`artifact_sweep_budget_ms = 0`. In `.aethyme/config.toml`, set `strict = true`,
`auto_remove = false`, and `ignored_directory_sweep = false`. Finish-time
cleanup is independent of `auto_remove`.

Inspect `.aethyme/gc-journal.json` before the first normal open: startup can
resume a previously confirmed plan independently of these automatic-maintenance
holds. Resolve any pending journal through the reviewed GC recovery workflow
before proceeding. A shared router/engine update affects multiple enrolled
repositories; apply the holds and verify the drain for every affected database.
Raise the compatibility floor, verify that old clients with open database
connections have exited, then restore the reviewed retention and cleanup
settings and enable the broader sweep.

A compatibility floor prevents fresh old clients from opening the database; it
does not stop a process that already has an open connection. Do not infer a
completed drain from the absence of a gate PID file: a gate may still be
waiting for admission. Do not restore saved configuration over concurrent
operator edits; reconcile those edits first.

The broader sweep checks current Git ignore rules, the complete closed checkout
cohort, and active artifact-use claims before deletion. It does not follow
symlinks or select protected Git administration and broker metadata roots.
An otherwise disposable ignored directory remains opaque: nested repositories
and metadata inside it are disposable along with its other contents.
A queued interrupted deletion is
revalidated against current policy before resuming; its queue record alone is
not permission to delete.

The artifact-use protocol is available on Unix targets. On unsupported targets,
the broader sweep and protocol-dependent unattended checkout removal remain
blocked. Explicit operator-requested normal cleanup retains its existing
platform guards; enabling the new sweep setting does not bypass that boundary.

To retain ignored leftovers and pause automatic artifact sweeps:

```toml
[cleanup]
strict = true
```

`auto_remove = false` disables automatic whole-checkout removal; it does not
replace strict mode for in-place artifact reclamation.

## Inspecting retained checkouts

Use the broker's normal cleanup workflow. A targeted cleanup avoids the full
bulk plan's recursive size inventory:

```sh
aethyme broker finish cleanup <session-id> --json
```

Several session records can name the same physical checkout. Count distinct
canonical checkout paths and inspect every record in that cohort. A closed row
cannot authorize removal while another row still owns the checkout.

A successful archive is not necessarily a successful removal. Inspect the
structured removal result and verify the checkout path is absent before
reporting it removed. Keep recovery archives until their preservation purpose
has been fulfilled. Estimated file sizes are not measured physical reclaimed
space, because clones, hard links, snapshots, and concurrent writers affect
volume usage.

## Maintenance report fields

`AutoCleanupReport` retains the existing `enabled`, `config_error`,
`evaluated_at_ms`, `removed`, `kept`, and `deferred` fields. `enabled` describes
whole-checkout automatic removal; session retirement follows the retention
policy separately.

| Optional field | Meaning |
| --- | --- |
| `sessions_retired` | Number of sessions closed by this maintenance pass; omitted when zero. |
| `retirement_deferred_reasons` | Typed reasons why retirement could not finish; omitted when empty. |

Readers must accept these fields being absent in older saved reports. A deferred
session remains protected; `sessions_retired` counts session state transitions,
not removed directories or reclaimed bytes. The checkout grace begins from
closure, and content/ownership checks still decide whether removal is allowed.
