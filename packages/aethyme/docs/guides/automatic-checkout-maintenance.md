# Automatic checkout maintenance

Last Updated: 2026-10-28

A session becoming closed and its checkout being removed are separate steps.
Idle-session retirement makes a checkout eligible for later inspection; it does
not authorize deletion. Checkout removal still checks ownership, other session
records, content delivery, protected files, process use, and current policy.

## Retention windows

The default open-session abandonment window is 72 hours. Maintenance uses the
latest recorded activity and worktree activity, and protects a session when its
recorded task holder is alive or its identity cannot be checked. A shared agent
service is not evidence that every task it once hosted remains active.
Implicit file leases describe the session's diff and are released atomically
with an otherwise eligible retirement; they do not keep an abandoned session
open forever. An explicit claim with active or unknown holder liveness still
protects its session and needs resolution before retirement.

Closed checkouts have a 15-day grace period. Configure retention in the primary
repository's `.aethyme/broker.toml`:

```toml
[retention]
session_abandoned_after_hours = 72
closed_worktree_grace_hours = 360
```

Retirement runs in bounded pages with a durable cursor, so an old backlog does
not require one unbounded foreground pass. Unreadable retention configuration
keeps sessions open and reports a deferral. Recent activity and active holders
remain protected regardless of a session's position in the page.

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
exited and the new gate-use protocol is active. During an upgrade, also suspend
retirement with `session_abandoned_after_hours = 0` and hold automatic removal
with `strict = true` and `auto_remove = false` before the new binary first opens
the live database. Back up the database consistently and preserve the original
configuration. Raise the compatibility floor, verify that old clients with
open database connections have exited, then restore the reviewed retention and
cleanup settings and enable the broader sweep.

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
