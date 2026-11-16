# Automatic checkout maintenance

Last Updated: 2026-11-16

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
session_merged_after_hours = 2
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
captured, the new witness keeps the schema-2 exact-device behavior.
Older witnesses have no saved volume UUID. Automatic whole-checkout removal,
GC reclamation and compaction (including in-place ignored-directory
reclamation), `finish` and the recovery of an interrupted finish cleanup all
use one ownership re-proof for a closed spawned checkout whose only mismatch
is renumbered devices: paths, all four inodes, the spawn root generation and
the way the four objects share devices must be unchanged, and the Git
registration must still name the checkout on the session branch. A
finish-cleanup recovery proof saved since 0.8.96 records volume UUIDs and is
re-proven by UUID instead. See "Automatic removal" in the
[worktree cleanup guide](worktree-cleanup.md) for the record each lane leaves
and the keep code a refusal gets. Live artifact-use producers still compare
devices exactly. Older runtimes refuse schema-3 witnesses; use the new paired
runtime for maintenance of repositories containing them.

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
before proceeding. A startup resume that fails because the journal itself is
invalid warns once, naming `gc apply --confirm <digest>` and
`gc abandon --confirm <digest>`, and retries that journal only after 15 minutes
or once the journal changes. A busy GC lock is retried on the next open. A reviewed gate
log that changed is retained and skipped, not deleted, and does not block the
rest of the journal. A shared router/engine update affects multiple enrolled
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

## When closed checkouts are removed

Removal follows merges, not capacity pressure. On every run it is due, the
detached worker judges closed checkouts and removes those whose work every
removal proof shows landed, whether or not the repository is near
`[worktrees] max_checkouts`. A recorded merged pull request or a cleanup
request past the closed-worktree grace only moves a checkout ahead in the
queue: removal still needs the checkout's commits proved contained in the
fetched default branch, past its grace, with nothing unique in it. A merged
pull request alone is not enough.

- Below three quarters of the limit (or with no limit) it runs a `steady`
  slice: at most four removals in up to 60 seconds, the priority lane and at
  most 16 checkouts of the lap per pass, taking turns so the lap moves on in
  every pass. A checkout the database alone rules out (inside its
  closed-worktree grace, or used by an open session) and one whose earlier
  keep is remembered unchanged (uncommitted changes, unmarked deletions,
  commits not on the default branch, ignored leftovers) are kept without
  starting Git and cost none of those 16. Passes go on while the
  lap moves and the slice's time and the usual gates allow, and while the
  lap is unfinished and nothing blocks the worker run takes another slice
  right away instead of exiting, within its five-minute run. It is due again a minute after a slice that removed
  something, a minute after one that removed nothing with the lap still
  unfinished, and 5, 10, 20, then at most 30 minutes after slices in a row
  that reached the end of the lap with nothing removed. A priority checkout
  kept for a stable reason (ownership, a missing directory, ignored
  leftovers) is not judged ahead of the lap again until its state changes. A cleanup request whose
  grace ends after the last steady slice makes the next one due at once, so a
  landed checkout goes within about a minute of its grace ending (the next
  broker open wakes the worker). Steady removal is due only while some closed
  session still holds its checkout: a repository with none never wakes the
  worker for it. A repository whose closed checkouts are all kept (each with
  a keep code) costs one short worker run about every 30 minutes, started by
  a broker open. A run that cannot reach its slice (every host judging
  permit is taken, the runtime cannot be verified, the worker cannot start,
  no time is left) holds steady removal off for one to five minutes. A
  removal that finds another repository's worker deleting waits at most two
  seconds, then is put off: the slice stops `host_deleter_busy` and the
  checkout is judged again (the priority lane or the lap reaches it first)
  about a minute later.
- From three quarters of the limit it runs the `pressure` slice with the
  capacity limits (up to twelve removals in 120 seconds, shorter retries).

