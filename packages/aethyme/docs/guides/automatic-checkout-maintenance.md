# Automatic checkout maintenance

Last Updated: 2026-11-14

A session becoming closed and its checkout being removed are separate steps.
Idle-session retirement makes a checkout eligible for later inspection; it does
not authorize deletion. Checkout removal still checks ownership, other session
records, content delivery, protected files, process use, and current policy.

## Retention windows

The default open-session abandonment window is three days (72 hours).
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

Closed checkouts have a one-day grace period. Configure retention in the primary
repository's `.aethyme/broker.toml`. Existing explicit overrides continue to
apply after an upgrade:

```toml
[retention]
session_abandoned_after_hours = 72
session_landed_after_hours = 24
closed_worktree_grace_hours = 24
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

Expiry alone never closes a session. The normal three-day inactivity window,
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

On macOS, new spawn witnesses record stable volume UUIDs alongside paths,
inodes and the original root-generation token. A remount may change a device
number without changing the filesystem object; matching saved UUIDs and inodes
permit that case in cohort checks and automatic checkout maintenance. A different
volume, inode, path or generation still refuses. If a stable UUID cannot be
captured, the new witness keeps the schema-2 exact-device behavior. Interrupted
finish-cleanup recovery proofs also retain their exact device checks.
Older witnesses have no saved volume UUID and keep their exact device checks
everywhere except automatic whole-checkout removal, which re-proves a closed
spawned checkout whose only mismatch is renumbered devices (paths, inodes, the
spawn root generation, the Git registration on the session branch and the
branch tip must all still match). See "Automatic removal" in the
[worktree cleanup guide](worktree-cleanup.md) for the record it leaves and the
keep code a refusal gets. Artifact reclamation and finish-cleanup recovery
proofs for such a checkout still compare devices exactly, so use a separately
reviewed archive resolution when one of those refuses. Older runtimes refuse schema-3 witnesses; use the new paired runtime
for maintenance of repositories containing them.

This opt-in can compact a checkout during its ordinary one-day grace period.
Every retirement archive records a minimum retention floor of at least 15 days
and at least the configured `closed_worktree_grace_hours`, measured from the
archive's recorded creation time. The floor is stored in the archive receipt
and manifest, so lowering policy later or landing the commits does not shorten
an existing archive's protection. Keep archives until their recovery purpose
has been fulfilled. A bounded background attempt may defer when it lacks enough
remaining time; an incomplete archive is never permission to remove a checkout.

## Ignored folders

Normal whole-checkout cleanup retains unknown Git-ignored directories, including
local datasets and saved records. Recognized or explicitly configured build-output
directories remain disposable; a standalone ignored file is released only when
a `[cleanup] regenerable` pattern names its whole path, and a protected name is
never released (see the [worktree cleanup guide](worktree-cleanup.md)).

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

## Background deletion pace

The detached cleanup worker deletes finished sessions' build output while
nobody waits on it, so it runs as a background job: lowest CPU and disk I/O
priority (macOS and Linux), a rate limit on unlinks, one deleter at a time
across every repository on the machine, and a pause while memory pressure is
above normal or load is far above the core count. Commands you wait on
(`finish`, `finish cleanup <id>`, `gc apply`, `gc sweep`) keep full speed.

```toml
[cleanup]
# Unlinks per second for the background worker. Default 1000.
# 0 or "unlimited" restores full speed.
unattended_unlinks_per_second = 1000
```

The limit is lifted automatically while free space is below the gate
headroom, because freeing space is then the priority; the lowered priority and
the pressure pause stay. Nothing deferred is dropped: a worker that pauses,
reaches its time limit, or waits too long for another repository's deleter
leaves its request durable, and the next broker open starts a worker again.

`aethyme broker status --json` shows the last worker run under
`cleanup_retention.last_artifact_sweep.unattended_pacing`: entries and bytes
removed, the removal rate, time slept by the limiter, time paused for memory
and for load, time waited for the host deleter lock, requests coalesced into
the run, and whether priority lowering succeeded. See
[cross-process consumers](../architecture/cross-process-consumers.md) for the
field list.

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
| `landed_sessions_examined` | Open sessions idle past `session_landed_after_hours` that the landed lane judged; omitted when zero. |
| `landed_sessions_closed` | Of those, the sessions it closed; omitted when zero. |
| `landed_sessions_declined` | Sessions it kept, counted per reason code (`recent_activity`, `changed`, `process_alive`, `holder_alive`, `holder_unbound`, `holder_unknown`, `lease_held`, `lease_unknown`, `worktree_unavailable`, `busy`, `operation_active`, `operation_unknown`, `policy_unavailable`, `auto_remove_disabled`, `pinned`, `pull_request_unmerged`, `no_session_commits`, `no_landing_evidence`, `not_landed`, `landing_unknown`, `worktree_unclean`, `moved_before_close`, `upstream_moved`, `budget_exhausted`); omitted when empty. A session the abandonment window then closed is not counted. |
| `retirement_cursor` | `{after_session_id, high_water_session_id}`: where the open-session retirement lap stood when the report was written; omitted before the first lap. |
| `landed_retirement_elapsed_ms` | Milliseconds the landed lane spent; omitted when zero. |

Readers must accept these fields being absent in older saved reports. A deferred
session remains protected; `sessions_retired` counts session state transitions,
not removed directories or reclaimed bytes. The checkout grace begins from
closure, and content/ownership checks still decide whether removal is allowed.

A clean session whose work is on the fetched default branch can enter guarded retirement after `session_landed_after_hours` (default 24 hours), with no `finish`. The landing is the rule `finish` uses: a representation recorded against that branch, ancestry, or a content landing that accounts for every commit (a commit made after a squash keeps the session). The content search runs only when the cached pull-request listing records a merged pull request for the session, a representation exists, or the head is already an ancestor. Its recorded holder must be gone, or still running with an expired task lease and no gate, operation, preparation or open file in the worktree; the `expire_abandoned_task_leases` opt-in is not needed for a landed session. Set it to `0` to disable this earlier path. The ordinary `session_abandoned_after_hours = 0` also disables retirement, and `[cleanup] auto_remove = false` keeps every session out of it. Unknown holders, current task leases, unmerged linked pull requests, changed heads, dirty or unproven work are preserved, each counted under a reason in `landed_sessions_declined`. Checkout removal still has its separate grace period and safety checks.