Both triggers use the same proofs, pacer, memory and load gate, GC lock gap
and yield to foreground commands, and `[cleanup] auto_remove = false` turns
both off. A running gate keeps a closed checkout (`gate_running`) only when
one of that checkout's own sessions runs it, when the gate's recorded
checkout is the checkout, inside it or contains it, or when nothing
identifies where the gate runs (a record from an older binary, or a
malformed one); a gate in another session's checkout does not keep it.
`kept[].gate_keep` and the `gate_checks` counters say which. The worker's last slice, with its `trigger`, is
`capacity_removal` in `aethyme broker status --json` and
`auto_cleanup.capacity_removal` in `aethyme broker gc plan --json`; see
"Automatic removal" in [worktree cleanup](worktree-cleanup.md).

## Background deletion pace

The detached cleanup worker deletes finished sessions' build output while
nobody waits on it, so it runs as a background job: lowest CPU and disk I/O
priority (macOS and Linux), a rate limit on unlinks, one deleter at a time
across every repository on the machine, a throttle while memory pressure
is at the warning level and a stop while it is critical. Commands you wait on (`finish`, `finish cleanup <id>`,
`gc apply`, `gc sweep`) keep full speed.

The one-deleter rule is a host-wide slot the worker holds only while it
deletes: one checkout removal and the rate-limit sleep that pays for it, or
a run of file-deletion batches (handed over after 20 seconds). Judging,
counting, lock gaps and pressure waits never hold it, and each deletion waits
at most two seconds for it; if another repository's worker is still
deleting, this one goes back to judging and retries shortly. Lanes that
delete in ways the pacer cannot see batch by batch (checkout archives, the
requested checkout cleanup) hold it for their whole pass, waiting at most ten
seconds. Session retirement deletes nothing (closing a session and finishing
a session whose checkout is gone only write rows), so it takes no slot. At most two repositories judge closed
checkouts at the same time; a third waits up to five seconds and otherwise
retries a minute later. The slot and the judging permits are released by
the operating system if a worker dies.

Critical memory pressure (macOS level 4, or Linux memory pressure at its
limit) is a hard stop: no removal starts while it lasts, and when it
outlasts the worker's pause budget (120 seconds per run) the work stops and
waits for a later run. Memory pressure at the warning level (macOS level 2,
or a level that cannot be read) is a throttle, like load: the worker waits
at most a few seconds for it to clear, then goes on with one checkout
removal per slice and at least 2 minutes before the next slice
(`stop_reason` `memory_warning_throttled`, a wait no new landing shortens);
file deletion waits up to 1 second per batch of 256 files. A machine that
sat at the warning level for hours used to stop every removal for 10
minutes at a time. A high load average is only a throttle, never a stop:
on a busy machine it can stay far above the core count for days while memory
is fine. While the one-minute load average is above twice the core count (or,
on Linux, I/O pressure is high) the worker waits at most a few seconds for it
to clear (5 seconds before a checkout removal, 1 second per batch of 256
files) and then goes on: a checkout removal slice removes one checkout and
the next slice runs at the normal cadence but at least 30 seconds later, so
workers do not remove back to back while load stays high, and file deletion
runs at about a
fifth of the configured rate (about 200 files a second at the default).

```toml
[cleanup]
# Unlinks per second for the background worker. Default 1000.
# 0 or "unlimited" restores full speed.
unattended_unlinks_per_second = 1000
```

The limit is lifted automatically while free space is below the gate
headroom, because freeing space is then the priority; the lowered priority,
the memory pause and the load throttle stay. The limit also applies under
load, on top of the load wait. Nothing deferred is dropped: a worker that pauses,
reaches its time limit, or finds another repository's worker deleting
leaves its request durable, and the next broker open starts a worker again.

`aethyme broker status --json` shows the last worker run under
`cleanup_retention.last_artifact_sweep.unattended_pacing`: entries and bytes
removed, the removal rate, time slept by the limiter, time paused for memory
and waited for load, how often work went on throttled under load
(`load_throttled`), time waited and throttled at the warning memory level
(`memory_warning_pause_ms`, `memory_warning_throttled`, omitted when zero), time waited for, held and spent deleting under the host
deleter slot (`host_lock_wait_ms`, `host_slot_held_ms`,
`host_slot_deleting_ms`, `host_slot_busy`), time waited for a judging permit
(`judge_permit_wait_ms`, `judge_permit_busy`), requests coalesced into the
run, and whether priority lowering succeeded. The worker's
`command-metrics.jsonl` row carries the same waits as phases
(`artifact_worker_host_deleter_wait`, `artifact_worker_host_slot_held`,
`artifact_worker_host_slot_deleting`, `artifact_worker_judge_permit_wait`,
`artifact_worker_pacer_memory_pause`, `artifact_worker_pacer_load_wait`,
`artifact_worker_pacer_memory_warning_wait`,
`artifact_worker_pacer_limiter_sleep`, and the counts
`artifact_worker_pacer_memory_warning_throttled` and
`artifact_worker_capacity_removal_memory_warning_throttled`); it is written
when the run exits: normally within about five minutes of its start, plus up
to the 30-second retirement reserve when checkout removal overran the run,
and longer when the read-only shared-cache measurement runs after the
worker lock is released. See
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
| `landed_sessions_declined` | Sessions it kept, counted per reason code (`recent_activity`, `changed`, `process_alive`, `holder_unbound`, `holder_unknown`, `lease_held`, `lease_unknown`, `worktree_unavailable`, `busy`, `operation_active`, `operation_unknown`, `policy_unavailable`, `auto_remove_disabled`, `pinned`, `pull_request_unmerged`, `no_session_commits`, `no_landing_evidence`, `not_landed`, `landing_unknown`, `worktree_unclean`, `moved_before_close`, `upstream_moved`, `budget_exhausted`, `adopted`, `holder_alive_current`, `holder_alive_in_use`); omitted when empty. `holder_alive_current` (formerly `holder_alive`): the agent process still runs and this is its latest session; `holder_alive_in_use`: it moved on, but a process has its working directory or an open file in the checkout. A session the abandonment window then closed is not counted. |
| `retirement_cursor` | `{after_session_id, high_water_session_id}`: where the open-session retirement lap stood when the report was written; omitted before the first lap. |
| `retirement_cursor_start` | The same position when the first pass of the report began; equal to `retirement_cursor` when the lap did not move. Shown as `landed_session_retirement.cursor_start`. |
| `abandonment_sessions_examined`, `abandonment_sessions_closed`, `abandonment_sessions_kept` | Sessions past `session_abandoned_after_hours` that the oldest-first lap asked the abandonment window, the ones it closed, and the ones it kept per reason code (`window_disabled`, `changed`, `session_pid_alive`, `follow_up_protected`, `worktree_activity_unavailable`, `recent_activity`, `holder_unknown`, `holder_alive_current`, `holder_record_unavailable`, `holder_shared_service`, `holder_moved_on_dirty`, `task_lease_current`, `operation_guard_busy`, `operation_guard_unavailable`, `lifecycle_busy`, `deadline_reached`, `gate_running`, `gates_unknown`, `operation_unresolved`, `operations_unknown`, `merge_queue_active`, `merge_queue_unknown`, `preparation_in_progress`, `preparation_invalid`, `preparation_unknown`, `checkout_not_directory`, `artifact_producer_active`, `artifact_protocol_unavailable`, `legacy_artifact_markers`, `open_files_in_checkout`, `open_files_unavailable`, `open_files_throttled`, `lease_held`, `lease_unavailable`, `database_busy`, `policy_unavailable`, `landed_declined`, `unrecorded`); omitted when zero or empty. Shown as `landed_session_retirement.abandonment` (`examined`, `closed`, `kept`) in `broker status --json`. Each keep also adds `session_abandonment_kept_<reason>` to the command's `phase_counts`. |
| `landed_retirement_elapsed_ms` | Milliseconds the landed lane spent; omitted when zero. |
| `merged_pull_request_sessions_examined` | Of `landed_sessions_examined`, spawned sessions with every pull request merged judged in the `session_merged_after_hours` window; omitted when zero. |
| `merged_pull_request_sessions_closed` | Of `landed_sessions_closed`, the ones that window closed; omitted when zero. |
| `sessions_closed_by_reason` | Sessions closed per lane reason: `merged_pull_request_landed`, `landed`, `abandoned`; omitted when empty. Shown as `landed_session_retirement.closed_by_reason` in `broker status --json`. |
| `sessions_closed_by_holder_evidence` | Sessions closed per holder evidence: `gone`, `moved_on`, `lease_expired`, `unbound`; omitted when empty. |
| `missing_checkout_examined`, `missing_checkout_finished`, `missing_checkout_kept` | Closed sessions whose checkout is already gone that the lane judged, recorded as cleaned, and kept per reason (`checkout_present`, `pool_root_missing`, `pool_root_unproven`, `volume_differs`, `volume_unproven`, `open_session_at_path`, `finish_recovery_pending`, `archive_intent_pending`, `git_registered`, ...); omitted when zero. |
| `retirement_evaluated_at_ms` | When the retirement counts were evaluated, present when a later pass whose retirement lane judged nothing carried them forward. |

Readers must accept these fields being absent in older saved reports. A deferred
session remains protected; `sessions_retired` counts session state transitions,
not removed directories or reclaimed bytes. The checkout grace begins from
closure, and content/ownership checks still decide whether removal is allowed.

A clean session whose work is on the fetched default branch can enter guarded retirement after `session_landed_after_hours` (default 24 hours), with no `finish`. The landing is the rule `finish` uses: a representation recorded against that branch, ancestry, or a content landing that accounts for every commit (a commit made after a squash keeps the session). The content search runs only when the cached pull-request listing records a merged pull request for the session, a representation exists, or the head is already an ancestor. Its recorded holder must be gone, or still running with an expired task lease and no gate, operation, preparation or open file in the worktree; the `expire_abandoned_task_leases` opt-in is not needed for a landed session. Set it to `0` to disable this earlier path. The ordinary `session_abandoned_after_hours = 0` also disables retirement, and `[cleanup] auto_remove = false` keeps every session out of it. Unknown holders, current task leases, unmerged linked pull requests, changed heads, dirty or unproven work are preserved, each counted under a reason in `landed_sessions_declined`. Checkout removal still has its separate grace period and safety checks.

A spawned session whose every recorded pull request merged closes earlier, after `session_merged_after_hours` (default 2 hours) without activity, counted from the later of its last activity and its last merge. It keeps every proof of the landed path (HEAD landed on the fetched default branch by the same rule, a clean checkout, a gone holder or an expired task lease, no explicit lease held) and adds: the broker spawned the session (an adopted session keeps the 24-hour path and is counted as `adopted`), at least one pull request is recorded and none is open or closed unmerged, and no gate, queue entry, coordinated operation, preparation or artifact producer of the session is running. The pull requests, HEAD, default-branch tip and checkout cleanliness are read again under the session and lifecycle claims immediately before the close; a commit, a new file or new activity in between keeps the session (`moved_before_close`). `0` disables this earlier path; `session_abandoned_after_hours = 0` disables it too. The close is the transition `finish` makes: the checkout stays on disk for its closed-checkout grace, then normal removal judges it. The `session.abandoned` event records `close_reason` (`merged_pull_request_landed` or `landed`), `head`, `upstream_tip`, `landing_evidence` (`representation`, `ancestry` or `content`) and `pull_requests`; an automation note from `automation:session-auto-close` tells the session why, and a session command run with `--session <id>` afterwards names the automatic close and suggests `aethyme broker start --reuse`. `merged_pull_request_sessions_examined` and `merged_pull_request_sessions_closed` count this path within the landed counts.

The abandonment window (`session_abandoned_after_hours`, default 72 hours, so 3 days) closes any open session without a live holder, whatever its pull requests or landing. Its `session.abandoned` event now also records `close_reason` (`inactive_no_pull_request` when no pull request was ever recorded, otherwise `inactive`), `head` and `unlanded_commits`. Closing deletes nothing: a closed checkout whose commits are not on the default branch is kept by automatic removal (`not_contained`) together with its branch until the work lands or a person decides, and the same note and refusal message apply.


### A live agent that moved on

One long-lived agent process often holds dozens of open sessions, starting one after another without finishing them. A running holder therefore keeps a session only while there is evidence it uses that session. The holder has moved on when the same process (pid and recorded start time) was bound to a different session after this session's last activity (`session.holder_bound` events; an agent turn only refreshes the session whose checkout it runs in). A moved-on holder no longer keeps a landed session (merged-pull-request or landed window): the session closes when nothing has its working directory or an open file in the checkout (with no exemption for the holder), no gate, operation, preparation, artifact producer, merge-queue entry or live explicit lease remains, and every other landed proof holds. The abandonment window treats a moved-on holder as gone only for a clean checkout (no uncommitted changes or unmarked tracked deletions). The holder's latest session is never moved on. Each close records `holder_evidence` (`gone`, `moved_on`, `lease_expired`, `unbound`) and, for `moved_on`, `holder_moved_on_to_session`. One open-file observation now answers every candidate for ten seconds (at most four per pass).

### Closed sessions whose checkout is gone

The background worker also finishes closed spawned sessions whose checkout no longer exists, recording them as cleaned exactly as a completed cleanup does, without deleting anything (the session branch is kept). It does so only after the session's closed-checkout grace (`closed_worktree_grace_hours`), and only when the pool root that held the checkout is on disk with this repository's root marker, the pool root is on the volume the spawn witness recorded (when it recorded one), Git no longer registers the path (a prunable registration keeps it until Git prunes it), no open session uses the path, and no finish recovery or archive intent is pending. Each finish records `session.checkout_missing_finished` with the evidence; up to 32 sessions per pass.

## Session retirement in the background worker

Every worker run, whatever lane started it, queues a session-retirement
request of its own when the abandonment window is enabled, nothing is
pending and the 10-minute pause after the last completed lap is over
(`artifact_worker_retirement_requested`; `artifact_worker_retirement_not_due`
when no request was due). Retirement then gets up to 120 seconds of the run,
and never less than 30 seconds even when checkout removal before it used,
failed or overran the run (`artifact_worker_retirement_share_budget`,
`artifact_worker_retirement_share_used`,
`artifact_worker_retirement_share_reserve_used`). It no longer waits for the
host deleter slot. A pass waits up to 5 seconds for the GC lock a
foreground command's inline pass may hold (`session_retirement_gc_lock_wait`,
`session_retirement_gc_lock_busy`) and, when it stays held, retries a minute
later instead of ten. Between passes it stops only on critical memory
pressure (`session_retirement_memory_backoff`); at the warning level it
waits at most a few seconds and goes on. Which sessions may close is
unchanged.

A session past the abandonment window by its session row whose checkout Git
metadata moved more recently is kept for `recent_activity`. Until that
activity plus the window (at most one window ahead, however the metadata is
dated), every retirement pass, foreground or background, skips it instead of
judging it again, and the `status`/`agents` view, which reads the memo but
never writes it, does not judge it either; that view also no longer judges
sessions whose row is still inside the window
(`agents_view_abandonment_not_due`) (`retirement_not_due_memo_skips`,
`retirement_not_due_memo_recorded`; meta key
`auto_cleanup.session_retirement_not_due`). A skip only delays a close; the
landed lane is still asked, and a changed window discards the memo.
