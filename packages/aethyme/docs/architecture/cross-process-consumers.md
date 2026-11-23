# Cross-process consumers of Aethyme entry points

Last Updated: 2026-11-17

When code outside the `packages/aethyme/` Rust source tree invokes an
Aethyme command, it crosses a process boundary. Static
analysis (ruff, cargo check, type checkers) does not see those
invocations. They have to be audited by hand.

This file is the canonical inventory: every shell script, hook, skill
template, deployed wrapper, and CI step that calls an Aethyme entry
point. **Before deleting or renaming any CLI entry point — Rust
binary subcommand or shell helper — grep this file for callers and
update each.**

The 2026-05-08 hard-delete of `python -m src.cli explore` broke the
deployed `aethyme-explore` wrapper because we hadn't tracked it as a
consumer. This file exists so that doesn't happen again.

## How to use this file

1. **Adding a new wrapper / hook / external invocation?** Add a row
   to the appropriate section below. Note the source path, the
   invoked Aethyme entry point, and what kind of failure surfaces if
   the entry point disappears.

2. **Removing or renaming an Aethyme entry point?**
   - Search this file for the entry point's name.
   - For each consumer listed: either update the consumer in the same
     commit, or add a backwards-compat shim, or accept the breakage
     with explicit reasoning.
   - If you find a consumer that's NOT listed here, add it before
     proceeding. The miss is itself the bug.

3. **Suspect something is stale?** Run
   `scripts/eval/verify-playground.sh` against any deployed
   playground; it asserts a subset of the contracts below.

## Session coordination consumers

Routine `broker status --json` in a registered worktree and
`broker status --session <id> --json` both return the
`aethyme.broker-session-view/v2` document. The earlier `--session-view` spelling
selects the same view and revision scope. Version 2 makes global recovery a
count summary and omits per-observation evidence from default advice. `session.next_action` is the current
session's next safe step. A running wait can supply an instruction with a null
command. `session.blockers` reports its own recovery findings,
`peer_state` scopes overlaps to its leases and declared scopes, and
`global_state` counts other recorded coordination warnings by kind. A repository
count can change without naming the record. Use `--full --json` for exact
evidence and `broker unblock` for recovery inspection. The broker verifies
exact resource conflicts when carrying out a requested operation.

With `--since-revision`, `aethyme.broker-status-delta/v1` carries section
replacements for a valid revision of that same session. Old focused compact and v1 session-view
tokens reset, as do tokens from other scopes. An empty section map leaves deferred checks
unknown.

`aethyme broker advanced events --session <id> --since <event-id> --follow
--json` emits NDJSON rows attributed to that session, in event-id order. It
omits global and peer-attributed events, and cannot recover rows removed by
retention. Clients persist the last consumed id and fetch focused or full
status when they need current state or evidence. The installed, trusted hook
plugin delivers changes to the session, relevant recorded peer overlaps and
blocker observations at prompt and tool boundaries, alongside pending lease
release requests. Session and blocker checks run at most once every 30 seconds
during a continuing session. An idle agent receives updates on its next turn.

## Code-shape diagnostics

`aethyme quality code-shape` is an opt-in CLI for source-shape diagnostics.
Its JSON output has `schema_version: 1` and `kind: code_shape`. Error mode
returns 1 for findings and 2 for incomplete analysis or operational errors;
warning mode returns 0 for findings. Its configuration, file scope and
limitations are described in [code-shape.md](../guides/code-shape.md).
The initial external consumers are manual operators and opt-in CI scripts.

## Opt-in broker service transport

`aethyme broker advanced service serve <private-runtime-directory>` runs a
foreground Unix health service. `service ping <private-runtime-directory>
[--json]` emits the versioned JSON health response. No service is started
implicitly and existing broker commands keep their direct execution path.
Neither command opens or upgrades repository state. The directory is explicit,
canonical and private to the current user. Frames are bounded to 65,536 bytes,
versions and request IDs are checked, and unavailable endpoints fail visibly.
The exact wire schema, permission rules and staged migration are documented in
[broker-service.md](broker-service.md). Initial external consumers are manual
operators; no installed hook, scheduled task or application is migrated yet.

## Transcript archiving, on by default (`aethyme transcripts`)

`aethyme transcripts status|list|archive --dry-run|--apply|restore
<session-id|path>|enable [--after-inactive-days <n>]|disable [--json]` is a
host-level operator surface. JSON output names its shape:
`aethyme.transcripts-status/v1`, `aethyme.transcripts-list/v1`,
`aethyme.transcripts-archive/v1`, `aethyme.transcripts-restore/v1`,
`aethyme.transcripts-settings/v1`; refusals are
`aethyme.transcripts-error/v1` with `code` and `message`. Its input is
`<host state>/transcripts/settings.toml` (`[transcripts] archive`, **on by
default** since 2026-11-17 when the file is absent or omits `archive`;
`archive_after_inactive_days`, `archive_after_inactive_days_claude`, with
the older `archive_after_days*` names still read and losing to the new
ones); `enable` and `disable` write only that file (`--after-days` stays an
alias of `--after-inactive-days`). Eligibility is inactivity: the newest of
the file's mtime, its last record's `timestamp` (tail only) and its linked
broker sessions' `last_activity_at` must be older than the setting.
`status --json` gains `enabled_source` (`default`|`settings`, also under
`config`) and `first_run` (`gate`, `noticed_at_ms`, `acknowledged_at_ms`,
`acknowledged_by`, `automatic_archiving_from_ms`,
`automatic_archiving_from`, `notice`, `commands`); `config` renames
`archive_after_days*` to `archive_after_inactive_days*`. Archive reports gain
`enabled_source`, `first_run_gate`, `record_tails_read` and the skip reasons
`recent_record` and `recent_session_activity`. On a default-on host
`aethyme broker status` shows the advice `transcripts.archiving-default-on`
(severity notice; in every status form including compact, its summary
names the start time) and records when in
`<host state>/transcripts/first-run.json`; `broker start` text output
prints the same notice while the wait runs;
the background worker archives nothing until 24 hours later, unless
`enable` or a manual `archive --apply` (recorded as `acknowledged_by`) skips
the wait. It writes `<host state>/transcripts/` (`index.sqlite3`,
`runs.jsonl`, `archive.lock`, `first-run.json`, `claude/`), Codex's
`archived_sessions/*.jsonl.zst`, and reads broker databases read-only. The
external consumers are Codex itself (`codex unarchive` and `codex resume`
read the `.jsonl.zst` archives natively) and manual operators. A reader of
`status --json` `config.archive_after_days` must read
`config.archive_after_inactive_days`; a host that relied on the old
off-by-default must write `archive = false` (`aethyme transcripts disable`).
Since 2026-11-17 one host-wide open-file observation serves a pass's
judging for up to 30 seconds, and originals are removed in batches (200
copies or 60 seconds) judged against one observation taken after the
batch's last copy was verified, instead of one listing per provider root
every 2 seconds plus one per removed original; `open_listings` counts
those observations. Archive reports and `runs.jsonl`
lines gain the additive counters `open_observation_refreshes`,
`open_files_judged`, `open_files_judged_max_per_observation`,
`removal_batches`, `removals_refused_open`, `removals_refused_unobserved`
and `removals_refused_changed` (each also counted under its skip reason),
`runs.jsonl` gains `open_listings`, `phases_ms` gains `transcripts_remove`,
and lifetime counters gain the three `removals_refused_*`.
See [the guide](../guides/transcript-archiving.md).

## Opt-in MCP server launching (`aethyme mcp`)

`aethyme mcp connect <name> [--from-worktree] [--fallback-cwd <dir>]
[--fallback -- <command...>]` is meant to be the command an agent's MCP
configuration runs. It replaces its own process with the resolved server, so
its stdout is the server's protocol stream and everything it says goes to
stderr; a missing entry exits 3, a failed resolution 6, and `--fallback` runs
the given command instead. `aethyme mcp resolve <name> [--json]`
(`aethyme.mcp-resolve/v1`), `aethyme mcp status [--json]`
(`aethyme.mcp-status/v1`) and `aethyme mcp config print --agent codex|claude`
are read-only operator surfaces; `aethyme mcp trust [--repo <path>] [--json]`
(`aethyme.mcp-trust/v1`) records host trust for a repository file; refusals
under `--json` are `aethyme.mcp-error/v1`. Its inputs are
`<host state>/mcp/services.toml` and `.aethyme/mcp.toml` (`schema = 1`); it
writes only under `<host state>/mcp/` (`events.jsonl`, `resolved/`,
`identity-cache/`, `key-secret`, `trusted-repositories.json`). Nothing
installs it: the external consumers are agent configurations an owner edits
by hand from `config print`, so renaming `connect` or its flags breaks those
configurations (the fallback keeps the agent working only while `aethyme mcp
connect` itself still parses). See [the guide](../guides/mcp-servers.md).

`aethyme mcp hub serve <directory>` is the internal hub process; its only
consumer is `aethyme mcp connect` of the same build, which starts it and
speaks a line-delimited JSON attach protocol over a 0600 socket in
`<host state>/mcp/hub/` (versioned `"hub": 1`, build identity checked).
`aethyme mcp trust` now records only after a terminal confirmation or
`--yes --digest <sha256>`; its JSON adds `trusted`, `approval` and `reason`.
`status --json` adds `hub` (state, pid, upstreams) and per entry
`hub.upstream`, `hub.counters`. For `transport = "http"` entries the attach
hello adds `"transport": "http"`, `"path"` and `"auth"`, and the hub answers
with one `{"aethyme_hub": "ready", "url", "token"}` (or `fallback`) line; the server's port is
a `tcp_port` lease (`repository = "aethyme-mcp-hub"`) in the host resource
registry, and `status --json` adds `port` per upstream and
`last_listen_ms` / `http_sessions_ended` per key. Entries gain
`require_argv`, `http_path`, `http_auth` and `http_unauthenticated`.
Readiness of a private HTTP server is timed by the phase span
`mcp_http_listen_wait`; each re-check after the two macOS listener
snapshots raced (the group held more listeners than `netstat` showed) adds
one `mcp_http_listen_transient_retry` count (at most five per wait; the
hub's own watcher re-checks the same way, and its count lands only where a
phase recorder is installed on that thread). Refusal reasons are unchanged: a race that
persists still ends as `listener_unverifiable:listener_listing_empty` or
`listener_unverifiable:listener_count_mismatch`.

Managed servers add two more external consumers. `aethyme mcp intercept
install` creates symlinks named after launchers (`npx`, `npm`, `pnpm`,
`pnpx`, `bunx`, `yarn`) and managed executables that point at the `aethyme`
binary: the binary dispatches on `argv[0]`'s basename before anything else,
and reads `<host state>/mcp/intercept.match` (a line-based file it writes
itself, header `aethyme-mcp-intercept 1`); the environment variables
`AETHYME_MCP_SHIM_GUARD`, `AETHYME_MCP_SHIM_DEPTH` and
`AETHYME_MCP_SHIM_HOPS` are part of that contract, as are
`AETHYME_MCP_CONNECT` and `AETHYME_MCP_FALLBACK`, which `connect` sets on
what it starts and `bypass` reads. `aethyme mcp repair --apply` writes agent
configurations (the Codex `config.toml`, `~/.claude.json`, an untracked
`.mcp.json`) so their entries run `<absolute aethyme> mcp connect <name>
[--caller-args <n> [--caller-package <spec>]] --fallback -- ...`, which
makes `connect`'s spelling, including the caller-argument options, a
consumer contract there too. The manage, intercept,
repair and bypass commands emit `aethyme.mcp-manage/v1`,
`aethyme.mcp-intercept/v1`, `aethyme.mcp-repair/v1` and
`aethyme.mcp-bypass/v1`; `aethyme hook SessionStart` reads
`<host state>/mcp/settings.toml` (`[mcp] repair_on_session_start`).

The `service::jobs::JobStore` library adds a separate explicitly opened
SQLite job file (application ID `0x41534a31`, schema 1), durable idempotency keys,
atomic deduplication and fenced job state transitions. It has no CLI or socket
consumer in its foundation phase; plain serve/ping never open job storage.
Its typed operation describes GC planning only. See the durable-store section
in [broker-service.md](broker-service.md) for identities, state transitions,
deadlines, cancellation, result bounds and restart limitations. The legacy
broker database and its schema version are unchanged.

The dependent opt-in worker adds `service serve <directory> --jobs-for <repository>`
and `service submit`, `lookup`, `status`, `cancel` and `result`. Submit requires
an explicit request key and absolute job deadline; optional flags select full
scan and active gate-cache inspection. A private v2 JSON-line protocol accepts
only typed GC planning, scoped to the server-verified repository UUID and
filesystem identities. It provides durable lookup, shared outcomes and chunked
results alongside health protocol v3. Submit/lookup/status/cancel print the v2
Response envelope; result prints the assembled plan JSON. Exact shapes, null
semantics, bounds, restart fencing and ambiguous-response handling are specified
in [broker-service.md](broker-service.md). These are
manual operator surfaces; existing scripts and automatic jobs are not migrated.

The dependent identity phase adds `service repository <directory> [--json]`,
the v2 `repository` method/reply, an optional top-level `repository_key` request
string and the `repository_mismatch` error. Explicit null is refused; absent
scope retains the legacy wire format. `transport::scoped_job_request` pins every
operation to the verified service owner before dispatch. Identity fields and
mixed-version refusal behavior are specified in [broker-service.md](broker-service.md).

The dependent GC client phase adds `broker gc plan --service-directory <directory>
--request-key <key> --deadline-ms <utc-ms>`. These three options are required
together and are refused by other actions. This explicit path verifies the
locally selected repository, pins all requests, and returns the existing plan
JSON or human output. With none of these options, planning retains its direct
path. Ambiguous outcomes
never trigger a local fallback or an automatic retry; recovery and deadline
semantics are specified in [broker-service.md](broker-service.md).

`aethyme env shared preview <name> [--wait <seconds>] [--json]` starts the
checkout's `[[shared]]` profile as a separately owned worktree environment.
Its `aethyme.env-shared/v1` output adds `preview: true` and `shared_name`,
alongside the environment ID and URLs. It does not change the host's serving
shared alias; remove the preview with `aethyme env down <id>`. Preview uses
checkout contents, including uncommitted edits, and refuses profiles that
themselves require shared environments.

`broker gc plan --exclude-orphans` and the matching `gc apply --exclude-orphans`
omit orphan worktree roots from the reviewed candidate set. The plan carries an
`orphans_excluded` blocker, its digest binds that scope, and apply recomputes the
same scope before acting. Overall retained-storage budget verdicts remain
unknown when they cannot be proved without the omitted roots. The same option
must be passed to plan and apply. This option is local-only and is refused with shared-service planning. Default GC remains
unchanged and refuses incomplete orphan inventory.

The dependent discovery phase adds `service directory <private-service-root>
--jobs-for <repository> [--json]`. Both output modes emit one object:
`schema_version: 1`, `directory`, and the full `repository` identity (`key`,
`repository_uuid`, `root`, `common_dir`). It resolves main and linked worktrees
to one address under an existing caller-selected private root, refuses unsafe
or overlong paths, and creates no directory, service or broker state. The
shortened directory name is an address only; consumers must still verify the
full repository identity before requests. Read-only discovery is specified in
[broker-service.md](broker-service.md).

The dependent startup phase adds `service ensure <private-service-root>
--jobs-for <repository> [--json]`. Its JSON flattens the discovery object and
adds `pid`, `version`, and `started`. Explicit ensure creates only the private
leaf and service coordination files, serializes startup, and reuses only a
version-compatible service with the complete verified repository identity.
Failed owned children are stopped and reaped; existing incompatible endpoints
are preserved. It submits no work and enables no default routing. Startup
budgets, log behavior and lifecycle limits are specified in
[broker-service.md](broker-service.md).

## Host storage ledger (`aethyme storage`)

`aethyme storage status [--json]` (`aethyme.storage-status/v1`) and `storage
run [--json]` (`aethyme.storage-pass/v1`) are operator surfaces; `storage
worker` is internal, started detached by broker commands. Files, all in the
host state directory: `storage-ledger.json` (the ledger, written by the worker
only), `storage-ledger-summary.json` (what the start-of-command check reads),
`storage-worker.lock` (pid, start time, heartbeat, token; a dead or stalled
owner's lock is taken over), `storage-roots.json` and
`storage-gate-budgets.json` (worktree roots, adopted storage roots and gate
cache budgets), `storage-ledger-dirty/` (producers' notes of entries they
wrote; gate runs write one per managed cache), and the optional owner file
`storage.toml` (`[budget]`). `aethyme storage adopt <dir>` writes
`.aethyme-storage-root` into a directory; a storage root must carry it. `aethyme
storage archives [--json]` (`aethyme.storage-archives/v1`) and `storage
keep-archive <id> [--unkeep]` (writes `<archive>.aethyme-keep` beside a
recovery archive) are operator surfaces; `storage.toml` `[budget]` adds
`unlanded_archive_days`, and `broker status --json` adds advice
`storage.archives-expiring`.
`broker status --json` adds advice `storage.over-budget`,
`storage.below-floor` and `storage.unclassified` (class names and sizes; no
paths outside Aethyme's roots). `AETHYME_STORAGE_LEDGER` (`0`/`1`) turns the
start-of-command check off or on.

Host temp folders (2026-11-22): after each pass the worker reads the per-user
temp folder (macOS `confstr(_CS_DARWIN_USER_TEMP_DIR)`, never `$TMPDIR`) and
`/tmp`, at most every 10 minutes, into
`<host state>/storage-ledger-host-temp/reading.json` (`updated_at_ms`,
`roots[]` with `kind` `user_temp`|`system_tmp`, `path`, `entries`,
`entries_capped`, `other_users_entries` (counted, never sized),
`aethyme_entries_excluded` (the broker's `ae<uid>` alias directory),
`bytes`, `unmeasured_entries`, `partial`, `free_bytes`,
`total_bytes`, `walked_entries`, `measure_ms`; `history[]` hourly samples,
72 per root), `sizes.json` (per-entry sizes keyed by a 16-hex-digit
SHA-256 of a per-host salt and the name; no names stored) and `salt` (32
random bytes, created once, mode 0600), classified `coordination`. The walk
lists at most 100,000 entries per root, sizes only entries owned by the
worker's user, shares a 10-second deadline across roots, reads directories
lazily, checks the deadline and the worker heartbeat every 256 children,
queues at most 4,096 directories per entry (beyond that the entry is
partial), never follows a link or leaves the root's device, stops without
writing when the worker lock is lost, and deletes nothing. A reading stamped
in the future does not block the next one and advises nothing. Known limit:
each top-level entry is `lstat`ed, which a dead network or automount mount
can block with no timeout.
`aethyme storage status --json` adds `host_temp` (the reading); `broker
status` adds advice `storage.host-temp-large` (notice; warning when the volume
is below `host_temp_free_floor_bytes`) from the reading only, skipped when the
reading is over a day old. `storage.toml` `[budget]` adds
`host_temp_advice_bytes` (8 GiB), `host_temp_advice_entries` (5,000) and
`host_temp_free_floor_bytes` (10 GiB); an older binary on the same host (a
pinned GC service runtime included) rejects a file that sets them and runs
with default budgets, so set them only once every binary on the host is at
least this version. Span `storage_host_temp_measure` times the
reading; counters `storage_host_temp_root_measured` (one per root, with its
time), `storage_host_temp_partial` (a root left partial by the deadline) and
`storage_host_temp_lock_lost` (a reading stopped by a lost worker lock).

Recovery archive landing proofs (2026-11-23): whether an archive's commits
are in its repository's default branch is answered by one prover per process
(`storage_ledger::landing`), keyed by the repository path. Each default tip
is read once per pass and then reused for 30 s, but every "contained" answer
is confirmed against a freshly read tip before it is returned (a moved tip is
re-read, carried or reset, and the archive re-checked once). Every commit of
an archive is first checked against what is known, then the unknown ones are
probed (`merge-base --is-ancestor`) once per tip; a forward move of the tip
carries known answers with one `rev-list <new> ^<old>`, any other move drops
them. Every Git read (tips, ranges, probes) uses the resolved Git program at
an ordinary disk priority, with `core.useReplaceRefs=false`,
`GIT_NO_REPLACE_OBJECTS=1`, `GIT_GRAFT_FILE` set to the null device,
`GIT_NO_LAZY_FETCH=1`, and inherited `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_INDEX_FILE`, `GIT_COMMON_DIR`, object-directory, namespace, replace-base,
shallow-file and config-parameter variables removed; each read gets at most
20 s, what is left of an active Git deadline, and what is left of 120 s for
all of the process's reads together. `<host state>/storage-landing-cache.json`
(`schema` 1, `repositories[]` of `repository`, `tip`, `not_contained[]`;
written by the worker after eviction, classified `coordination`) keeps only
commits proven not contained that this process asked about, so a damaged or
stale file can keep an archive, never remove one, and a removed archive's
commits drop out. The pass report (`storage-ledger.json` `last_pass`, and
`storage status --json`) adds `landing`: per-pass counts `tip_reads`,
`tip_memo_hits`, `live_tip_checks`, `live_tip_moved`, `duplicate_commits`,
`cached_contained`, `cached_not_contained`, `probes`, `probe_contained`,
`probe_not_contained`, `probe_unproven`, `probe_timeouts`, `budget_spent`,
`tip_advanced` and `tip_reset`; and `unproven_archives` (archives kept
because their landing could not be proven either way: a budget, a deadline,
a Git failure, a moving tip; their skip reason starts `landing not proven`
and they are not counted in `unlanded_archives`). Spans
`storage_landing_default_tip`, `storage_landing_probe`,
`storage_landing_tip_advance`, `storage_landing_cache_load` and
`storage_landing_cache_save`; counters `storage_landing_tip_memo_hit`,
`storage_landing_live_tip_check`, `storage_landing_live_tip_moved`,
`storage_landing_duplicate_commit`, `storage_landing_cached_contained`,
`storage_landing_cached_not_contained`, `storage_landing_probe_contained`,
`storage_landing_probe_not_contained`, `storage_landing_probe_unproven`,
`storage_landing_probe_timeout`, `storage_landing_budget_spent`,
`storage_landing_tip_advanced` and `storage_landing_tip_reset`.

## Build output of kept worktrees

`broker finish --json` adds `cleanup.artifact_bytes_reclaimed`: bytes of
recognised build output (for example a Cargo `target` holding `CACHEDIR.TAG`)
removed from a closed session's worktree that finish kept. It is `0` when the
worktree was removed (that size stays in `cleanup.reclaimed_bytes`) or nothing
was reclaimed; readers of saved reports must default it to `0`.
`broker finish cleanup --all-cleaned --json` adds
`artifact_directories_reclaimed` and `artifact_bytes_reclaimed`; `--apply`
runs the same guarded closed-session reclaim on every retained checkout it
did not remove. Both are `0` for a read-only plan. `retention.artifact_sweep_budget_ms = 0`
or `[cleanup] strict = true` still disables the reclaim.

## Deferred post-close reclaim

`broker finish --json` adds `cleanup.reclaim_deferred` (bool) and
`cleanup.reclaim_worker_pid` (integer). Both are omitted unless finish could
not queue post-close checkout cleanup because another process held the GC
lock (`.aethyme/gc.lock`), normally the background reclaim worker; the pid is
that lock's owner when the lock file names one. `reclaim_deferred: true` means
reclaim was handed off, not skipped: the session's durable cleanup request is
kept, the running worker re-reads that request after its cache lane, and any
later broker open re-queues it. The checkout itself is not removed before the
request's closed-worktree grace period ends either way. The kept checkout's
build output is a separate rule (changed 2026-11-12): it is due under
`retention.artifact_reclaim_days` (default `0`, at once) whether or not finish
got the lock. When the lock is held, the closed-session reclaim records a new
artifact-sweep recovery generation instead of nothing; the holding worker laps
again for a generation that arrived during its lap (its pages stop at the
newest session seen when the lap began), and any later broker open admits a
worker for it. Command metrics count each such hand-off as
`closed_session_artifact_reclaim_deferred_gc_locked` in `phase_counts`.
Before this change the build output waited for the next periodic lap
(`artifact_sweep_interval_hours`, 24 h by default) or for checkout removal.
`cleanup.attempted` and `cleanup.artifact_bytes_reclaimed` keep their meaning.
Readers of saved reports must treat a missing field as `false` / absent.

## Whole-root reclaim plans are budgeted (0.8.110)

`aethyme broker gc reclaim plan` without `--session` runs within a 60 s
discovery budget and a 20 s sizing budget (`--full-scan` lifts both). Readers
of `gc reclaim plan --json`, of the review snapshots, of the worktree root and
of `command-metrics.jsonl` see these changes:

- **`candidates[].bytes` may be `null`** in a whole-root plan: the candidate
  was not measured in budget. It was always an integer before. A `null` size
  contributes nothing to `reclaimable_bytes` or `total_bytes`, which are then
  lower bounds. Sizes never enter the digest. `reclaim apply --json` output
  is unchanged.
- **`confirmable`** (bool, always present). `false` when discovery stopped
  before every worktree was walked. Such a plan's `digest` is still present
  but is salted so it matches no complete scan, no review snapshot is saved
  for it, and apply refuses it. Its human output prints no digest and no apply
  command.
- **`discovery`** (whole-root plans only): `complete`, `budget_ms` (`null`
  under `--full-scan`), `worktrees_scanned`, `worktrees_not_scanned`,
  `elapsed_ms`. A listed worktree is always complete; the one in progress when
  the budget runs out is finished, so `elapsed_ms` can exceed `budget_ms` by
  one worktree's walk.
- **`sizing`** (whole-root plans only): `budget_ms` (`null` under
  `--full-scan`), `measured`, `reused`, `unmeasured`, `budget_exhausted`,
  `elapsed_ms`, and `deferred_to` (`{pid, process_start, started_at_ms}`,
  omitted unless this plan waited out its budget behind another whole-root
  plan). The budget covers that wait and the walk together. A `--full-scan`
  plan never defers.
- **Two bookkeeping files in the worktree root**, both private to the broker
  and written through synced temporaries:
  - `.aethyme-reclaim-plan-sizes.json`: `{"schema_version": 1, "root",
    "entries": [{"path", "bytes", "measured_at_ms"}]}`, sizes measured by
    whole-root plans. An entry is reused for 90 s after it was measured. A
    measuring plan rewrites the file every 5 s while it runs, stamping each
    batch with its oldest measurement's time, and once more at its end.
  - `.aethyme-reclaim-plan-in-flight.json`: `{"pid", "process_start",
    "started_at_ms"}` while a whole-root plan runs. It is ignored once that
    process is gone. A stale marker is first renamed to
    `.aethyme-reclaim-plan-in-flight-stale-<pid>-<nanos>-<n>.json`, and only
    removed if it still holds the bytes judged stale; a live plan's marker
    moved by mistake is linked back.

  Their temporaries are named `.aethyme-reclaim-plan-tmp-XXXXXX.json`. Every
  one of these names matches the `.aethyme-reclaim-plan-*.json` pattern that
  root inventories already treat as broker bookkeeping, so a leftover from a
  killed process never stops an otherwise empty root from reading as empty.
  None of them is a 64-hex review snapshot name.
- **`reclaim.plan` metric row** (one per plan that ran): `phase_timings_ms`
  (`discovery`, `protection`, `in_flight_wait`, `sizing`), `phase_counts`
  (`worktrees_scanned`, `worktrees_not_scanned`, `candidates`, `measured`,
  `reused`, `unmeasured`, `size_shares`) and `reclaim_plan` (`scope`,
  `full_scan`, `discovery_complete`, `budget_exhausted`, `reused`,
  `deferred_to_pid`, `confirmable`). A plan writes no outcome event.
- **`reclaim.apply` metric row and outcome event** (new: apply used to record
  neither). Every apply writes a `broker.command.succeeded` / `.failed` event
  and a row, refusals included. Once it has scanned, the row carries
  `phase_timings_ms` (`scan`, `protection`, `review`, `removal`),
  `phase_counts` (`candidates`, `reviewed`, `authorized`, `removed`,
  `withdrawn`, `removal_failed`, `not_reviewed`, and `kept_<class>` for each
  protection that kept candidates: `active_session`, `unrecorded_worktree`,
  `not_reviewed`, `recent_work`, `open_file_use`, `other`) and
  `reclaim_apply` (`scope`, `review`: `exact` | `saved_review`, `refused`:
  `null` | `scope_mismatch` | `review_unavailable` | `review_unreadable` |
  `root_unbound` | `root_changed`, `reclaimed_bytes`). Kept reasons are
  free text that can name paths and processes, so only these labels reach
  the row.
- **Spans and attached counts.** When a command attaches `phase_counts` or
  `phase_timings_ms` that is not an object, the row's value is replaced by an
  object holding the recorded spans instead of the spans being dropped.

Readers must accept a `null` `bytes` and treat a missing `confirmable` (rows
and documents from before 0.8.110) as `true`.

## Submit: peers' changed paths reused, verification slot into the trash (2026-11-21)

- **Peer scans reuse watcher-proven answers.** A lease refresh scans each
  coordinating session's changed paths. On the busiest repository that is
  about 31 sessions per submit, each `git diff` plus `ls-files` taking about
  1.5 s, which is 47 s of summed worker time and 7.4 s of wall time
  (`implicit_lease_scan_all`).
  - Only the acting session's answer feeds a decision. Every other session's
    paths feed overlap reports, which never refuse (`audit_submit_ownership`
    reports another session's overlapping lease and never refuses it).
  - Peer scans therefore reuse the worktree watcher's stored answer in any
    command, `submit` included. That answer is keyed on the watcher counter,
    the baseline commit and the index identity, and HEAD is not part of
    `git diff <base>`.
  - The acting session, and every lease-claim scan, still asks Git.
  - Counter (introduced): `changed_files_peer_reuse`, counted when a peer scan
    uses the watcher in a command that does not otherwise allow reuse.
    Hits and misses keep the existing `changed_files_reused` and
    `changed_files_miss_<reason>` counters.
- **The verification slot goes into the trash.** At the end of submit, the
  merge-simulation slot's worktree is moved into the trash of the directory
  holding the slots, with one same-volume rename. Its Git registration is
  dropped, and the paced host deleter unlinks it later. This replaces Git
  removing the whole tree inline, which took 21 s per submit on the busiest
  repository.
  - The trash row is `stripped` and `verification` (a new row field),
    recorded for the submitting session. It is never read as that session's
    checkout being in the trash, and its bytes count as neither strip nor
    scratch. The drain counts them in `verification_entries_removed`, and the
    counter is `verification_slot_trash_entries_removed`.
  - A slot placed in the temporary-directory fallback (`<temp>/aethyme-run/`)
    keeps Git's removal (counter: `verification_slot_trash_skipped_temporary`).
  - The drain now drops rows whose trash directory's parent no longer exists
    and is not an unmounted removable volume: nothing is left to delete there.
    It counts them in `vanished_rows_dropped`, and the counter is
    `checkout_trash_vanished_rows_dropped`. Such rows used to be kept forever
    and held the trash "behind".
  - A lock someone else placed, environments that cannot be destroyed, or a
    refused rename fall back to Git's removal as before.
  - Span (introduced): `verification_slot_trash`.
  - Counters (introduced): `verification_slot_trashed` and
    `verification_slot_trash_fell_back_to_git`.

## Early close when nothing can be lost (2026-11-20)

The abandonment window (`session_abandoned_after_hours`, for example `1`) may
now close an idle session before the window of its work class (`[cleanup]
stale_*_close_after_hours`), but only when nothing on the session can be lost.
The rule is evaluated afresh on every look, and again under both claims right
before the close transaction. It runs cheapest first (`abandonment_safety.rs`)
and requires all of these:

- no merge, rebase, cherry-pick, revert or bisect in progress;
- no stash made on the session branch;
- every tip (HEAD and the branch) is contained in the default branch, in the
  session branch as the remote holds it now (`ls-remote`, never the local
  tracking ref, which outlives a remote deletion), or in a merged
  pull-request head. The diff base gets no exemption;
- no staged change, no unstaged change, and no untracked file that Git does
  not ignore.

Anything that cannot be read counts as unsafe. A session that fails the rule
keeps today's path: a closing-soon note, then the archive-verified close at
its class window. A session closing early tolerates no process in its
checkout, its holder's included: the open-file check runs whatever the holder
evidence says.

A follow-up session that the landed lanes protect also closes at the
abandonment window when it passes the rule. Without that, a one-hour window
would be forced up to the landed window for those sessions. `start --reuse`
resumes such a session at the same HEAD.

An agent's `PreToolUse` hook now counts as activity. The hook writes the file
`.git/aethyme/hook-activity-<session>.json` in the main checkout at most once
a minute, and the close path reads its modification time, so an agent that
only reads is not idle.

The work-class classification no longer drops a tip equal to the diff base.
An adopted session whose HEAD carries commits nobody pushed is now `unpushed`,
not `landed`.

- **Checked under the claims before closing.** If the rule fails on the
  first look but holds under the claims, the session is kept for that look
  (`session_abandonment_early_rule_first_held_under_claims`), because the
  process checks ran without the early-close strictness. The next pass starts
  from the rule holding and runs them in full.
- **Hidden index entries.** An entry flagged assume-unchanged or skip-worktree
  hides changes from status, so such a checkout counts as not examined.
- **Diff base.** The exemption differs by place. The early rule exempts no tip
  equal to the diff base. The work-class computation, which only picks the
  class window, drops such a tip when the base is contained in the upstream
  default branch or in the primary checkout's HEAD.
- **Reading the rule without a waiting period.** Each early close is recorded
  on its `session.abandoned` event as `early_close: {rule:
  "nothing_on_the_session_can_be_lost", checks: [...]}`, listing the checks it
  passed. The meta key `stale_sessions.early_closed` holds `{checkout path:
  at_ms}`, keyed by checkout so that a retired checkout reused as a new
  session still counts. A `start --reuse` of that checkout within 24 h counts
  `session_abandonment_safe_closed_then_resumed` and is recorded in
  `stale_sessions.early_resumed`. `broker status` attaches
  `session_abandonment_early_close_24h: {closed, resumed_within_24h}` and
  warns with `sessions.early-close-resumed` when any were resumed: the agent
  still wanted the session.
- **Counters (introduced).**
  - `session_abandonment_safe_closed`: a session closed by the early rule.
  - `session_abandonment_kept_unsafe_<reason>`, where `<reason>` is one of
    `in_progress`, `stash`, `head_not_contained`, `branch_not_contained`,
    `staged`, `unstaged`, `untracked`, `unreadable` or `not_examined`.
  - `session_abandonment_follow_up_safe`.
  - `abandonment_safety_safe` and `abandonment_safety_unsafe`.
- **Spans (introduced).** `abandonment_safety_check`, and
  `abandonment_safety_remote` for the `ls-remote`.

## Abandonment lane: no stop on a refused listing, its own time, and why sessions stay (2026-11-20)

What may close is unchanged. This only changes how often the lane judges.

- **Refused process listing.** A detached worker started from a sandboxed
  agent command inherits the sandbox and is refused the process listing. The
  lane used to stop on such a pass. It now stops only when that pass judged
  nothing else. Otherwise it goes on, and only the sessions whose verdict
  needs the listing (`holder_process_table_denied`) wait for a worker that can
  list processes. Counter: `session_retirement_continued_process_table_denied`.
- **Own time for abandonment.** In each page of the oldest-first lap, a
  landed proof may start only while more than a third of the page's time is
  left, and must finish before that third begins. The third is kept for the
  abandonment judgements of the sessions after it, which need no Git. The
  cursor is not held for skipped landed proofs: they come in a later lap or in
  the likely-landed queue. Counter:
  `landed_proof_stopped_for_abandonment_reserve`.
- **Not examined is not dirty.** A working-tree status that cannot be taken to
  the end (its deadline cut it, or Git failed) is "not examined". The session
  is kept for this pass as `working_tree_not_examined`, with no class record
  and no closing-soon note, and the answer is not remembered as uncommitted
  work. Counters: `stale_work_clean_not_examined` and
  `session_abandonment_kept_working_tree_not_examined`.
- **Why each session stays (meta, introduced).**
  `stale_sessions.last_keep` maps each open session to its last abandonment
  keep `{reason, at_ms}`; a close forgets it.
  `stale_sessions.abandonment_last_judged_at` records the last time the lane
  judged a session (closed, or kept for a reason other than a refused
  listing).
- **Status advice (introduced).**
  - `sessions.abandonment-held` (info): "N session(s) past the abandonment
    window are held: X by <reason>, ...".
  - `sessions.abandonment-lane-idle` (warning): sessions are idle past the
    window and the lane has judged none for over an hour.
  - `retention.invalid` (warning): `.aethyme/broker.toml [retention]` cannot
    be read, which disables every automatic close. A fractional
    `session_abandoned_after_hours` such as `0.5` used to disable the lane
    silently.
- **Attachment (introduced).** `abandonment_lane_last_judged_age_ms` on the
  `status` metric line, or `null` when nothing was ever judged. Span:
  `status_abandonment_lane_advice`.

## No committed graph: nothing to regenerate (2026-11-19)

A repository can declare `[graph] authority = "committed_fragments"` without
ever committing `.aethyme/graph/`. Every verification of such a tree then
regenerated every fragment (316 to 439 s per submit on the busiest repository)
to reach the only possible verdict, `stale`. Regeneration always writes
`.aethyme/graph/.gitattributes` and `.aethyme/graph/manifest.json`, which the
source tree lacks. The event also listed all regenerated paths (about 26,500).

The verifier now checks for that case first, with one object lookup:
`ls-tree <treeish> -- .aethyme/graph`. When the case holds, it returns at once
without isolating, indexing, linking or hashing a regenerated copy. The case
holds when all of these do:

- the verified commit has no `.aethyme/graph`;
- for a working state, the index and working tree also have no hashed path
  under it (`ls-files -co --exclude-standard`), and an on-disk engine pin is
  one Git would hash;
- `.aethyme/graph/manifest.json` is not ignored.

The status stays `stale`, because older readers drop unknown statuses. The
reason is `no committed graph fragments in tree; nothing to verify`, and
`changed_paths` holds only `.aethyme/graph`. Its readers (the `broker report`
graph section, submit's printed "stale graph paths", the
`graph.integrity_checked` payload) only display it. The advice now says the
repository declares fragments but has none committed, and how to generate
them or disable the policy.

Precedence is unchanged:
- an environment that refuses graph verification is `error`;
- a missing or empty engine pin is `incompatible`, with the full path's
  reason;
- otherwise the outcome is `stale`.

Any other case takes the full verification. The lanes covered are submit (the
known-tree path), the general disposable path (brokered pull-request advice,
native gates), `gates run` and session gate runs (the working-state path,
before any slot is created). The default-branch context warmup also returns
quietly, with no deferred warning each tick, when the commit has no committed
graph.

- **Span (introduced).** `graph_integrity_absent_check`.
- **Counters (introduced).** `graph_integrity_absent_tree`, and
  `start_context_warmup_no_graph` for the warmup.

## Submit's enforced graph verification skips the slot hash (2026-11-19)

On the busiest repository, the policy at the submission base declares
committed graph fragments authoritative. Its `graph.integrity_checked` events
show `enforced: true`, mostly with status `stale`. So the known-tree shortcut
for disabled policies never applied there, and `submit_graph_integrity` still
took about 208 s of real verification.

That path used to hash the verification slot only to decide which commit to
verify: HEAD when the hash equals HEAD's tree, otherwise a synthetic commit of
the dirty state. Submit now checks that HEAD's tree is the simulated tree it
materialized the slot from, which is one object lookup, and verifies HEAD
without re-reading the slot. On a mismatch the general path runs. The isolated
copy in which fragments are regenerated is created and written only by the
verification, so its two hashes (source check and regenerated tree) use the
stat-seeded hash, which falls back to the full hash whenever it cannot prove
equality. Verdicts are unchanged: tests compare the outcome with the general
path's, field for field, for a passing and a stale graph.

- **Counters (introduced).** Why the full path ran:
  `graph_integrity_full_enforced` (the policy enforces fragments) or
  `graph_integrity_full_environment_refused` (inherited Git routing variables;
  the outcome is an immediate `error`). Both are counted alongside the
  existing `graph_integrity_full_verification`.
  `graph_integrity_known_tree_mismatch` counts a slot whose HEAD did not
  carry the simulated tree, where the general path ran.
- **Spans (introduced).** Inside `submit_graph_integrity`:
  `graph_integrity_isolate` (inventory, storage check, isolated checkout and
  source check), `graph_integrity_source_check`, `graph_integrity_index`,
  `graph_integrity_link` and `graph_integrity_compare` (regenerated tree hash
  and changed paths). `graph_integrity_tree_hash` keeps timing each hash.

## Tree hash spans, and a stat-seeded hash for submit's verification slot (2026-11-19)

The working-tree hash (the gate cache key and the writer-quiescence proof)
builds a private index from `read-tree HEAD`, which has no stat data, so
`add -u` re-reads every tracked file, and then walks untracked files. Its
cost was inferred, not measured. It is now split into spans wherever it runs,
and submit's gate run on its freshly materialized verification slot seeds the
private index from the slot's own index instead. With the seeded index,
unchanged files are matched by stat data, and changed or racily clean entries
are re-read, the way `git status` decides.

- **Seeded hash (submit verification slot only).** The private index is a
  copy of the slot's index, with the copy's modification time set to the
  original's so Git's racy-clean rule is unchanged. A copy taken while the
  index was being rewritten, or whose timestamp reads back later than the
  original's, is refused. Every seeded Git command runs with
  `core.ignoreStat=false`, `core.trustctime=true`, `core.checkStat=default`, `core.fsmonitor=false`
  and `core.untrackedCache=false`. The seeded result is used only when all of
  these hold, each checked rather than assumed:
  - no entry is assume-unchanged or skip-worktree (`ls-files -v`);
  - the copied index's `write-tree` equals `HEAD^{tree}`, so nothing is
    staged and the entries are those of a fresh `read-tree HEAD`;
  - `core.autocrlf` is unset or false;
  - `check-attr` reports `text`, `eol`, `crlf`, `filter`, `ident` and
    `working-tree-encoding` as unspecified or unset on every tracked path.

  Any other case, and any error, takes the full hash. Session gate runs, the
  shared-verdict recheck, native gates, semantic-patch preview and graph
  integrity keep the full hash. `AETHYME_TREE_HASH_SEED=off` disables the
  seeded hash.
- **Spans (introduced).** Inside every hash: `tree_hash_index_build` (full
  path only), `tree_hash_tracked_rehash`, `tree_hash_untracked_walk`,
  `tree_hash_untracked_add` and `tree_hash_write_tree`. In the seeded path:
  `tree_hash_seeded_total`, `tree_hash_seed_index`, `tree_hash_seed_checks`
  and `tree_hash_seed_attributes`. Per call site: `gate_inputs_tree_hash`,
  `gate_shared_wait_tree_hash`, `session_gate_tree_hash`,
  `native_gate_tree_hash`, `semantic_patch_tree_hash` and
  `graph_integrity_tree_hash`.
- **Counters (introduced).** `tree_hash_seeded`, or one of
  `tree_hash_seed_fallback_<reason>` with `<reason>` one of `switched_off`,
  `unreadable`, `index_moving`, `timestamp_not_kept`, `entry_flags`, `index_not_head`,
  `content_conversion` or `git_failed`.
- **Attachment (introduced).** `tree_hash_files` on the command's metric line:
  `hashes`, `tracked_entries` and `untracked_files`, totals over the command.
  `tracked_entries` counts index entries. The full path re-reads all of them;
  the seeded path re-reads only the ones whose stat data changed, which is not
  counted.

## Submit's graph check reports the simulated tree when graphs are not authoritative (2026-11-19)

Submit's `graph_and_gate_selection` phase took 3 to 8 minutes on the busiest
repository, before any gate ran. The phase loads the graph and gate policies
at the base, which takes seconds, and runs the graph-integrity check on the
verification slot. That repository does not declare committed graph fragments
authoritative, so the check's verdict is always `disabled`. Its only work was
computing the outcome's `tree_hash` by re-hashing every file of the freshly
materialized slot through a fresh index. Submit now reports the simulated
merge tree that the slot was materialized from, which is that hash, without
reading the files. An enforced policy, or an environment that refuses graph
verification, takes the full path unchanged. Gate selection and gate execution
are unchanged; the graph check never fed gate selection.

- **Phase counters (introduced).** `graph_integrity_disabled_known_tree`
  (verdict `disabled`, reported from the known tree) or
  `graph_integrity_full_verification` (full path).
- **Spans (introduced).** Inside `graph_and_gate_selection`:
  `submit_selection_policy_load`, `submit_graph_integrity` and
  `submit_gate_definitions_load`.

## Worker admission skips its due checks while the worker runs (2026-11-19)

Every broker open admits the detached maintenance worker: it reads eight
"is work due" sources (about 57 ms median on the busiest repository, 0.8.134)
and only then tries the worker's singleton lock. On that repository the worker
was almost always running, so nearly every open read everything only to count
a coalesced request. The open now probes the singleton lock first. When the
worker holds it, the open counts the coalesced request (as before) and skips
the due checks; work that is due stays durable for the first open after the
worker exits, as it did when the lock was found held after the checks. The
probe holds the lock only for the instant of the check, so an admission that
goes on to launch takes it again exactly as before. The main-checkout sync
timer slot is no longer claimed while the worker runs; it is claimed by the
first open after the worker exits, which then launches the worker that runs
the timer lane.

- **Phase counters (introduced).** In the command's `phase_counts`:
  `worker_admission_skipped_live_worker` (the worker held its lock; the due
  checks did not run) or `worker_admission_due_checks_run` (they did). The
  probe's time is the span `worker_admission_live_worker_probe`; the existing
  `worker_admission_due_checks` span now appears only when the checks run.
  `gc.artifact_sweep.*.coalesced_requests` now counts every open that found
  the worker running, not only those that had found work due first.

## Worktree watch streams and changed-path reuse counts (2026-11-19)

A lease scan asks Git which files each session changed unless the host
worktree watcher shows the worktree unchanged since a stored answer
(`worktree_watch.rs`). With one file-system stream for every worktree, a
flagged event loss or a watch-set change outdated every stored answer; on the
busiest repository `agents` reused 970 of 10,184 answers in a day. On macOS the
watcher now gives up to 128 worktrees their own stream, the most recently asked
about first, so a loss, an error or a watch-set change outdates only the
worktree it concerns. Worktrees past the cap, or whose own stream does not
start, share one stream; its losses and watch-set changes outdate only the
worktrees on it. A placement is kept until the worktree is forgotten. Other
platforms put every worktree on the shared stream, because there a stream costs
a per-user inotify instance. A worktree whose watch starts nowhere is retried
after 1 s, doubling to 240 s, instead of every second. Expected idle cost of an
own stream on macOS: one thread (2 MiB of stack reserved, a few pages touched),
one run loop and an fseventsd client over Mach ports; no file descriptor.
What counts as a change, the claim protocol, the answer's key (counter,
baseline commit, index identity, maximum age) and the commands allowed to
reuse (`agents`, `status`) are unchanged; `submit` and every other deciding
command still always ask Git.

- **Phase counters (introduced).** One zero-length occurrence each in the
  command's `phase_counts` (and `phase_timings_ms`, always 0), per lease-scan
  question: `changed_files_reused` (existing) on a hit; on a miss exactly one
  `changed_files_miss_<reason>` with `<reason>` one of `symbolic_base`,
  `worktree_unreadable`, `no_watcher`, `no_counter`, `index_unreadable`,
  `no_answer`, `counter_moved`, `base_changed`, `index_changed`,
  `answer_expired` or `unsupported`; when reuse is not attempted, one of
  `changed_files_reuse_switched_off` (`AETHYME_WORKTREE_WATCH=off`),
  `changed_files_reuse_not_allowed` (a deciding command such as `submit`) or
  `changed_files_reuse_no_host_state`; and `changed_files_claim_failed` when a
  miss could not claim its counter, so its answer is not stored. Git time stays
  in the existing `changed_files_asked_git` span. Hit rate:
  `changed_files_reused / (changed_files_reused + changed_files_asked_git)`.
- **Watcher status fields (introduced).** `status.json` in the
  `worktree-watch-v2` host-state directory gains the gauges `own_stream_cap`,
  `own_streams`, `shared_streams` (0 or 1), `watched_on_shared` and
  `stream_threads` (own plus shared streams, one event-loop thread each), and
  the counters `rescans_scoped` (errors and flagged losses confined to one
  worktree's own stream), `watch_set_changes` (reviews that started or stopped
  watching a worktree), `watch_failures` (watches that could not start
  anywhere; that worktree is asked of Git), `watch_fell_back_to_shared`
  (worktrees put on the shared stream although own streams were allowed) and
  `watch_retries_deferred` (reviews that skipped a worktree waiting for its
  retry). `rescans` now counts invalidations of the worktrees on the shared
  stream: its losses, errors and watch-set changes.
  The directory's on-disk protocol is unchanged, so its version suffix is too.
  Readers must tolerate missing fields from older watchers.

## Gate cache miss events (2026-11-16)

The gate runner recorded a verdict reuse (`gate.cached`) and a forced re-run
(`gate.cache_bypassed`) but not an ordinary miss, so a hit rate could not be
computed from the broker's own records. It now records one `gate.cache_missed`
event per cacheable gate whose lookup reused nothing. Measurement only: cache
keys, lookup and reuse are unchanged.

- **Events (introduced).** `gate.cache_missed` with payload `{gate, tree,
  reason}`. `gate` and `tree` are the same values as in `gate.cached` and
  `gate.cache_bypassed`; no path, session id, branch or task text.
  `reason` is one of `no_reusable_verdict` (the indexed lookup returned no
  reusable row; "nothing stored for this tree" and "a stored row that is not
  reusable" are not distinguished, which would need a second query) or
  `profile_incomplete` (the execution profile could not be fully captured, so
  no lookup ran). A gate with `cache = false` and a bypassed run
  (`gate.cache_bypassed`: `--no-cache`, native gate jobs) make no lookup and
  emit no miss. The write is best effort: a refused write is reported and never
  fails the gate. Readers must tolerate new `reason` values.
- **Hit rate.** `gate.cached / (gate.cached + gate.cache_missed)`. Dividing
  hits by `gate.cache_bypassed` events is not a rate. History written before
  this change has hits and no misses, so a window reaching back past it
  overstates the rate.
- **Report.** `aethyme broker advanced insights` (`--json`: `gates.cache_misses`,
  `gates.cache_hit_rate`, null with no recorded miss; text: a `cache misses N,
  hit rate P` line under Gates) reports both over the same window as
  `gates.cached`.

## Prepared session context and semantic patches

The repository's existing `aethyme broker advanced watch pr tick` scheduler warms
navigation for the locally observed remote default commit when committed
graph authority is enabled. It reads exact Git objects, validates both the
source and fragment digests, and atomically publishes a derived store under
`.aethyme/run/start-context`. A known current commit skips materialization.
The warmup does no remote fetch and runs no repository commands. It reuses a
host graph artifact when available without waiting for another cache writer.
The `start_context_warmup`, `start_context_warmup_inputs`, and
`start_context_warmup_materialize` spans attribute the scheduler's work.

`broker start --json` and adopt responses can carry `task_context` with
`source_commit`, up to eight suggested `paths` and `symbols`, and
`suggested_gates` when gate policy can be parsed. The source commit must
match the recorded start or adoption revision. Up to four
`source_anchors` point to symbols by path and line, each with up to two
indexed callers. `observed_baseline` shows the last conclusive local gate
result with the exact baseline tree and current gate definition when one is
available. These historical observations do not verify the task candidate
or claim that the current host environment matches the recorded run.
Suggested gate names use the navigation paths. Missing context leaves the
existing scope warning. `start_task_context` measures the lookup and query.

A repository can commit declarative recipes in
`.aethyme/semantic-patches.toml`:

```toml
[[recipe]]
name = "new-api"
kind = "rust_call"
paths = ["src/**/*.rs"]
from = "crate::legacy::request"
to = "crate::api::request"
```

`aethyme broker start --task recipe:new-api --json` selects the named recipe
from the fetched default branch. The Rust parser matches direct,
crate-qualified calls in the declared scope. The broker writes a patch file
under session scratch and evaluates the selected gates in a separate
checkout using the existing gate trust decision. Gates see a synthetic
candidate commit with the recorded base as its parent and as
`AETHYME_GATE_BASE_COMMIT`. It reports
`semantic_patch` (base commit, candidate commit and tree, patch path, changed files,
replacement count, gate status and outcomes) or `semantic_patch_warning`
when it could not make a candidate. Paths to evidence are present when gate
logs were produced or retained for diagnosis. Status
`selected_gates_passed` describes the preview tree and is independent of the
ordinary submission gates. `no_gates_selected`, `selected_gate_failed`,
`candidate_changed`, and `unavailable` carry the remaining outcomes. The
preview store cannot supply cache evidence for a submission. Recipe phases
are `semantic_patch_prepare`, `semantic_patch_candidate`, and
`semantic_patch_verify`.

From the session checkout at the reported base, inspect the patch with
`git apply --check -- <patch_file>`. A user or agent can then apply it and
review the working diff before committing the migration.

## Concurrent gate verdict reuse

Cache-eligible local gates with a complete execution profile check the existing
repository verdict store. On a miss, a repository-local advisory lock gives one
producer the right to execute. The producer holds the lock through durable
verdict recording. A waiter re-observes its tree, then checks the same exact
cache identity. It can use the recorded result, take over when no reusable
verdict exists, or report resource contention if the shared wait reaches 60
seconds or the native deadline. Cache bypass and incomplete profiles still
execute separately. The lock names use 4,096 stable shards to bound runtime
files. Collisions may wait but never share a verdict across different keys.

The `gate_graph_integrity_disabled_skip` count records when a local gate
invocation skips a graph verification hash because the repository has no
committed graph authority. The gate still hashes its exact inputs.
`gate_artifact_marker` times registration of durable checkout-use markers,
`gate_cancel_obsolete_runs` times the pre-run reconciliation of older gate
processes, and `gate_execution_profile` measures one capture of the environment
and tool versions used by the cache. `gate_command_setup` measures a gate's
pre-spawn setup. Tool probes and the gate subprocess through output collection
contribute to child-command attribution. Nested phases describe overlapping
intervals and must not be added together.

The `gate_verdict_wait` phase measures contested lock time. The
`gate_verdict_joined` phase count measures completed handoffs. A reused
`GateRunOutcome` keeps the producer's run ID and duration. Its
`wait_duration_ms` records contention on the receiving command when present
and remains zero for an immediate cache hit. This is local to the shared
broker state and uses the existing tree and execution profile trust contract.
The comparison base is not part of that identity: a gate that reads
`AETHYME_GATE_BASE_COMMIT` must set `cache = false` (see "Diff-dependent gate
commands"), so a session's verdict for a tree satisfies a submission of the
same tree.

## Git wrapper probe verdict cache (2026-11-15)

Before its first Git command every `aethyme` process proves that the `git` it
will run is not a PATH wrapper that decorates porcelain output or rewrites exit
codes (#176, #178): about ten Git processes in a temporary repository. The
probe's verdict on each candidate is now recorded per user, so the probe runs
once per Git binary (and environment) instead of once per process.

- **File.** `<host cache dir>/git-probe/verdicts.json`, where the host cache
  dir is `AETHYME_HOST_CACHE_DIR`, else `$XDG_CACHE_HOME/aethyme`, else
  `~/Library/Caches/Aethyme` (macOS) or `~/.cache/aethyme`. The directory is
  0700 and the file 0600; it is written through a private temporary file and a
  rename. JSON `{"schema": 2, "entries": [{"key": {...}, "verdict": "accepted" |
  {"rejected": {"reason": "..."}}, "recorded_at_ms": n}]}`, at most 32 entries,
  newest first. Private to the broker: no other tool reads it, and its shape
  changes with `schema` (a file of another schema is ignored and rewritten).
- **Key.** Probe version, effective uid, the candidate's absolute PATH
  spelling, and for that path and every symlink hop to the final file: device,
  inode, size, mode, mtime and ctime with nanoseconds, plus the link text. For
  every candidate also a SHA-256 of `PATH` (any forwarder, script or binary,
  resolves what it runs through PATH; the cost is a re-probe when PATH changes)
  and of the loader variables `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`,
  `DYLD_FALLBACK_LIBRARY_PATH`, `DYLD_FRAMEWORK_PATH`,
  `DYLD_FALLBACK_FRAMEWORK_PATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH` and
  `LD_AUDIT`. For the macOS `/usr/bin/git` shim also `DEVELOPER_DIR`, the
  `xcode-select` link and the selected developer directory's `usr/bin/git`
  identity. A hit requires every field to match exactly.
- **What is recorded.** Acceptances and rejections (decorated output or
  rewritten exit codes). A cached rejection skips the candidate exactly as a
  fresh one would, so a stale one can only pass over a binary that has since
  become honest, and changing that binary changes its key. A probe that could
  not run (`Unusable`) is never recorded and drops any entry for that
  candidate. A verdict is recorded only if the candidate's key did not change
  while it was probed.
- **Expiry and distrust.** Entries expire after 24 hours; one dated more than
  five minutes in the future is invalid. A file that is not a regular file
  owned by the user with mode 0600, in a directory owned by the user that group
  and others cannot write, larger than 64 KiB, over the entry cap, or that does
  not parse is ignored: the process probes and never trusts it.
- **Deleting on Git evidence never rests on a cached acceptance.** The rule as
  built:
  - `broker finish`, `close`, `cleanup`, `gc` (including the detached
    `gc __continue-artifacts` worker), `reclaim`, `storage` and `worktrees`
    resolve Git with a probe of their own.
  - Every deletion primitive re-proves a cached acceptance once per process
    before it acts and refuses if a fresh probe no longer accepts the program
    in use: the pinned tree removal every GC, sweep, storage and registry
    deletion goes through, `removal::remove_tree` (reclaim, orphan checkouts),
    `git worktree remove` and branch-ref deletion. `paths_equal`, the evidence
    for a superseded promotion, does the same.
  - The ignored-directory sweep at broker open and GC journal replay
    (artifacts, recovery archives) confirm the verdict before they re-prove
    their evidence, and retain everything with a reason when it fails.

  A command that cannot delete may still run Git on a cached acceptance.
- **Residual.** A wrapper whose behaviour changes while its file and the keyed
  environment do not (it reads a config file or another variable), shared
  libraries the binary loads, and `GIT_EXEC_PATH` (stripped by the probe,
  inherited by real Git calls, as before this change) are not keyed. For
  commands that cannot delete, such a change goes unnoticed for up to 24 hours.
- **Switch.** `AETHYME_GIT_PROBE_CACHE=off` (also `0`, `false`, `no`) disables
  reading and writing it. The test harness sets it through `.cargo/config.toml`,
  and a process with `CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_aethyme` or
  `AETHYME_TEST_DB_SUFFIX` set never uses it either.
- **Probe directories.** The probe directory is now
  `$TMPDIR/aethyme-git-probe-XXXXXX` (it was `.tmpXXXXXX`). A process killed
  mid-probe leaves it behind. The detached GC worker, inside the host deleter
  slot and through the paced pinned tree removal, removes up to 200 per run that
  are older than an hour, owned by the user and contain only `empty-template`,
  `empty-gitconfig` and `repo`, whose `.git` (if present) holds only what an
  empty-template `git init` and two commits create. A legacy `.tmp*` directory
  is removed only when it holds exactly those three entries of exactly the kind
  the probe makes; any other `.tmp*` entry is never touched. Only the worker's
  own `$TMPDIR` is swept: leftovers under another `TMPDIR` wait for a worker
  started with it.
- **Telemetry.** Command metric spans: `git_program_resolve` and
  `git_probe_run` (timed), and the counters `git_probe_cache_hit`,
  `git_probe_cache_hit_rejected`,
  `git_probe_cache_miss_{absent,expired,key_mismatch,invalid,disabled,unkeyable}`,
  `git_probe_fresh_forced`, `git_probe_cache_write_failed`,
  `git_probe_dirs_sweep` (timed) and `git_probe_dirs_removed`.
- **`ps`.** Spawned as `/bin/ps` on macOS; PATH order on Linux, where busybox
  and procps can both be installed and the broker parses procps columns.

## Unattended deletion is paced (2026-11-13)

The detached worker (`aethyme broker gc __continue-artifacts <repo>`) is now a
polite background job. Foreground commands an agent waits on (`finish`,
`finish cleanup <id>`, `gc apply`, `gc sweep`, the bounded inline sweep at
broker open) are unchanged and run at full speed.

- **Priority.** First thing in the worker process: `setpriority(PRIO_DARWIN_PROCESS,
  0, PRIO_DARWIN_BG)` and `setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS,
  IOPOL_THROTTLE)` on macOS; `nice 19`, `SCHED_IDLE` and the idle `ioprio`
  class on Linux; nothing elsewhere (Windows included). Children (Git)
  inherit it. Not configurable.
- **Rate limit.** Every unlink of descriptor-relative tree removal in the
  worker passes a token bucket checked every 256 entries.
  `[cleanup] unattended_unlinks_per_second` in `.aethyme/config.toml`
  (default `1000`; `0` or `"unlimited"` restores full speed). An invalid
  value keeps the default and is reported as `config_error`. When the sweep
  is below gate headroom (disk-pressure mode) the limit is lifted for that
  pass; priority and pressure back-off stay.
- **Pressure back-off.** Once per batch the worker reads
  `kern.memorystatus_vm_pressure_level` and the one-minute load average
  (macOS) or `/proc/pressure/memory`, `/proc/pressure/io` and `/proc/loadavg`
  (Linux). While memory pressure is above normal (PSI memory
  `some avg10 >= 10` on Linux) it sleeps in one-second steps, never past the
  pass deadline and at most 120 s per run; then removal stops resumably
  exactly as at a deadline. Since 2026-11-15 load is a throttle, never a stop:
  while PSI I/O `some avg10 >= 40` or load exceeds twice the core count, each
  batch waits at most one second and goes on (see "Load throttles unattended
  removal instead of stopping it" below). An unreadable signal means proceed
  at the rate limit.
- **One deleter per machine.** At most one worker on the machine deletes at
  a time, through `<host state dir>/run/gc-unattended-deleter.lock` (an
  advisory `flock`, released by the kernel if its holder dies). Until
  2026-11-15 a worker took it after its due checks, waiting up to 15
  minutes, and held it to its exit, judging included. Since then it is held
  only while deleting, waiting at most 2 s for each deletion (see "The host
  deleter slot is held only while deleting" below). Without a resolvable
  host state directory the worker proceeds unserialised.
- **Coalescing.** The worker holds its repository singleton for 2 s before
  reading the backlog. A broker open or `finish` that finds the singleton held
  never starts a second worker; it increments the scoped meta counter
  `gc.artifact_sweep.coalesced_requests`, which the next run reads and resets.

Telemetry: `broker status --json` gains the optional object
`cleanup_retention.last_artifact_sweep.unattended_pacing` (absent until a
paced worker has run on a binary with this change; readers must accept its
absence). It is stored under its own scoped meta key
(`gc.artifact_sweep.last_unattended_pacing`) and attached on read, so the
persisted sweep report keeps the shape older binaries parse. Fields:
`at_ms`, `unlinks_per_second_limit` (null when unlimited), `config_error`
(omitted when none), `priority.cpu_lowered`, `priority.io_lowered`,
`entries_removed`, `logical_bytes_removed`, `elapsed_ms`,
`removal_rate_per_second`, `limiter_sleep_ms`, `memory_pause_ms`,
`load_pause_ms`, `pressure_yields`, `host_lock_wait_ms`, `requests_coalesced`,
`rate_lifted_for_disk_pressure`, and since 2026-11-15 `load_throttled`
(absent in summaries written before), and later the same day the host slot
fields `host_slot_held_ms`, `host_slot_deleting_ms`,
`host_slot_acquisitions`, `host_slot_busy`, `judge_permit_wait_ms` and
`judge_permit_busy` (0 when read from an older summary); `host_lock_wait_ms`
became the sum of the bounded waits before each deletion. Tree-removal progress itself is not
journalled per file: the ignored-directory queue is written once per removed
directory, and the sweep cursor once per pass.

## Coordinated fetches never write-block (2026-11-12)

A coordinated fetch of exactly one branch into its own remote-tracking ref
(`fetch --quiet --no-tags <remote>
refs/heads/<b>:refs/remotes/<remote>/<b>`, or the strict `--atomic` profile)
is journaled `failed` when it exits non-zero or times out,
with `details.failure_class` `repeatable_fetch_failed` or
`coordinated_operation_timeout` and `remote_outcome: not_applicable`. A crash
mid-fetch is closed as `reconciled_failed` with
`details.reconciliation.source: repeatable_fetch_auto_resolved` and an
`operation.fetch_reconciled` event, at broker open or the next overlapping
write. None of these write-blocks the repository. Every other write, including
any other fetch spelling, keeps `outcome_unknown` and its barrier.

A coordinated `gh` write whose `--body-file`, `--notes-file`, `-F <file>`
(under `pr`, `issue` and `release`), `--input` (under `api`) or
`-F key=@file` (under `api` and `workflow`) names a missing file, a directory
or an empty regular file is refused before anything is journaled.

## Writes that never reached the remote are failed (2026-11-13)

A coordinated write that provably sent nothing to the remote is journaled
`failed`, not `outcome_unknown`, and does not write-block the repository.

- A `git push` (any spelling, through `broker push` or `broker advanced git`)
  whose Trace2 events show the top-level process entered
  `transport_push/get_refs_list`, never left it, never entered `push_refs` or
  ran `pre-push`, and exited non-zero, and whose remote's effective push
  URLs (`git remote get-url --push --all`) all use a transport built into
  Git: `http`, `https`, `ssh`, `git+ssh`, `ssh+git`, `git`, `file`, an
  `host:path` SSH address or a local path. A `<helper>::` URL (including
  `ext::`), any other scheme, a set `remote.<name>.vcs`, or a URL list that
  cannot be read keeps the previous classification. `details.failure_class` is
  `push_failed_before_remote_refs_listed`, with `remote_outcome: not_sent`,
  `remote_write_contact: not_contacted` and `recovery: retry`;
  `details.push_reconciliation` keeps the plan and adds `evidence`
  `{classification: failed, source: git_trace2, reason:
  push_ended_before_remote_refs_listed}`. No post-failure remote read is
  made for it.
- A `gh` write that exits 1 with complete stderr whose first line is a
  flag-parser rejection (`unknown flag:`, `unknown shorthand flag:`,
  `flag needs an argument:`, `bad flag syntax:`, `invalid argument "`,
  `required flag(s)`, `accepts`, `requires at least`) for a built-in command,
  or a `gh api` call without `--paginate` (any value but an explicit
  `--paginate=false`) or `--slurp` whose stderr shows the connection
  was never established (gh's `error connecting to` DNS message, `dial tcp`,
  `net/http: TLS handshake timeout`, a certificate failure) and no HTTP status.
  `details.failure_class` is `github_request_not_sent`, with
  `remote_outcome: not_sent`, `recovery` `fix_arguments` or `retry`, and
  `evidence.signature` one of `argument_parse` (plus `parser_error`, the
  matched prefix only), `dns_resolution`, `connection_not_established` or
  `tls_handshake`.

A timeout, a reset or failure after the ref listing, and every other `gh`
failure keep the previous classification. Each failed coordinated remote write
adds one count to the command's `phase_counts` naming how it was settled:
`remote_write_failed_before_remote_refs`, `remote_write_failed_request_not_sent`,
`remote_write_failed_refused` (`github_refused`),
`remote_write_failed_repeatable_fetch`, `remote_write_failed_by_remote_read`
and `remote_write_succeeded_by_remote_read` (only when the journal records
that the post-failure push re-read observed the destinations, or the create
observation listed the collection), `remote_write_failed_other` and
`remote_write_succeeded_other` (settled any other way, such as a URL `gh`
printed), `remote_write_outcome_unknown`, and for a
provider killed at its budget `remote_write_outcome_unknown_timeout` or
`remote_write_failed_timeout`. Consumers must ignore names they do not know.

## Session push hooks before the lanes; self-settling unknown pushes (2026-11-23)

Additive. A strict push of the caller's own session branch (`broker push`, or
`broker advanced git -- push --no-follow-tags --porcelain -- <remote>
<sha>:refs/heads/<own branch>`) on its narrow lane runs the repository's
`pre-push` hook in `git push --dry-run` against the pinned commits before it
takes any shared lane, and the real push then carries `--no-verify`, when an
executable `pre-push` exists in the directory Git runs hooks from
(`core.hooksPath`, a relative one resolved against the worktree's top as Git
does, or the common directory's `hooks`). Git hands the dry run's hook the same
remote name and URL arguments and the same stdin lines it hands the real push,
so the hook runs once with identical inputs. Its environment is the push's
minus `AETHYME_BROKER_OPERATION_ID` and the operation progress variable (the
operation is not running yet): a hook that needs them, such as Aethyme's
protected-branch check, fails closed. The pre-lane remote read that plans the
push is bounded to 60 s. Under the lanes the push is re-planned and refused
(`refs_moved_after_pre_push`) unless every destination, proposed commit *and*
remote old value equals what the hook was given. `[coordination] hooks_outside_lock` is now three-state: absent
means this default, `true` means every eligible push (as before), `false` means
no push (the hook stays inside the push, under the lanes);
`session_hooks_hold_repository_lock = true` also keeps it inside.

- The hook run has its own budget, `[coordination] pre_push_hook_budget_seconds`
  (default 1200; unbounded when the caller waits forever). The queue and
  transfer deadline the caller asked for (60 s for `broker push`) starts once
  the hook has passed. A hook that runs out its budget or refuses resolves the
  journaled row `failed` (`admission_timed_out` / `pre_push_refused`, the
  hook's stderr kept); nothing was sent, so it blocks nobody.
- From before the hook to the end of the operation the push holds the
  exclusive lane `<repository>::git-ref/refs/heads/<branch>::pre-push`, taken
  first while nothing else is held, so two pushes of one branch still
  serialise. A refusal while waiting for it is journaled with reason
  `push_hook_lane_unavailable`.
- Phases: span `push_git_hook_lane_wait` (that lane's wait); the existing span
  `push_git_hook_dry_run` (the hook run) and `push_git_remote_read_before_lock`;
  count `push_hook_outside_lock_default` when the default chose this path.
  `coordination_timing.hooks_outside_lock` is `true` for such pushes.

A coordinated push stopped at its deadline is settled without an operator when
the evidence proves it:

- One bounded (15 s) read of its exact destinations (span
  `push_timeout_remote_read`) finds every destination at the commit it
  proposed: `succeeded`, with `remote_outcome` `succeeded`,
  `push_reconciliation.evidence.source` `post_timeout_remote_read`, counts
  `push_timeout_proven_succeeded` and `remote_write_succeeded_timeout`.
- The same read finds every destination still at its old value *and* Git's
  Trace2 shows it was still inside its own `pre-push` hook (a live trace whose
  ref listing finished, a child Git classes as the `pre-push` hook that
  started and never ended, and `push_refs` never began): `failed`, with
  `remote_outcome` `not_sent`, counts `push_timeout_proven_not_sent` and
  `remote_write_failed_timeout_in_pre_push_hook`. Neither alone is proof: Git
  stops writing Trace2 on a write error and keeps pushing, and a cut-off
  transfer may still land.
- Anything else stays `outcome_unknown` (`push_timeout_left_unknown`, or
  `push_timeout_not_observed` when no read was possible). The read goes to the verified push URL bound to the operation's
  recorded repository; a remote that no longer resolves to it (URL changed,
  push URL elsewhere) counts `push_timeout_remote_unverified` and stays
  unknown. The re-read below uses the same verified endpoint.

An `outcome_unknown` push of a session's own branch that carries its exact
plan is re-read by the next write it would block (span
`unknown_push_auto_reconcile`, budget 15 s or the writer's remaining deadline).
When the branch holds exactly the proposed commit it becomes
`reconciled_succeeded`; when it is still at the plan's old value at least 10
minutes after the push ended, `reconciled_failed` (a server applies a fully
received push within seconds). Both carry `reconciliation.source`
`session_push_remote_read`. A third value, a deleted branch whose old value
was a commit, or the old value sooner than that stays unknown. Counts
`unknown_push_auto_reconcile_succeeded`, `unknown_push_auto_reconcile_failed`,
`unknown_push_auto_reconcile_left_unknown`,
`unknown_push_auto_reconcile_unavailable`, and
`unknown_push_auto_reconcile_error` (settling failed; the writer goes on and
the row stays unknown). Status transitions now keep a row's
`push_reconciliation` (as they kept `fetch_reconciliation`), so a crashed
push keeps the plan that settles it.

Since 2026-11-23 a coordinated Git write records the identity of the process
carrying it out in details key `provider_process` (`pid`, `process_group`,
`start_time`, `start_time_clock`, written once at spawn while the row is
`running`, and kept by status transitions like `push_reconciliation`). The
old-value settlement above additionally requires that process proven gone:
its pid absent or reused (a different start token) *and* a signal-zero probe
of its process group answering `ESRCH`. A provider still alive, a row that
recorded no provider (rows written before this change), or an identity
without a usable start token stays `outcome_unknown`, counted
`unknown_push_auto_reconcile_provider_not_proven_gone`; a broker that died
mid-push leaves an orphaned `git push` with no deadline, and its transfer
could still land. The provider is proven gone *before* the remote read (a
provider alive during the read could land the push after it), and only when
the push is old enough for a `failed` settlement; the read still runs
otherwise, since `succeeded` needs no provider proof. Where the provider ran
without a group of its own (the foreground-terminal exception below,
including agent harnesses that run commands in a pty's foreground), its
recorded group is the caller's (the harness's or shell's job), and while
that group lives, which in a long-lived harness is typically always, such an
unknown push is not auto-settled as failed: only `succeeded` or an operator
settles it. `reconciled_succeeded` is unchanged (the proposed commit is
proof on its own). No writer signals a recorded provider: a dead broker's
orphaned group is not killed by a later writer, because a pid and start token
read before a `killpg` do not reserve the group id against reuse in between.

Deadline process groups (2026-11-23). A coordinated provider (every brokered
Git or GitHub command, including fetches, pushes and the pre-push hook dry
run, all through the operation lane's `output_within`) and the broker's own
`git fetch`/`push`/`pull`/`clone`/`ls-remote` calls (`run_git_process_output`,
including `remote_branch_head(s)` and the exact-push endpoint reads) now lead a process
group of their own (`setpgid(0, 0)` at spawn, Unix only). At the deadline the
whole group is stopped instead of the provider's pid: `SIGTERM` to the group,
up to 500 ms for the leader to exit, `SIGKILL` to the group, then the leader is
reaped. Both signals are sent while the unreaped leader, or its zombie, still
reserves the group id. A leader that exits while a descendant still holds its
output pipes is left unreaped until the output closes or the deadline stops
the group, so that descendant is stopped too. Span `deadline_group_stop`
(bounded by the 500 ms grace, the reap and a 200 ms absence probe). Counts by
how the leader ended: `deadline_group_kill_exited_on_term` (exited within the
grace), `deadline_group_kill_needed_kill` (still running when `SIGKILL` was
sent), `deadline_group_kill_already_gone` (had exited before the deadline;
only members it left were stopped); plus
`deadline_group_kill_members_remained` (the group was still present 200 ms
after the reap, e.g. a member in uninterruptible I/O),
`deadline_group_kill_unanchored` (the exit probe answered `ECHILD`: the
leader was reaped elsewhere and may no longer reserve the group id, so no
further group signal is sent), `deadline_group_exit_probe_failed` (the exit
probe failed otherwise; the leader is still unreaped, so the group is still
stopped as if running), `deadline_group_signal_failed` (a signal error other
than `ESRCH`/`EPERM`) and `deadline_child_kill_failed` (ungrouped child). The
group is skipped, and the child killed alone as before, when the broker is
the foreground process group of a controlling terminal, where Git's
credential prompt, `ssh` and interactive `gh` must still be able to read the
terminal (counter `deadline_group_kept_foreground_terminal`). That includes
agent harnesses that run each command in the foreground of a pty: there a
timeout still kills only the provider, as before. Processes that
leave the group (start their own session or group) are outside this cleanup.
Normal completions signal nothing; deadline values are unchanged.

Write-block scope: an ambiguous (`outcome_unknown`, or `running` with its
process gone) write whose recomputed scope is its own session's branch lane
(`git-ref/refs/heads/<that session's branch>`, never a shared default or
integration branch) blocks that session's writes, every write on that lane,
and every write that names the branch: a GitHub command whose ref targets
include it (a merge of its pull request, a delete, `update-branch`), or a Git
command any argument of which mentions it or that uses `--all`, `--mirror`,
`--prune` or `--tags`. Other sessions' other writes, repository-wide Git and
GitHub ones included, proceed and add the count
`unknown_session_branch_write_not_blocking`. An ambiguous write of anything
else (the default branch, `aethyme/integration`, tags, any repository-wide
write) still blocks every write. A consumer that assumed every unknown write
blocks the whole repository must read the row's scope. Covered by
`a_session_push_hook_holds_no_shared_lane`,
`a_session_push_hook_slower_than_the_push_deadline_does_not_time_it_out`,
`a_failing_session_push_hook_refuses_the_push_with_its_output`,
`session_push_hooks_run_early_only_when_a_hook_exists_and_not_opted_out`,
`a_timed_out_session_push_that_landed_reconciles_itself`,
`a_genuinely_unknown_session_push_blocks_only_its_session_and_branch`,
`an_unknown_session_push_is_not_settled_from_a_different_endpoint`,
`another_sessions_write_naming_the_ambiguous_branch_stays_blocked`,
`an_unknown_write_of_a_shared_branch_blocks_every_session`,
`a_remote_that_moves_after_the_hook_refuses_the_unverified_push` and
`unknown_outcomes_block_their_own_lane_and_wide_writes_block_everything` in
`aethyme-broker/tests/operations_e2e.rs`, and
`only_a_push_stopped_inside_its_hook_is_proven_unsent` and
`a_timed_out_push_is_proven_only_against_its_recorded_repository` and
`a_timed_out_push_settles_only_on_combined_evidence`.

## Coordinated operation outcome journal (2026-11-17)

Schema v64 adds a nullable `coordinated_operations.run_nonce`, a random value
set when a run moves to `running`. It is compatible: older writers leave it
NULL, older readers ignore it, and `min_compatible_schema` stays 60.

Once a coordinated `git`/`gh` child exits (or fails to spawn, or is killed
at its budget), the broker writes the known result to
`<broker db dir>/run/operation-outcomes/operation-<id>.json` before it
touches the database: schema `aethyme.coordinated-operation-outcome/v2`,
`operation_id`, `session_id`, `command_sha256` (SHA-256 of the row's exact
`command_json`), `operation_created_at` (the row's `created_at`), `run_nonce`,
`writer_pid` (the row's `pid`), `status` (`succeeded`, `failed` or
`outcome_unknown`), `exit_code`, `details_json` (the details the row would
carry, including any created URL, pushed refs or reconciliation evidence) and
`child_finished_at`. The file is a synced temporary renamed into place,
followed by a sync of the directory.

With the journal on disk, the operation's host lane is finished with the
observed outcome and its coordination locks are released; only then is the row
updated, in one `IMMEDIATE` transaction that does nothing else (update plus its
`operation.<status>` event), committed with `synchronous = FULL` so the WAL is
synced before the journal is deleted. The update applies only while the row is
`running` or `outcome_unknown` and every field of the run binding matches
(id, session, command, `created_at`, `run_nonce`, `pid`); it is retried with
exponential backoff (50 ms doubling to 2 s, each attempt waiting at most 5 s
for the lock) for up to 60 s. A power loss at any point leaves the journal or
the durable record, never neither. If the journal cannot be written, the locks
are held through the record as before.

If the database stays busy past that budget, the command still reports the
child's true result. `broker advanced git|gh --json` gains
`outcome_record_pending` (`operation_id`, `journal_path`, `database_error`,
`attempts`, `waited_ms`), stderr gains a `warning:` line naming the operation
and saying it will be recorded automatically, and the exit code follows the
child's outcome. `broker advanced operations show <id> --json` gains
`outcome_journal: pending|none`.

If the row already holds a different outcome than the run observed (for
example an operator reconciled it the other way), or no longer matches the
run, the command reports the observed result, the row is left unchanged, the
journal is kept as `operation-<id>.json.conflict`, an
`operation.outcome_conflict` event is recorded (`operation_id`, `provider`,
`repository`, `observed_status`, `recorded_status`, `reason`, `source`:
`record` or `replay`), and `--json` gains `outcome_conflict`
(`operation_id`, `observed_status`, `recorded_status`, `reason`,
`journal_path`). `outcome_unknown` is not evidence and never conflicts.

Replay: every writable broker open (every CLI command and the detached
artifact worker; snapshot opens excluded), and every coordinated write before
its write-block check, replays pending journals. A journal is applied only if
its operation exists, the full run binding matches, and the row is `running` or
`outcome_unknown`; the replayed row's details add `outcome_journal`
(`replayed_by`: `broker_open` or `write_block_check`, `replayed_at`,
`child_finished_at`, `writer_pid`, `status_before_replay`). A row with no
`run_nonce` (run by an older binary) is never settled from a journal. A journal
for a row already settled with the same outcome is deleted; a different
outcome is a conflict as above. A journal that is unreadable, carries another
schema, names a missing operation or does not match the run is kept as
`operation-<id>.json.refused` and never applied. Set-aside files use schema
`aethyme.coordinated-operation-outcome-set-aside/v1` (`operation_id`, `kind`,
`reason`, `set_aside_at`, `observed_status`, `recorded_status`, `journal`). A
replay waits at most 1 s for the write lock and otherwise leaves the journal
for the next opener. While a replay cannot write, the write-block check treats a
row whose matching journal holds `succeeded` or `failed` as decided rather than
marking it `outcome_unknown`.

`broker status` warns with `operation.outcome-journal-unreplayed` per journal
older than ten minutes, `operation.outcome-journal-refused` per refused journal
while its operation is still `running` or `outcome_unknown` (naming the
reason), and `operation.outcome-journal-conflict` per conflict file until it is
removed.

Invariant: success is never inferred without a journal of the same run. A
`running` row with no journal (the process died before the child's result was
known) keeps the previous path: `outcome_unknown`, a write block, and explicit
reconciliation.

Telemetry: phase counts `operation_outcome_journal_written`,
`operation_outcome_journal_write_failed`,
`operation_outcome_locks_released_early`, `operation_outcome_record_retry`,
`operation_outcome_record_attempt_failed`, `operation_outcome_record_pending`,
`operation_outcome_journal_replayed`, `operation_outcome_journal_discarded`,
`operation_outcome_journal_still_pending`, `operation_outcome_journal_refused`,
`operation_outcome_journal_conflict`; phases `operation_outcome_record_wait`
(total wait) and `operation_outcome_journal_replay`; attachment
`operation_outcome_record` (`outcome`, `attempts`, `retries`, `waited_ms`,
`journal`).

Every broker open also stopped writing `meta.min_compatible_schema` when the
recorded floor is already at least the binary's: the unconditional upsert took
the database write lock on every open, including each running operation's
10-second heartbeat.

Removing the journal, or recording before it is written, brings back the
stranded `running` row that write-blocks every session after a write that
succeeded. Dropping any part of the run binding lets a stale journal settle
another run that reuses the id after a database reset. Covered by
`operations/outcome_journal/tests.rs` and the v64 schema test.

## Stale sessions close from the background worker, by work class (2026-11-17)

**Changed behaviour.** CLI commands (`status`, `agents`, `exec`, `leases
claim`, `start`, `submit` ...) no longer close abandoned or no-work sessions
inline: the `agents` view counts them (`agents_view_abandonment_due_deferred`)
and queues the shared detached worker (`gc __continue-artifacts`), which
judges them. A reader that expected one `broker status` to close an old
session must now wait for the worker. Library callers of `Broker::agents`
still judge inline. No session closes without a complete process listing in
the same pass; `Unbound` sessions also need the complete open-file and
working-directory observation.

**New `[cleanup]` keys** (validated by the `[cleanup]` loader; an invalid
value keeps sessions open): `stale_session_auto_close` (true),
`stale_landed_close_after_hours` (24), `stale_pushed_close_after_hours` (72),
`stale_unpushed_close_after_hours` (168), `stale_closing_notice_hours` (24).
Past the abandonment window, a session with no live holder waits for its
class window (the more protective of HEAD's and the branch tip's class), and
a session holding work also for a closing-soon note left at least the notice
period earlier with no activity since. A holder bound to a newer session of
the repository (same pid and start time, no bind back) is no live holder, in
the abandonment and no-work windows. The keep reason `holder_moved_on_dirty`
(counter `session_abandonment_kept_holder_moved_on_dirty`) is removed; the new
keep reasons are `work_class_not_due` and `start_reserved` (a held start
reservation on the checkout's path).

**New fields and records.** `session.abandoned` from the abandonment window
gains `work_class` (`landed`, `pushed_unmerged`, `unpushed`). A closing-soon
automation note (sender `automation:session-auto-close`, fingerprint
`session-closing-soon`) gates every close of a session holding work. Status
advice: `sessions.stale-backlog`, `sessions.closing-soon`,
`sessions.process-table-denied`. Meta keys `stale_sessions.classes`,
`stale_sessions.process_table_observed_at`,
`stale_sessions.process_table_denied_at`.

**Worker.** A retirement pass refused the process listing closes nothing and
defers its sessions (they are no longer counted in
`landed_session_retirement.abandonment.kept`), and the worker stops its
retirement lane with the request pending
(`session_retirement_stopped_process_table_denied`).

Covered by `stale_session_policy` unit tests, the `session_auto_close` lane
tests, the `gc` worker test, `file_lock` tests and
`tests/session_abandonment.rs`. Upgrade note: the automatic checkout
maintenance guide, "Stale sessions by work class".

## Compact status by default, session block and push plan (2026-11-17)

**Changed default.** `aethyme broker status --json` now prints the compact
document (`aethyme.broker-status-compact/v1`), not the full one. A reader that
needs the full document (`agents[]`, `leases`, `queue`, `blockers`,
`outstanding_advisories`, `cleanup_retention`, `capacity_removal`, top-level
`deferred_checks` ...) must pass `--full` (or `--refresh`). Field names of both
documents are unchanged; each now carries `form` (`"compact"` or `"full"`), and
compact adds `hint`. `--compact` is still accepted and changes nothing; it,
`--summary`, `--session` and `--since-revision` are refused beside `--full`.
Compact JSON status is routine status: a read-only snapshot of broker state.
It still appends its command-metrics row (see Telemetry below) but writes no
outcome event and no broker-database row. Readers of the repository compact form use category counts, `summary` and
`drilldown`. See Session coordination consumers for the worktree-scoped default. Every internal reader of the
full fields (tests, the JSON-shape snapshot, generated agent docs, the agent
hook's advisory pointer, finish's recovery hint, the owners drilldown) now
names `--full`. The ignored `local_compatibility` old-binary test still runs
`broker status --json` against a pinned pre-change binary, whose default is
the full document.

**Evidence cap.** In every form, an advice entry whose `evidence` array
serializes to more than 8 KiB keeps its first items (at most 20, about 4 KiB)
and gains `evidence_truncation` (`truncated: true`, `count`, `shown`,
`omitted`, `evidence_bytes`, `cap_bytes`, `command`).
`AETHYME_STATUS_EVIDENCE_CAP=off` disables the cap. `storage.over-budget` now
names five unlanded recovery archives and one "and N more" line instead of one
line per archive; `aethyme storage status --json` lists them all.

**Session block.** `status --session <id> --json` adds `session` (`id`,
`status`, `branch`, `worktree`, `head_oid`, `remote_oid`, `unpushed_commits`,
`push_state_error`, `pr`, `pr_state`, `preparation`, `blockers`, `advice`,
`next_action`). `pr` comes from the broker's pull-request watch only; its
`mergeable`, `merge_state` and `required_checks` are null and listed in
`unknown_fields`. Without a recorded watch, `pr` is null and `pr_state` is
`"unknown"`. No network call is made on this path.

**Push plan and report.** `broker push --session <id> --plan [--json]` pushes
nothing, writes no metric row or outcome event, reads the remote branch once
and prints `remote`, `destination_ref`, `proposed_sha`, `remote_sha_last_known`,
`remote_sha_recorded`, `remote_sha_tracking`, `remote_sha_observed`,
`remote_observed`, optional `remote_read_error`, `mode` (`fast_forward`,
`lease` + `lease_expected_sha`, `up_to_date`, `would_refuse` + `refusal`),
`commits_to_push`, `pushed: false` and `command`. The push report adds
`pre_push_remote_oid`, `pre_push_remote_observed` and `pushed_refs[]`
(`destination_ref`, `proposed_sha`).

**Telemetry.** Every status call, compact default included, appends a row to
`.aethyme/logs/command-metrics.jsonl` (previously routine status wrote none
unless `AETHYME_MEASURE_OUTPUT` was set). Status rows add
`status_form`, `status_json`, `status_session_flag`,
`status_evidence_truncated` and `caller_hint` (the parent process's executable
name, at most 64 characters of `[A-Za-z0-9._+-]`; never arguments or paths).
Compact `phase_timings_ms` gains `session_block`.

Covered by `aethyme-broker/tests/status_compact_cli.rs`,
`aethyme-broker/tests/session_push_cli.rs`, the `cli::status_extras` and
`cli::telemetry` unit tests, `storage_ledger` tests, and the `aethyme-cli`
help and JSON-shape snapshots.

## Push finalization and review-on-push reads (2026-11-13)

`broker push` reports `duplicate_work` exactly as before, but derives liveness
only for the live sessions whose branch, task or cached open-PR listing match
the pushed session (usually none). The others are neither read for activity
nor reconciled by the push: their status transitions and abandonment closes
happen at the next `broker status`, `broker agents` or other full agents view.
Each session left out counts once as `agents_view_sessions_skipped` in the
command's `phase_counts`; `agents_view` times the part still done. The same
applies to the duplicate-work list `start`, `adopt` and `reuse` print.

`broker advanced review run` (and so `[review] run_on_push`) starts its
read-only provider reads together instead of one after another: the change
read stays on the calling thread (`review_run_change_read`), while the
snapshot (`review_run_snapshot_read`), the projection facts
(`review_run_pr_facts_read`: labels, comments and, now concurrently, the `gh`
identity), and with `--from-provider` the diff (`review_run_diff_read`) run on
threads that only call `gh`. Each is joined where the run used to make it, so
errors surface in the same order; `review_run_prefetch_wait` is the time the
run still waited for them. `review_run_classify` and `review_run_github_writes`
time classification and the coordinated writes. A push's own metric splits
`review_on_push` into `review_on_push_policy`, `review_on_push_child_start`
and `review_on_push_child_wait`. What is reviewed, written and reported is
unchanged; the review still runs on every push to a non-draft pull request,
including a repeated push of the same head.

## Landed sessions close without `finish` (2026-11-13)

Unattended session retirement (the inline sweep on a writable broker open and
the shared background worker, `gc __continue-artifacts`) now asks the landed
lane first for every open session idle past `session_landed_after_hours`
(default 24), whatever its age, and falls back to the abandonment window only
when the landed decline leaves the question open. The landed lane closes a
session only when all of these hold, re-checked under the session operation
guard and the lifecycle claim:

- no pull request linked to it is unmerged in the cached listing the PR watch
  keeps (`pull_request_session_links` and `pull_request_milestones`; no
  provider call is made per session);
- `[cleanup] auto_remove` is on and no keep pin matches the checkout;
- the fetched default branch holds every commit of its HEAD by the rule
  `finish` uses: a representation recorded against that branch, ancestry, or
  `default_branch_landing`, whose `landing_covers` keeps a commit made after a
  squash unlanded. The content search runs only when a merged pull request, a
  representation or ancestry already suggests a landing; a session that never
  committed and never merged a pull request is left to the abandonment window;
- `unclean_reason` finds nothing: no uncommitted or untracked path, no merge,
  rebase or stash in progress, and only ignored content the shared recovery
  classifier treats as disposable. It now runs `git --no-optional-locks
  status`, so asking never refreshes the index whose modification time is the
  worktree activity both idle windows read;
- no process holds it: the broker-spawned PID is gone, and the recorded holder
  is gone or, when it still runs (one agent process routinely holds many
  sessions in turn), its task lease on this session has expired and no gate,
  unresolved operation, preparation or open file uses the worktree. The
  holder's own working directory inside the worktree counts as use here
  (the opt-in abandonment lane still tolerates it). The
  `expire_abandoned_task_leases` opt-in is not needed for a landed session.

The proof is computed once, before the claims, within the pass deadline (out
of budget: `budget_exhausted`, and the cursor stays on that session for the
next pass unless that one proof alone spent 10 s of a pass). Under the claims
it is only verified: HEAD is the proved head, the fetched default-branch tip
is unchanged (`upstream_moved` otherwise), and `git status` plus in-progress
markers are clean. Immediately before the close HEAD and the worktree's
index/HEAD modification times are read again; any change is
`moved_before_close`. A head the search found unlanded is remembered in broker
meta (`landed_lane.not_landed.<session id>` = `<head> <tip>`), so it is not
searched again until either moves; `landed_landing_search` times searches and
`landed_landing_search_memo_skipped` counts the ones skipped.

Closing releases the session's leases in the same transaction and requests the
existing whole-checkout cleanup, which removes the checkout after
`closed_worktree_grace_hours` through the same containment proof as before.
The `session.abandoned` payload's `retirement_basis` is
`clean_recorded_landing_and_gone_holder` or, for a live holder with an expired
task lease, `clean_landing_and_expired_task_lease`.

The retirement pass keeps its keyset cursor over open sessions (128 per page,
fixed high-water lap), so successive passes resume instead of restarting.
`AutoCleanupReport` adds optional `landed_sessions_examined`,
`landed_sessions_closed`, `landed_sessions_declined` (counts per reason code),
`retirement_cursor` (`after_session_id`, `high_water_session_id`) and
`landed_retirement_elapsed_ms`; all are omitted when zero or empty, and the
background worker sums them across its passes. `broker status --json` (routine
and `--refresh`; not `--summary`) adds an optional top-level `landed_session_retirement` object
(`evaluated_at_ms`, `examined`, `closed`, `declined`, `cursor`) taken from the
last report, omitted until the landed lane has judged a session. Routine
declines do not raise the `cleanup.auto-removal` advice; `policy_unavailable`,
`landing_unknown`, `holder_unknown` or `operation_unknown` do, and whenever the
advice is shown it carries one evidence line with the same counts.
Each pass adds `landed_sessions_examined`, `landed_sessions_closed`,
`landed_session_retirement` (time) and one `landed_sessions_declined_<reason>`
per kept session to the command's `phase_counts`. `status` output size is the
existing `output_bytes` of its metric line.

Reason codes: `recent_activity`, `changed`, `process_alive`,
`holder_alive_current`, `holder_alive_in_use`, `holder_unbound`, `holder_unknown`, `lease_held`, `lease_unknown`,
`worktree_unavailable`, `busy`, `operation_active`, `operation_unknown`,
`policy_unavailable`, `auto_remove_disabled`, `pinned`,
`pull_request_unmerged`, `no_session_commits`, `no_landing_evidence`,
`not_landed`, `landing_unknown`, `worktree_unclean`, `moved_before_close`,
`upstream_moved`, `budget_exhausted`, `proof_timed_out`. An ignored tool cache keeps a session as
`worktree_unclean` until the repository names it in `[cleanup] regenerable`
(a list of globs, relative to the worktree root, in `.aethyme/config.toml`).
Fixtures that keep a
checkout across broker opens with `[cleanup] auto_remove = false` keep their
sessions open too (`auto_remove_disabled`).

## Automatic checkout maintenance (v0.8.44)

`AutoCleanupReport` adds optional `sessions_retired` and
`retirement_deferred_reasons` fields. They are omitted when zero or empty, and
readers must accept saved reports without them. `enabled` continues to describe
automatic whole-checkout removal; session retirement follows retention policy
separately. Closing a session does not prove removal or reclaimed bytes.

The primary repository's `.aethyme/config.toml` adds
`[cleanup].ignored_directory_sweep`, default `false`, for in-place reclamation
of Git-ignored directories in retained closed checkouts. Strict mode, keep pins,
protected metadata roots, active use and unresolved ownership continue to retain
data. Selected ignored directories stay opaque; nested metadata inside them is
disposable too.
Standalone ignored files are outside this new directory policy. The separate
whole-checkout grace is 24 hours (1 day) by default. Recovery archive
retention remains 15 days.

Broker schema 58 raises the compatibility floor to 58 for the gate-use protocol.
Already-open older clients must drain before enabling destructive automatic
maintenance; the floor alone does not revoke open connections. Unix targets
support this protocol; unsupported targets retain the existing explicit cleanup
contract while protocol-dependent unattended removal stays blocked. See
[automatic checkout maintenance](../guides/automatic-checkout-maintenance.md)
for rollout holds, policy restoration and the report field contract.

## Generated files and checkout capacity (2026-11-13)

Cleanup (finish, `finish cleanup --all-cleaned`, unattended removal, `gc`
and recovery archives) no longer keeps a closed checkout for an ignored file
that is byte for byte what an Aethyme writer (`deploy`, `upgrade`, the
local-only lane) produces at that path without reading the checkout:
`.claude/settings.local.json` exactly as created fresh (only the SessionStart
hook), `.claude/hooks/aethyme-load-context.sh`, and the deployed
`.claude/skills/aethyme*/` and `.codex/skills/aethyme*/` files. A directory is
disposable only when every entry is such a file, a nested directory of the
same kind or `.DS_Store`; symlinks are never followed or proven. A settings
file that deploy merged into, that a harness added permissions to or that a
person edited, a file written by a different Aethyme version, and
repository-rendered content (root policy, onboarding skills, local policy and
marker) keep the existing protection and message. Strict mode still keeps
every ignored leftover. The catalog is
`aethyme_enhance::deploy::generated_artifact_contents`, derived from the
writer itself.

`broker status` (default and `--refresh`) adds the advice row
`cleanup.checkout-capacity` (severity `warning`) once registered linked
checkouts reach three quarters of `[worktrees].max_checkouts`. Its evidence
names the registered count, the limit, open sessions and retained checkouts;
its commands are `aethyme broker gc sweep --json` and
`aethyme broker finish cleanup --all-cleaned --json`. When the status budget is
spent the check is skipped and `checkout_capacity` is added to
`deferred_checks`. `broker start` at the limit already runs the unattended
removal before refusing; that behaviour is unchanged.

## Managed daemon runtime identity and bundle pins (#1313)

Managed daemon startup is a private matched-router/engine protocol. The router
selects a versioned pair under `<managed_root>/versions/<bundle>` by exact full
embedded version/revision/commit identity while holding the shared
`<managed_root>/.update.lock`, then acquires a shared pin at
`<managed_root>/runtime-pins/<bundle>.lock`. It passes that pinned descriptor
to the daemon child on stdin with `AETHYME_ENGINE_RUNTIME_PIN_STDIN=1` and
`AETHYME_ENGINE_RUNTIME_PIN_PATH=<exact pin path>`. Those private markers are
claims only: the child validates the descriptor, exact path, executable pair
and full build identity before serving, and keeps the pin open for the daemon's
lifetime. Version probes clear both markers.

The private line-delimited JSON identity request is `{"command":"identity"}`;
its response must contain `ok: true`, `protocol_version: ENGINE_PROTOCOL_VERSION`
(currently 1), and the exact `build_identity`. The socket key includes the
canonical repository path, protocol version and full identity. The daemon
pidfile name is derived from the full identity.
Updaters retain current and previous bundles and skip a noncurrent bundle with
a busy pin. Once all daemon descriptors close, a later updater removes the
released pin and bundle.

Installer cleanup uses `RuntimeBundlePin::try_acquire_exclusive_for_cleanup`
for safe old directory components, including orphan and staging names. Its
`None` outcome preserves an unrecognized directory without treating
it as missing; malformed pin metadata remains an error. Non-UTF-8 names are
also preserved. This cleanup-only path retains the existing private-path and
file-lock checks. Candidate resolution, shared consumer pins and inherited
daemon descriptors keep the strict versioned-bundle name validation.

This protocol is cooperative: older updaters ignore pins, and pre-protocol
daemons use legacy pid/socket names, so the new lifecycle cannot account for
them. It does not establish safe mixed-generation rollout on a shared mutable
runtime. For scheduled idle GC rollouts, provision a dedicated immutable managed
installation prefix per repository. The service plan binds one generation, exact
versioned router/engine paths, hashes and full build identity; changing that
runtime requires deactivating and uninstalling the old generation, then
reviewing, installing and activating a replacement plan. Do not point the
service at a shared mutable global selector.

## Scheduled GC service

The macOS operator commands `broker gc service plan|install|status|activate|deactivate|uninstall`
introduce a separate `aethyme.gc-sweep-service-plan/v1` plan namespace. Installation
is inert; each state change requires the exact installed plan digest. The plan
binds repository UUID, canonical repository/home paths, user identity, immutable
managed router and engine paths, both binary hashes, build identity, fixed
900-second interval, generation and LaunchAgent bytes. Changed runtimes require
explicit deactivation, uninstall and review of a replacement plan.

The LaunchAgent invokes only `broker gc sweep --json`, with
`AETHYME_GC_SWEEP_SERVICE_UUID` and `AETHYME_GC_SWEEP_SERVICE_GENERATION` markers.
Before opening a mutating broker it checks the repository snapshot, installed
generation and both binaries. Native GC policy, singleton admission, bounded
worker passes and retry backoff remain authoritative. `RunAtLoad` is false;
intervals missed while asleep are not proof that a tick ran. No agent/model
calls, retention overrides or automatic recovery-archive purging are added.

`aethyme.gc-sweep-service-control/v1` records acknowledged or unknown scheduler
operations. Read-only status reports that observation and runtime health.
Uninstall requires an acknowledged deactivation, checked under the same
installation lock used by activation and managed ticks; it removes only the
exact owned generation files. Missing receipts are distinct from unreadable or
unsafe receipts.

`aethyme.gc-sweep-service-last-pass/v1` is atomically replaced and limited to
4 KiB. It binds repository UUID/generation and timestamps, with `sweep_complete`,
`sweep_paused`, `error` or `unknown` outcomes and optional sweep counters. A
completed sweep is not proof that all checkout cleanup or archive retention
work has finished. Detached continuation workers keep their own native receipts.
Existing PR-watch plans, units and health records retain their contract.

## PR-watch service plan (#887)

`aethyme broker advanced watch pr service plan --broker-bin <absolute-router>`
adds an operator-only read-only macOS launchd planning surface. Optional
`--seconds 120..3600` defaults to 300; `--json` emits the versioned
`aethyme.pr-watch-service-plan/v1` schema. The canonical plan digest binds
repository/user identity, executable and embedded helper hashes, paths and
unit content. The explicit macOS `service install` command requires the exact
recomputed digest via `--confirm` and emits `aethyme.pr-watch-service-install/v1`
with created paths, no activation and an unobserved scheduler state. It never
loads a unit or replaces differing files. Explicit macOS `service activate` and
`deactivate` require the installed digest; `status` is read-only. Their
`aethyme.pr-watch-service-control/v1` result reports scheduler observations and
optional generation-bound last-pass metadata. Unacknowledged actions exit
nonzero and report unknown scheduler state; loaded is not a delivery proof. See
`docs/guides/pr-watch-service.md` for fields, digest construction and limits.

## Direct-child helper observations (#713)

`aethyme broker advanced exec-status --session <id> [--receipt <id>] --json`
adds nullable `helper_observation` to each receipt. When present it contains
`direct_child_outcome` (`succeeded`, `failed` or `cancelled`), nullable
`exit_code`, `observed_at_ms`, `process_group_cleanup: "unknown"` and
`ownership_audit: "not_run_by_helper"`. Human output prints the same distinction.
The parent receipt's existing phase, audit and cleanup fields remain authoritative
for their existing purposes; a helper observation never completes those checks.
Signal termination has no numeric exit code. A cancellation observation means
the helper observed its directly supervised child exit on a cancellation path,
not that it verified every descendant or detected an escaped process.

Schema 57 adds a diagnostic table, retaining the existing compatibility floor.
Older writers can continue their existing receipt transitions. The private helper
wire moves to v4; caller and helper must use the matched runtime. The helper opens
a fresh existing database connection, checks its file identity and exact receipt
binding, and does not create or migrate a database. Lock waiting is limited to
250 ms; storage operations are not an OS-level wall-time guarantee. A failed
write leaves the observation absent. Abrupt helper death, database contention or
failed cancellation/reaping can still leave an unknown outcome. The helper never
persists argv, environment, output or process IDs in this table.

At most 100 helper observations are retained per session, independently of
unresolved parent receipts. Parent receipt deletion cascades to its observation.
An absent or aged-out observation is unknown, not proof of success or cleanup.
These diagnostics introduce no process-signaling, lease-release or worktree-removal
authority. Gate/preparation and legacy interactive execution do not gain this
observation unless they use the receipt-bearing supervised path.

## Broker Git resource profiles and coordination visibility

Git commands retain repository-exclusive coordination unless their entire argv
matches a closed single-ref profile. The fetch profile explicitly disables tags,
FETCH_HEAD writes, submodule recursion, maintenance, commit-graph writes, pruning
and configured refmaps. Scoped operations hold the historical repository lock
shared and the derived branch resource lock exclusive. All current Git writers
also take a device/inode-bound common-directory lane (shared for strict profiles,
exclusive otherwise); fetches additionally lock their exact local destination.
This prevents remote aliases and symlink spellings from splitting local write
coordination. Legacy clients retain same-remote historical exclusion only;
upgrade all writers before depending on cross-remote concurrency. Caller `--scope` labels never select a lock. Unknown outcomes
from broad operations still fence all Git resource scopes. Unsupported hooks,
custom refmaps, shallow repositories and unrecognized configuration retain the
broad command spelling; a strict profile refuses before spawning.

A strict fetch's operation details may contain `fetch_reconciliation` schema 1:
exact source/destination, prior destination, common-directory device/inode,
provider process generation and an actual child-wait result. Minimal timeout and
crash transitions preserve this key. Automatic reconciliation requires normal
child completion, unchanged identity/configuration, the same canonical remote
and fresh exact local/remote ref evidence. Missing proof, signal termination,
remote advancement or provider errors remain unknown; no fetch is retried and
PID disappearance alone never grants authority. Legacy records gain no synthetic
completion evidence. Host-operation schema 2 adopts existing schema-1 rows and raises the writer
floor because older host writers cannot honor parent/resource crash barriers.
Already running broad writers retain their parent lock until completion; future
old-runtime opens refuse schema 2. Deploy the matched router/engine to all local
writers when activating this protocol. Broker operation details remain additive;
this grants no new deletion rights.
Canonical remote identities containing the reserved Git-resource separator are
unsupported for remote writes. Such requests refuse before creating operation
rows; existing ledger records remain intact. Use an unambiguous remote path.

Opt-in push hooks outside the remote lock run against OID-pinned source
refspecs; the real push uses the identical pinned sources. The original local
refs are revalidated after waiting, but later movement cannot change the sent
objects. Hooks must not assume that their `local_ref` column names the original
branch; `local_sha` is the verified object. `AETHYME_PUSH_REFS` retains the caller's
original refspecs. Pushes setting upstream retain ordinary Git hook execution
and named-source semantics instead of the outside-lock optimization.

Coordination wait details retain schema 1 and add `blocked_scope`,
`safe_next_action` and `missing_evidence`. Matching running operations supply the
holder identity and existing phase/progress timestamp. A resource wait does not
attribute unrelated ref activity as its holder.

`broker push` overlap rows may include bounded `line_ranges` alongside the complete
`files` and `conflicting_hunks` result. At most five shared paths and eight inclusive
old-side conflict spans per side/path are shown, with explicit truncation and
whole-file markers; line 0 can represent an insertion before the first old line.
`pull_request_basis` distinguishes a locally verified tracked-default merge base
from `provider_diff`, whose exact base was not verified locally. Provider ranges
are diagnostic spans from each input diff. Provenance does not establish that
the two diffs have identical base commits; do not assume a shared coordinate
basis from the provenance label alone.

`broker advanced leases plan --json` adds `shared_edit_owners` derived only from
its existing conflict snapshot. `coordination_history.loaded: false` explicitly
means notes and release-request history were not scanned; the included status and
note-list commands provide a deliberate drilldown. No source/output relationship
is inferred from generated-path classification.

## Inventory

### YAML configuration files (#1223)

Every human-authored `.aethyme` configuration stem (`config`, `gates`, `prepare`, `broker`) may now be written as `<stem>.toml`, `<stem>.yaml` or `<stem>.yml`. Exactly one may exist. If two are present, every reader refuses with `config_ambiguous_format` and names both files; neither is preferred. YAML is a strict YAML 1.2 subset converted into the same value the TOML file produces, so semantics are identical. The subset refuses `null`, duplicate keys, tags, merge keys (`<<`), non-string keys, integers outside i64 and multi-document streams, and caps alias expansion. Readers include checkout reads, reads at a commit (gate trust, `[promote]`, delivery policy, graph policy), init/certify existence checks and the collaboration enrollment reads. `GATE_POLICY` classification covers the `.yaml`/`.yml` twins of `gates` and `config`. The preparation source digest and gate-policy comparisons are over the parsed value, so converting a file between formats does not by itself invalidate caches or count as a policy change. Writers never rewrite YAML: `aethyme upgrade` reports a `manual_yaml_edit_required` blocker carrying the exact patch, and `aethyme deploy --graph-repository` refuses with the same code and prints the YAML to add. Rust consumers load configuration through `aethyme_config::load_config(dir, stem)` (re-exported as `aethyme_broker::config_source`).

Compatibility: TOML-only repositories see no change. An older binary ignores a YAML file and behaves as if the stem were absent, so a repository that adopts YAML needs every agent on a binary with #1223. A wrapper that probes `.aethyme/gates.toml` to decide whether gates exist must also check the YAML twins. Covered by `aethyme-config` unit and lint tests, `aethyme-broker/tests/config_formats.rs`, and `repository_upgrade::yaml_config_tests`.

### Reviewed worktree identity recovery (#847)

The operator-facing commands are
`aethyme broker gc storage identity plan <host-operation-db> <exact-root> [--json]`
and `aethyme broker gc storage identity apply <host-operation-db> <exact-root>
--confirm <old-sha256:new-sha256:owner-marker-sha256> [--json]`.
The CLI exposes these commands, and operator recovery instructions consume
this contract. Renaming the commands or changing the confirmation tuple
requires updating both.

Planning reports the exact saved and proposed records without replacing the
saved record. Apply requires all three reviewed hashes, revalidates the exact
root, origin, repository binding and owner marker under the registry lock,
and refuses changed or unknown evidence. Recovery requires matching directory
inodes and current nonzero volume UUIDs; it does not establish the historical
volume UUIDs of a legacy record. Unresolved history remains retained. An older
strict record decoder may refuse the recovered record, so operators must keep
host state and use a compatible runtime. See
[the storage lifecycle recovery contract](local-v3-storage-lifecycle.md)
before applying a plan.


### Repository-scoped worktree-root bindings (#1002)

The public operator surfaces are `aethyme broker advanced worktree-root
[--json]`, `worktree-root binding plan <exact-root> <layout> [--json]`,
`worktree-root binding apply <exact-root> <layout> --confirm <sha256>
[--json]`, `worktree-root recovery plan [--json]`, and `worktree-root
recovery apply --confirm <sha256> [--json]`. The full command syntax and JSON
fields are documented in [the broker CLI reference](../reference/cli.md).
Opening the placement command may attach the current Git common directory to
an unbound repository row; it does not create or bind the root. Binding and
recovery plans are read-only, and apply requires the exact digest from the
reviewed plan. Recovery accepts only the supported durable prefixes of a fresh
reservation.

Schema v54 adds the per-repository binding record. The v54 migration retains
compatibility floor 52 for legacy unbound databases; identity-bound writes
raise that database's floor to 54 so v53 writers refuse it. The schema v53
`spawned_worktree_instances` witness remains intact. Consumers should preserve
the version-1 binding record's lower-snake-case states, origins, layouts, and
sources, and tolerate nullable fields as documented in the CLI reference.

### Detached whole-checkout continuation (#997, #999)

The same private worker can process a separate whole-checkout request after an
enabled automatic cleanup pass in `Inline` or `Drain` scope defers work.
Artifact backlog or a cache-only request alone does not create checkout work;
only the qualifying pass's own checkout deferral does. `FinishDrain` never creates
that request. Its requested generation, completed generation and retry-after value
are separate from cache-lap completion and use the captured storage-context
namespace. The worker does not read, resume or rewrite an operator's
`.aethyme/gc-journal.json`.

The installed-pair and singleton-worker checks above still apply. Each checkout
pass also acquires the GC lock, rechecks the exact request generation and runs
the ordinary automatic-removal proofs. A request is scheduling state, not
deletion authority: the complete colocated closed-session set, persisted spawn
witness, fresh instance identity, clean state, remote containment and current
process-use protection remain required. A stale worker cannot complete a newer
generation. Non-store cache-lane failures are deferred and do not suppress an
independently due checkout lane; cache-lane store errors remain fatal. A generation
completes only when policy is valid and the pass reports no deferred work (or
cleanup is disabled), and the request generation is still current. Kept or
protected checkouts can remain after a generation completes.

The worker reserves time for checkout work and stops after bounded passes or
repeated no-progress, retaining incomplete work with retry backoff. Scheduling
builds complete canonical cohorts and live-path evidence before rotating the
durable cursor, then inspects selected cohorts. A deferred cohort is
checkpointed before ending that tranche so a later pass can reach the next
cohort. Complete history discovery and individual filesystem or database calls
can exceed the cooperative deadline. This is neither a hard completion deadline
nor a repository storage quota; retained or unknown work may continue occupying
space.

### GC journal recovery commands (#852)

The operator-facing `aethyme broker gc abandon --confirm <pending-digest>`
command archives a validated pending GC journal and clears the active journal
while holding the GC lock. Schema 2 journals may be archived for recovery;
this runtime resumes only schema 3 journals, whose row fingerprints bind the
reviewed candidate contents. Unknown future schemas are refused. Abandonment
never resumes deletion or removes project rows, files, worktrees, or artifacts.
A fresh plan and a separate apply confirmation are required afterward; because
plan digests are deterministic, a fresh plan can have the same digest.

`aethyme broker gc forget-abandoned --confirm <archived-plan-digest>` explicitly
retires only the matching private journal archive or validated interrupted
staging files. The archive has a 32-file / 64 MiB admission limit, is never
pruned automatically, and cannot be forgotten while its exact bytes are needed
for active-journal crash recovery. Neither command accepts an arbitrary archive
path or grants deletion authority.

### Metrics-log trim, drifted files and resume backoff

These changes let a confirmed journal finish when the files it reviewed have
changed since review:

- **Append lock.** Writers of `.aethyme/logs/command-metrics.jsonl` take a
  shared `flock` on `.aethyme/run/command-metrics.lock` around each append. The
  GC trim takes it exclusively from its read until after its atomic rename.
  Once every writer takes the lock, no row can be lost. A writer waits at most
  two seconds and then appends unlocked, which is how older binaries always
  append. The trim narrows the risk from unlocked writers in two ways. An
  append it sees before the rename makes it retry, up to three attempts. After
  the rename it watches the replaced inode for 250 ms, still holding the lock,
  and copies what lands there into the new file: whole lines while it watches,
  and any trailing bytes without a newline, as written, when the period ends.
  An unlocked writer that opened the file before the rename and writes after
  that period still loses its row. When every attempt sees an append, or the
  lock cannot be opened or taken, the trim is skipped, nothing is removed and
  the journal continues. Each trim first removes `.command-metrics.jsonl-XXXXXX`
  temporaries (regular files with exactly that name pattern, in the log's
  directory) that an interrupted trim left.
- **Digest.** A plan digest binds the command-metrics rewrite as `path`,
  `action` and the rule `trim: "rows_older_than_age_cutoff"`. The cutoff is
  bound through `age_anchor_ms` and `policy.command_metrics_days`. The
  candidate's `before_sha256`, `after_sha256` and byte counts stay in the plan
  JSON as reporting only. Gate-log candidates are bound exactly as before. The
  digest of a plan that contains a metrics rewrite therefore differs from the
  one an earlier binary printed, so review that plan again after upgrading.
- **Apply.** Apply trims whatever the log holds when it runs and no longer
  compares the metrics hashes. That includes schema 4 journals written by
  earlier binaries, which resume instead of failing with "reviewed artifact
  drifted". A gate log whose hash no longer matches is retained, never
  deleted, and recorded in the journal's additive `files_skipped` array
  (`path`, `reason`). The `gc apply` JSON report gains the same field, omitted
  when empty. Older binaries ignore the journal field and drop it when they
  rewrite the journal.
- **Resume backoff.** When open-time resume fails in a way the next open
  would repeat (an invalid or unsupported journal, a confirmation mismatch),
  broker meta key `gc_journal_resume_failure` records
  `{journal_sha256, failed_at_ms}`. Open-time resume skips that exact journal
  for 15 minutes; changed journal bytes are retried at once. The returned error
  names `gc apply --confirm <digest>` and `gc abandon --confirm <digest>`. A
  held GC lock, a busy database or an I/O error arms nothing and is retried on
  the next open, as before. An explicit `gc apply` never reads the record; it
  clears it when the journal completes, and the first writable open without a
  journal clears it after `gc abandon` (which opens a read-only snapshot).
  Read-only snapshot opens never reach the resume path.
- **Counters.** These appear in the command's `phase_counts`:
  `gc_metrics_trim_retried`, `gc_metrics_trim_tail_recovered`,
  `gc_metrics_trim_skipped_concurrent_append`,
  `gc_metrics_trim_lock_unavailable`,
  `gc_metrics_trim_stale_temporary_removed`, `gc_file_skipped_drifted`,
  `gc_journal_resume_failed`, `gc_journal_resume_backoff_armed` and
  `gc_journal_resume_backoff_skipped`.
- **Abandon.** `gc abandon` accepts schema 2, 3 and 4 journals, as it already
  did; only its help text said otherwise.

### Spawned-worktree instance witnesses (#995)

Broker schema 53 adds `spawned_worktree_instances`, with a session ID and a
versioned witness captured from the checkout returned by Git worktree creation.
The session and witness are recorded in the same transaction. Later automatic
whole-checkout cleanup compares that persisted instance with fresh filesystem,
Git identity, ownership and lifecycle evidence before removal. A path or session
registration alone is insufficient authority.

The migration does not backfill historical or adopted checkouts. Missing,
changed or unknown evidence retains the checkout. The minimum compatible schema
remains 52: compatible older writers can register sessions without this optional
table, and this runtime will not automatically remove those unwitnessed
checkouts. This marker does not fence every older binary's cleanup behavior.
Drain old clients and deploy an aligned router/engine pair before relying on the
new cleanup policy across a repository.

### Detached automatic artifact continuation (#818)

The broker starts the private `broker gc __continue-artifacts <canonical-main-root>` child only from the installed router paired with its sibling engine. The child requires `AETHYME_GC_ARTIFACT_SWEEP_WORKER=1`, a writable non-ephemeral main checkout, aligned router/engine build identities, and the OS advisory singleton lock before it opens broker state. Its artifact lane may remove eligible ignored build caches; that lane never removes a checkout or branch. A separate durable request can enable the whole-checkout lane described below. It records completion only after a full eligible cache lap; deadline, lease contention, deletion failure, or repeated no-progress leaves the generation pending with bounded backoff. A new pressure refusal clears that backoff. The command remains hidden from help and is invoked only by the paired installed runtime.

Open-session retirement has its own `gc.artifact_sweep.session_retirement_state`
metadata row: version 1 stores the requested generation, completed generation
and retry deadline together as one compare-and-set value. Eligible Inline and
Drain sweeps may enqueue one idempotent generation, including when the artifact
budget is zero. The paired worker handles this lane independently of cache
progress, whole-checkout removal policy and the closed-checkout cleanup
outbox's grace deadline. It advances only the captured generation while holding
the GC lock after a complete retirement lap. Busy storage, unknown liveness,
page limits and deadlines leave work pending with bounded retry. Malformed,
unsupported or inconsistent state is left untouched and grants no retirement
authority; other worker lanes remain independently eligible. This row does not
authorize checkout removal or branch deletion and adds no CLI report field.

Changing the private command, marker, canonical-root argument, runtime-alignment check, or lock handoff without updating both launcher and child can strand cleanup requests or run a mismatched binary. The child must leave incomplete requests retryable and must not report them complete.

Before removing an eligible closed-session build cache, the sweep also tries the
B1 per-checkout cooperative exclusive artifact-use admission after candidate
eligibility is rechecked and before publishing a new removal intent or
performing deletion. The reservation stays held through tree removal and the
interrupted-path progress update. A busy producer, unknown marker, malformed or unavailable inventory, or unavailable protocol
retains the cache and keeps the cleanup
request pending and incomplete. While a path is present as a symlink or
replacement, it remains blocked and queued. Proven absence can retire a
queued path without removal; other eligibility revocations remain separately
reported. A resolved present path goes through fresh eligibility and admission
checks. This exclusion does not authorize
cleanup of open worktrees and does not prove participation by older clients,
unmanaged direct commands, or escaped descendants; an empty marker inventory
alone is not cleanup authority. It adds no cleanup report field or broker
database schema; blocker reason strings remain opaque to consumers.

New producer markers use schema 2 only when a nonzero stable volume UUID was
captured for both the primary repository and checkout roots. On macOS, capture
uses a no-follow object check before and after reading the volume UUID. This is
best-effort: if either capture is unavailable, the producer writes schema 1
without either UUID and requires exact device identities during normal marker
admission and inventory. Explicit reconciliation keeps the existing validated
registry translation for a schema-1 primary device. Other platforms keep that
schema 1 behavior. Schema 2 still binds both exact root paths and inodes;
a changed device number is accepted only when both saved UUIDs match the current
roots. Marker readers that do not support schema 2 fail closed. Reconciliation
still requires the current validated primary-root registry witness, the exact
closed-checkout cohort, and the existing process-start and process-group
absence proof before removing a marker.

The nine durable cache-sweep keys are scoped by a SHA-256 namespace derived
from the complete captured broker storage context. Linked worktrees in the
same repository and using the same captured context share sweep state; a
different effective host-storage configuration gets a separate cursor and
worker generation. The repository-local namespace registry is transactional,
append-only and capped at 64 entries. A full or invalid registry refuses the
sweep without reading or changing scoped state; entries are never evicted.

The nine pre-namespace keys are inspected with bounded, read-only checks and
are never migrated or rewritten automatically. `gc sweep --json` adds
`legacy_unscoped_state` (`clear` or `unknown_pending`), and the text output
reports the same state. Malformed, non-text or oversized values, an unfinished
request, a nonzero cursor/progress marker, interrupted or blocked paths, or a
future retry time remain `unknown_pending`. A `clear` legacy value means no
known old backlog; `complete` means only that the current captured-context lap
was reported complete, not that every registered context or historical value
is resolved.

Consumers of the report must tolerate this additive field and must not treat a
current-context completion as global cleanup completion.

### Separate preparation-cache placement

The shared preparation cache can be placed independently of the host-resource
database with `AETHYME_PREPARATION_CACHE_ROOT`, or with
`BrokerOpenOptions::with_preparation_cache_root` in an embedding application.
The outer directory must already exist and be a real canonical absolute path.
On first initialization it must contain no unrelated entries; later opens
reuse it only when its marker and cache namespace validate. The host-resource
database remains the coordinator for leases and rotation journals; it is not
relocated by this option. The first writable broker open checks journal state
under the repository-scoped preparation lease before it initializes the root
marker and nonce-scoped cache directory. Capture and read-only planning do not
initialize the root. An initialized root is bound to the canonical coordinator
database path and refuses a different canonical path. The coordinator database
file must also remain stable; replacing it at the same path while connections
are live is outside the guarantee. Existing cache data is not migrated or
removed by selecting a new root.

The private child-storage context carries the effective cache-root binding to
the broker-owned artifact-continuation worker. No override keeps the existing
V2 child context and default cache namespace; an opted-in root uses the private
V4 child context so linked worktrees and continuation workers agree on the
root identity. Preparation writers, rotation recovery, and cache reclamation
use the same captured root and repository-scoped lease. The root setting
changes placement only; it does not impose a disk quota or bound arbitrary
writes made by preparation commands.

### Host-wide preparation install cap

`submit prepare` holds one unit of the host capacity pool
`aethyme-prepare-installs` (request run id `prepare-install-s<session>`,
memory-pressure admission) while its steps run. The limit comes from
`<host state>/preparation.toml` `[installs] max_concurrent` (1 to 64) or
defaults to min(CPUs / 2, physical memory / 4 GiB), at least 1, persisted in
`<host state>/preparation-install-limit.json`; a request always adopts the
active holders' limit. Steps receive `AETHYME_PREPARE_INSTALL_SLOT`, and a
nested preparation reuses that slot. A failed slot renewal never stops a
running install. Output
contract (additive): the prepare report and the `preparation` metric object
gain `install_slot` (`limit`, `limit_source`
`configured`/`default`/`active_pool`/`parent_slot`, `wait_ms`,
`other_holders`, `configuration_warning`, `renewal_failures`). A wait that ends with every slot busy is a new
error, "no host install slot for session N: H of L preparation install(s)
are running ...", with exit 6 (environment); a refusal waiting cannot resolve
exits as refused. Renaming the pool, the file or the key breaks hosts that set
or read them.

### Shared package-manager content caches

A `repository_shared` preparation step may opt into `shared_package_caches`
with unique values `pnpm`, `npm`, `uv`, or `pip`. The broker exposes only the
selected `AETHYME_PREPARE_*_DIR` paths. By default each is beneath the
repository-wide package store `<cache root>/<repository>/packages/<os>-<arch>/`,
shared by every preparation digest: the store key is the captured cache root,
canonical remote identity, OS, architecture and manager directory, and the
manager's own layout version (pnpm `v10/`, uv and pip wheel tags) keeps
incompatible content apart. Every digest directory links `pnpm-store`,
`npm-cache`, `uv-cache` and `pip-cache` to that store with relative symlinks,
so `AETHYME_PREPARE_CACHE_DIR/pnpm-store` reaches the shared store; an existing
real directory there (the earlier layout) is left in place.
`[shared_cache] package_store = "per_input"` keeps the earlier per-digest
stores; the default is not serialized, so existing preparation digests and
trusted policies are unchanged. The `packages` directory is never swept as a
stale digest; only a whole-store rotation under the exclusive lease removes
it, and a retired store containing it keeps a managed layout.

Runtime keys: the preparation digest keys each `[[runtimes]]` entry on the
SHA-256 of its probe's standard output, no longer on the executable's size
and modification time (a one-time rekey: every checkout is stale once after
upgrading). `submit prepare` runs each probe on exactly the resolved file (5 s
limit, own process group, trusted policy, storage not under pressure) and
caches verified outputs per executable and command in the checkout's Git
directory (`aethyme-runtime-probes.json`), from which status reads and
storage sweeps compute the same key without running anything. A failed,
unstartable or timed-out probe is `unverified`: its key is the executable
identity plus a per-run nonce, never shared, and the next preparation probes
again. A permanently failing probe makes every preparation reinstall, the
checkout never reports `current` (so `required_for_hooks` steps block hooks),
and each run's digest directory is reclaimed by the next sweep; advice
`prepare.runtime-probe-unverified` (metadata key
`preparation.runtime_probe_unverified`) names the probe and its reason. Advice `prepare.runtime-probe-unstable` (broker metadata key
`preparation.runtime_probe_unstable`) reports a probe whose output changed
for an unchanged executable. `[shared_cache] max_bytes = 0` now means the
8 GiB default rather than unbounded.

Package-store reclamation: preparation-cache sweeps (`gc` and the storage
recovery worker) rotate `<repository>/packages` under the repository's
exclusive lease when its volume is under the free-space reserve or a bounded
measurement finds it over the repository's `[shared_cache] max_bytes` (each
preparation records it in `packages/.aethyme-store-cap`; 8 GiB when unset or
zero), rename it to `packages-retired-*`, and remove
retired stores without a lease. A preparation refused for storage tries the
same rotation first. Telemetry phases: `package_store_rotated_pressure`,
`package_store_rotated_over_cap`, `package_store_rotation_busy`,
`package_store_retired_removal`, `runtime_probe`, `runtime_probe_unverified`.
Since 2026-11-23 a preparation never removes the retired store itself: it
renames it, persists a storage-recovery request and starts the storage worker,
whose sweep removes it (span `package_store_retired_handoff`, counters
`package_store_retired_handed_to_worker` and
`package_store_retired_handoff_failed`). `package_store_retired_removal` is
now recorded only by sweeps.

Mixed versions: binaries before this change compute different digests, so
they never open a digest directory holding the new links; their unlink-based
removal does not follow the links. They do report preparation-cache sweeps as
incomplete while `packages` exists, and keep a retired whole-repository store
that contains `packages` until a current binary reclaims it.

Output contract (additive): the prepare report gains `package_store`,
`cache_outcome` (`hit`, `miss_reason`) and per-step `volume_bytes_allocated`;
the command metric's `preparation` object carries `package_store` and
`cache_outcome`; `submit prepare status --json` gains `runtimes` and `package_store` from the
session's last recorded preparation (store path, scope, key components,
runtime identities, `same_volume`, `import_method`, free-space readings);
`broker status` gains the warning `prepare.store-on-other-volume`, read from
the broker metadata key `preparation.package_store_volume`. The command must
explicitly route its manager to the alias; native package-manager variables
remain caller-controlled. `CARGO_HOME` is deliberately unsupported because it
also contains configuration, credentials, and executables.

Changed 2026-11-18 (dependency bytes shared between checkouts). After a
repository-shared preparation's steps succeed, still under the cache lease,
the broker shares byte-identical files of 64 KiB or more under the outermost
Git-ignored directory of each declared output (for example `.venv/`) with
other checkouts by copy-on-write clone through
`<repository>/packages/<os>-<arch>/output-pool/<sha256[..2]>/<sha256>`,
read-only entries inside the package store, so its cap, sweep and whole-store
rotation cover them. The pass replaces a checkout file only with a clone of
identical bytes and restores its mode and timestamps; it never hard-links,
never touches tracked paths, and runs only where the probe found clone
support. Output contract (additive): `package_store.output_share` and
`package_store.cache_root_placement` in the prepare report and
`submit prepare status --json`; the command metric gains
`prepare_dependency_sharing` (counters, `unique_bytes_before`,
`unique_bytes_after`, no paths), `prepare_cache_root` and the
`prepare_output_share` span and the `prepare_cache_root_*` placement
counters; the storage sweep also retires a shared package store unused for a
week (`package_store_rotated_idle`, judged by the mtime of the store's
`.aethyme-store-cap` record). `[shared_cache] root` gains `auto`, now the
default: the host root when it shares the checkout's volume, the
worktree-volume root otherwise. `auto` serializes as `host`, so no
preparation digest or trusted policy changes. Older binaries refuse
`root = "auto"` written explicitly; omit it to stay readable by them.

The preparation schema remains version 1. An empty list is omitted from the
serialized configuration to preserve existing preparation digests; a
nonempty selection changes the digest and trusted prepare policy, and requires
trust before execution.

### Managed-storage ledger schema v4

The host-resource database contains a separate managed-storage ledger version
in `managed_storage_meta`. It is independent of the host-resource store version
in `meta`. Ordinary writable coordinator opens validate the managed-storage
schema they find and preserve its version; they do not advance a v1 ledger to
v4. This keeps the managed-storage floor unchanged for other repository
worktrees sharing the host database.

The crate-private managed-storage activation path is bound to the coordinator's
already-open database identity. It requires the exact host-wide confirmation,
rechecks the captured database path identity around an immediate transaction,
and applies v1→v2 claim-group, v2→v3 producer/step, and v3→v4 settlement-proof
and adoption-table migrations. The migration adds ledger tables and replaces
the producer fences needed by v4; it retains existing claims, lineages,
producers and steps and does not synthesize settlement proofs for old rows.

Activation is not exposed as a CLI command and must not run as part of a
repository-only deployment. The host database is shared across repositories,
so an operator must coordinate quiescence of every writer that may use it,
including older installed binaries, before any explicit activation. The path
identity checks detect observed replacement or alias drift, but do not
atomically pin SQLite's file handle against an uncooperative replacement
between checks; they are not a substitute for that coordination.

A producer is resolved only when its steps are complete and a settled claim
group and immutable settlement proof agree on the producer, lease generation,
root set, inventories, manifest, plan and resulting receipt digests. Existing
v3 settled rows have no v4 proof because migration does not backfill one; they
remain unresolved and root mutations stay fenced until separately reconciled
with evidence that satisfies the v4 contract. The v4 schema and migration do
not by themselves enable profile-bound root capture or connect preparation
dispatch to managed-ledger admission.

### Private spawned-agent supervision (#831)

The router and broker shim dispatch the reserved
`--aethyme-agent-supervisor-v1` and
`--aethyme-agent-supervisor-cleanup-v1` modes before ordinary CLI parsing.
They are paired internal process protocols, not operator commands. Launcher
and child must agree on their version, storage context, exact executable and
registration handshake; malformed or unsupported invocations refuse.

Library embedders whose own executable does not implement these modes must
select a compatible local runtime with
`Broker::with_agent_supervisor_executable`. Selection resolves an exact file
without PATH search; the caller chooses its trust and version policy. A
missing, nonregular or nonexecutable selected file refuses agent start before
allocating a checkout, session or log. A file passing those checks must still
implement the paired private protocol; protocol failure follows transactional
startup rollback. Unix supervision is required; unsupported platforms return
an explicit error.

Spawned-agent logs retain at most 16 MiB across stdout and stderr, with a
bounded prefix and tail. They are diagnostic excerpts. Interrupted capture
remains explicitly incomplete. Cleanup covers ordinary descendants in the
owned process group; it is not containment of programs that deliberately
escape that group. Consumers must preserve the private dispatch and exact
runtime selection when changing entrypoints.

### Generic guarded-command supervision

`Broker::guarded_exec` and its environment/heartbeat variants supervise the
command on macOS and Linux when opening `/dev/tty` confirms there is no
controlling terminal (`ENXIO`). The helper preserves argv, cwd, environment
override order, inherited stdin/stdout/stderr, and the existing ownership
snapshot/audit. Preparation continues to start its helper with null stdin; the
generic guarded command inherits the broker's stdin, including a pipe.

The noninteractive path requires an executable that implements Aethyme's
private inline-exec protocol. A CLI uses its own executable by default; a
library embedder must forward `agent_supervisor::dispatch` before normal
argument parsing (it dispatches inline-exec and agent protocols), or configure
`Broker::with_agent_supervisor_executable` with a compatible local executable.
Resolution is exact and does not search PATH.
An unavailable or incompatible helper refuses before the command request is
sent; the broker never retries through direct spawn after helper failure.

The inline-exec wire protocol uses the reserved
`--aethyme-inline-exec-supervisor-v4` entrypoint and `AIX4` frame marker. The
v4 helper emits a `Started` frame only after the child process has spawned;
older v1/v2/v3 helpers reject the v4 entrypoint before receiving a command. Keep
the entrypoint suffix and frame marker in sync when changing this private wire
protocol. The spawn-failure frame carries a stable tag for known
`std::io::ErrorKind` values plus an optional raw OS error code, preserving
NotFound, PermissionDenied, InvalidInput, and the underlying OS code. A kind
without a stable wire tag is sent with an explicit diagnostic; the receiver
reconstructs it from a raw OS code when available, or maps it conservatively
to Other without one. An unknown wire tag fails closed as an uncertain
command result.

When a controlling terminal is present, its probe is inconclusive, or the OS
is unsupported, the broker preserves the legacy direct runner and prints a
degraded-mode warning. Descendant cleanup is not verified on that path, so it
is not suitable as evidence of process quiescence or managed-storage admission.
`GuardedExecOutcomeUnknown` means the helper verified cleanup but could not
recover the command result; ownership audit was skipped. After dispatch, a
heartbeat or other broker callback failure returns `GuardedExecOutcomeUnknown`
once cleanup is verified, because the callback error does not make the command
result known; ownership audit is skipped. `GuardedExecCleanupUnknown` means
cleanup could not be verified and ownership audit was skipped. Unknown states
require inspecting the worktree before retrying. Downstream exhaustive matches
on public `BrokerOpError` must handle the new unknown-state variants.

### Primary-checkout fast-forward (#232)

Before `status`, `start`, `sync`, `push`, `submit` or `finish` runs from the primary checkout (hooks and advanced commands never move it; the same set as before), the broker checks the current branch against its configured, already-fetched tracking ref. The checkout advances only by fast-forward, only when it is on the default branch (the branch `origin/HEAD` names, or its own upstream when that is unset), has no unique commits, no Git operation in progress (merge, cherry-pick, revert, bisect, rebase, a held index lock), no sparse-checkout, assume-unchanged or skip-worktree entries, and no broker session adopted on it that is in use. A session adopted on the primary checkout is in use while its spawned process or recorded holder is alive, while the process listing cannot prove the holder gone, while one of its coordinated operations is unresolved, or for 30 minutes after its last recorded activity; an idle session with no live holder does not hold the move. After such a move every open session adopted on the checkout gets an automation note (sender `automation:main-checkout-sync`, fingerprint `main-checkout-advance:<from>:<to>`) saying the checkout advanced from `<from>` to `<to>`, and its diff base moves to `<to>` when its old base is in the default branch's history, so the upstream commits are never counted as that session's work. `status` is read-only and moves only a clean checkout, as before (a dirty one is recorded as `deferred_to_background`, silently); the mutating commands and the background worker also move a checkout whose local changes do not overlap the update. A path changed between HEAD and the target that is also modified, staged or renamed locally, or any untracked or ignored file at, above or below a path the update writes (compared case-insensitively), holds it. One exception: a locally modified (not staged) tracked file that is provably Aethyme's own generated output does not hold it. Proof is of the bytes, never the path: the working bytes already equal the update's (`matches_target`); `AGENTS.md`/`CLAUDE.md` whose generated receipt still hashes its whole body (`unedited_generated_policy`); a static deploy target byte-equal to the running binary's render (`current_template`); or a deploy-written path whose exact bytes were committed at that path in the default branch's last 200 commits touching it (`recorded_in_history`). `.aethyme/config.toml` is owner policy and qualifies only through `matches_target` (an owner may deliberately set it back to an older committed version), so anything else there holds (`owner_edits`); `.aethyme/overrides/` is never a candidate. A proven file is re-hashed, copied to `.aethyme/run/main-checkout-replaced/<epoch-ms>/<path>`, the copy hashed against the proven blob and flushed to disk, and only then put back to its committed content (`checkout-index` of that path only) inside the journaled step, after the marker and before the tree update; a file that changed since its proof, or a backup that does not verify, stops the step with nothing replaced. If the tree update then refuses, or the ref swap loses with HEAD unmoved and the tree goes back, each replaced file that still holds its committed content is copied back from its backup. Backup sets older than 7 days, or beyond the newest 20, are removed (`main_checkout_generated_backups_pruned`). Generated files that cannot be proven hold and are named with their reason (`edited`, `staged`, `owner_edits`, `unreadable`) and what to do. `aethyme deploy` run in the primary checkout of a broker repository now warns when it leaves generated files modified there, since nothing commits from that checkout. So do clean/smudge filters (`filter.<name>.smudge` or `.process` configured, and a `filter` attribute on a changed path or a changed `.gitattributes`) unless `[main_checkout] allow_filters = true`, because a filter can run programs and reach the network (reason `filters_configured`). The broker's own files under `.aethyme/` (its runtime paths, `gc-journal-archives/`, staged `.gc.lock.<pid>.<nanos>` files and `.operator-*-maintenance-hold-*` directories) are never counted as local changes.

Each move step writes the index and working tree first, with a two-tree `read-tree -m -u <from> <to>` that Git refuses rather than overwrite any local change, and moves the branch ref last by compare-and-swap (`update-ref <ref> <to> <from>`); if the swap loses, HEAD is re-read: at `to` (another writer fast-forwarded to the same target) the step is done; still at `from` the swap is retried once and otherwise the tree goes back by the reverse two-tree merge; anywhere else (another commit, or past `to`) the tree is left as written, the marker is kept and the checkout is held as `interrupted`. No stash, reset or forced checkout, submodules are not updated, and repository hooks do not run (`core.hooksPath` is pointed at a path that holds none; `read-tree` and `update-index` run none). Each Git command of a step has its own 30-minute bound, independent of the command's inspection deadline. A jump whose change set exceeds 2,000 paths advances in steps along the target's first-parent line, each at most 2,000 paths: one step per foreground command, up to 64 steps or 10 minutes per background attempt. Every step is journaled in `.aethyme/run/main-checkout-sync.json` (`schema_version`, `branch`, `from`, `to`, `started_at`, `pid`), created before the tree update and removed after the ref update. An attempt that finds the marker of a process that is gone (or older than two hours) resolves it before anything else: HEAD at `to` means the move finished; HEAD at `from` with every changed path at `to` in index and tree finishes it with the ref swap; HEAD at `from` with every changed path still at `from` drops it. Any other state keeps the marker, holds with reason `interrupted`, and `status` shows advice `main-checkout.interrupted` (severity `blocked`) with the inspection commands; no further move starts and `current` is never reported until it is resolved.

A foreground success records `broker.checkout.fast_forwarded` with `trigger` (`broker.<command>`), `branch`, `upstream_ref`, `from` and `to` (background moves are recorded in the sync state below, with `trigger` `broker.worker`, not as events) and prints `Updated main checkout: <branch> fast-forwarded from <from> to <to> (<upstream_ref>).` to stderr, followed by `  <n> commit(s); <m> local change(s) left as they were.` when local changes rode along. A held checkout prints `warning: left the main checkout unchanged: <reason>. Run: <shell-quoted-command>` to stderr, except when it is on another branch, `[main_checkout] auto_sync = false`, or `status` deferred a dirty checkout, which are recorded silently. This check does not fetch. Compatibility read-only snapshots and linked session worktrees are never moved; session worktrees use `broker sync --session <id>`. Standard output is unchanged, so JSON reports stay parseable.

The detached background worker also attempts the update on a timer: a broker open claims the slot at most once per `[main_checkout] sync_interval_minutes` (default 15; `0` disables the timer) through a compare-and-set on broker meta key `main_checkout.sync_due_at_ms` and leaves `main_checkout.sync_requested` for the worker. The worker sweeps the broker's own litter, fetches only the default branch once, with exponential backoff after failures, then attempts the same steps under a nonblocking lifecycle claim (lane `main_checkout_sync`) that yields to any queued foreground command. That fetch does not use the coordinated operation lane, which records every operation against a session row the worker does not have (the same exception as `start`'s default-branch refresh): it is `git -c core.hooksPath=<none> -c credential.interactive=never fetch --no-tags --no-prune --no-recurse-submodules --no-auto-gc --quiet <remote> refs/heads/<default>:refs/remotes/<remote>/<default>` with `GIT_TERMINAL_PROMPT=0`, an explicit non-forcing refspec that can update only that one remote-tracking ref, and Git's own ref locking orders it against any concurrent fetch or push of the same ref. Every attempt records its result as JSON in broker meta key `main_checkout.sync_state`; `broker status --json` (compact and `--full`) carries it as `main_checkout` (`branch`, `upstream_ref`, `head`, `upstream_head`, `behind`, `ahead`, `last_sync_at`, `last_attempt_at`, `last_result` = `current`, `advanced`, `held` or `no_upstream`, `reason`, `detail`, `blocking_paths` (at most 10), `blocking_paths_omitted`, `next_command`, `trigger`, `last_fetch_at`, `fetch_failures`, `fetch_error`, `fetch_failing_since`, `next_fetch_at`, `behind_since`, `blocking_session_id` = the adopted session that held the last attempt), omitted until a first attempt is recorded in an existing broker database. The session view that `status --json` prints from a checkout with a session (`aethyme.broker-session-view/v2`, including a session adopted on the primary checkout) carries the same `main_checkout` object and, when advice applies, `main_checkout_advice` (the `main-checkout.*` advice rows); like the compact form's `main_checkout`, neither is a revision section, so a `--since-revision` delta does not report their changes. `reason` is one of `local_changes_overlap`, `not_on_default_branch`, `detached`, `local_commits`, `operation_in_progress`, `hidden_local_state`, `live_session`, `git_refused`, `changed_during_preflight`, `inspection_failed`, `fetch_failed`, `disabled`, `interrupted`, `filters_configured`, `deferred_to_background`. Status advice `main-checkout.behind` (severity `warning`) appears once the checkout is more than `[main_checkout] behind_threshold_commits` (default 50) commits behind (with reason `live_session` its summary names the session, why it is in use and its last activity, and its commands include `aethyme broker status --session <id>` and `aethyme broker finish --session <id>`), or behind and not synced for more than `behind_threshold_hours` (default 24), or when the worker's fetch has failed for that long. `broker start` prints one stderr line beginning `note: the main checkout` under the same thresholds. Command metrics carry the attempt as the `main_checkout_sync` attachment and `main_checkout_*` phases.

### Session pull request open mode (#887)

`broker push --json` reports `default_branch.sync_impact` as `unknown`,
`related`, or `disjoint`. `disjoint` permits deferring an intermediate sync
only for the exact fetched default-branch and session commits and clean
working tree that were assessed. It requires complete, untruncated graph
coverage for both revisions; missing or incomplete evidence returns `unknown`.
A conflict or related surface still requires sync. The decision never replaces
validation of the current merge candidate before merging. The cached drift
comparison is ref-bound; status recomputes worktree-dependent impact rather
than trusting a cached impact verdict.

`aethyme broker push --session <id> --pr` creates a new pull request using the trusted default branch config `[broker.pr] open_as = "draft" | "ready"` in `.aethyme/config.toml`. A missing setting defaults to `draft`. The value is read from the fetched default-branch commit, not the session worktree; invalid values or unsupported keys inside `[broker.pr]` refuse before the session branch is pushed. The mode only applies when creating a pull request: an existing pull request is reported as the provider reports it and is not changed.

This setting is a user-facing broker config contract. Changes to its accepted values, trust source, default, or existing-PR behavior require updating the config template and the session-push contract tests.

### Review-thread conversation commands

`aethyme broker advanced review-thread reply <thread-id> --session <id> --repo <owner/name> --pr <number> --message <text> --reason <text>` and `aethyme broker advanced review-thread resolve <thread-id> --session <id> --repo <owner/name> --pr <number> --reason <text>` add dedicated commands for inline pull-request conversations. Before either mutation, the broker looks up the thread and verifies that its repository and pull-request number match the supplied target. Both mutations use exact allowlisted GraphQL queries and a per-thread coordination lock; the durable journal retains the query and thread ID so it can recover that lock after a restart, while redacting reply text. General PR comments remain available through `broker advanced gh`; arbitrary GraphQL remains refused as unverifiable. Renaming or changing these commands, their target verification, or their journal contract requires updating the CLI reference and the broker's review-thread contract tests.

### Skill template files (canonical sources, deployed by the `aethyme-enhance` crate)

| Source | Deployed to | Invokes | Failure mode if entry point removed |
|---|---|---|---|
| `skills/aethyme/SKILL.md` | `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md` | Short auto-load card. **Since 2026-09-25 (recovery plan P4.5/P4.6)** it leads with the one-call `aethyme explore --request ... --format brief` and states that Explore is a navigation aid whose results must be verified; the full JSON path follows for audit or deeper follow-ups: one bounded native `aethyme explore` call, `aethyme explore-summary --from "$AETHYME_JSON"` for the compact projection (**flipped from a `"$AETHYME_ROOT/.venv/bin/python"` heredoc in the Phase 5.5 projection flip, 2026-08-01**), then `aethyme verify-targets` over the SAME temp file. Links detailed workflows under `references/`, now including `broker.md` and `policy.md`. | Agent reads stale guidance, runs commands that no longer exist, or bulk-loads detailed workflows. Renaming/removing `explore --format brief` or `explore-summary` breaks the Explore step in every enhanced repo (Class-3: silent until an agent runs it). Caught by `verify-playground.sh` greps (`--format brief` and `explore-summary --from` present, no `.venv/bin/python` invocation), `aethyme-cli/tests/skill_templates.rs`, `aethyme-cli/tests/explore_summary_cli.rs`, `aethyme-cli/tests/explore_brief_cli.rs`, and `aethyme broker check-contract` text-consumer validation. |
| `skills/aethyme/references/*.md` | `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md` | Detailed optional workflows: Explore depth/intents/trust (`explore.md`, which since 2026-09-25 opens with the one-call `--format brief` and the verify-before-use rule), graph/task/context commands (`graph-task.md`), dead-code/facts/analyzer commands (`dead-code.md`), and **`policy.md` (introduced 2026-09-25, recovery plan P4.5)**: the full commit-hygiene list, the branch and review practices, generated-file/override upkeep, the removed-Python-CLI notice, and `aethyme deploy verify` — everything the compact root file no longer repeats. `broker.md` is rendered by `agents.rs:render_broker_reference` (not a static template) and now spells commands the #344 way (`broker start --adopt`, `start --reuse`, `advanced leases/exec/git/gh/ship/operations`, `finish cleanup`). All spelled `aethyme ...` since the 2026-07-30 Phase 2 template flip; since the Phase 3 flip (2026-07-29) every referenced `repo` subcommand answers natively (`aethyme_engine::repo_cli` + `aethyme_enhance::repo_cli`), no Python delegation. `references/explore.md` calls `aethyme explore-summary --from` for the compact projection (**flipped from the `"$AETHYME_PY"` heredoc in the Phase 5.5 projection flip, 2026-08-01**). | Enhanced repos receive a short skill with broken reference links or stale detailed commands. Caught by `aethyme deploy verify --generated-only`, `verify-playground.sh` (`explore-summary --from` present, no `.venv/bin/python` invocation in any reference), and reference deployment tests. |
| `skills/aethyme/AGENTS.md` | fully generated `AGENTS.md` and `CLAUDE.md` (deployed at repo root by `aethyme deploy`, aethyme-enhance crate) | **Compact since 2026-09-25 (recovery plan P4.5): about 575 tokens for this repository's generated block, down from about 1,920, held at or under 1,000 by `GENERATED_POLICY_MAX_TOKENS`.** The rendered file is the template header, then a `## This repository` section (name, language, kind, package manager, fast test, app entrypoint, onboarding/act skill and status paths, from generated onboarding), then — broker-configured repos only — `## Broker Coordination: before and after an edit` (`aethyme broker start --task [--short-name]`; `--short-name` is optional and, when omitted, a new session derives its label from the task, work only in the reported worktree, never edit another session's worktree, `broker submit --session <id>`, `broker finish --session <id>`, remote/shared writes through `broker advanced git` / `broker advanced gh`, the action-required file, and a pointer to `references/broker.md`), then the template's `## Finding code` (one line prescribing `aethyme explore --request ... --format brief` as a navigation aid to verify, not a mandatory first step), `## Commits` (compressed typed-commit rule, held to `COMMIT_POLICIES`), and `## Everything else` (pointer to the skill and its references), then override sections. The removed quick start, full commit list, branch/review practices, verification section, Python-CLI notice and broker detail moved to `references/policy.md`, `references/explore.md`, `SKILL.md` and `references/broker.md`. The heading still contains `Broker Coordination`, which `certify`'s agents-protocol check, local-only deploy verification and the upgrade classifier key on. Both the old long form and the new compact form are recognized as generated (`looks_like_generated_agents_document`), so an older file migrates instead of being preserved as hand-written content. Root files are Aethyme-owned generated artifacts; repo-specific human customizations come from `.aethyme/overrides/agents.json`. | Agent reads a stale long form until the repository is redeployed; removing `explore --format brief`, `broker submit`/`finish`, or the `advanced git`/`gh` spellings breaks generated guidance in every enhanced broker repo. Dropping `Broker Coordination` from the heading fails `certify`. Caught by `aethyme broker check-contract` text-consumer validation, `verify-playground.sh` root-guidance greps (`--format brief`, the navigation-aid warning), `aethyme-cli/tests/playground_hygiene.rs`, `aethyme-cli/tests/enhance_cli.rs` (including the token budget), `aethyme_enhance::agents` unit tests, and `aethyme deploy verify --generated-only` canonical-match checks. |
| `skills/aethyme/aethyme-explore` | `.claude/skills/aethyme/aethyme-explore`, `.codex/skills/aethyme/aethyme-explore` (executable) | Resolves the installed `aethyme` router from PATH, records best-effort invocation telemetry, then executes `aethyme explore "$@"`. If no daemon is running, the router starts the native Rust daemon and retries; no Python runtime is involved. | A missing router on PATH makes the wrapper fail; removing `explore` breaks both skill surfaces. `aethyme-testkit/tests/skill_surface_parity.rs` keeps Claude/Codex trees byte-identical and verifies the wrapper copies against the package template; the `cargo-test` gate includes both skill paths. |
| `skills/aethyme/aethyme-load-context.sh` | `.claude/hooks/aethyme-load-context.sh` (executable, wired via `.claude/settings.local.json`) | Reads `AGENTS.md` + `CLAUDE.md` from `$CLAUDE_PROJECT_DIR`; resolves the installed `aethyme` from PATH once; records wrapper telemetry best-effort; then invokes `aethyme repo hook-envelope` over stdin to emit SessionStart JSON. It has no source-checkout fallback. | With no router on PATH the hook exits 0 emitting nothing rather than injecting malformed context. Removing `repo hook-envelope` breaks context injection. Hook end-to-end coverage lives in `enhance_cli.rs`, `skill_templates.rs`, and the no-Python product CI lane. Re-run `aethyme deploy` in already-enrolled repositories after upgrading. |

### Deployment plumbing

| Source | Deploys what | Notes |
|---|---|---|
| Router `aethyme deploy` / `aethyme deploy verify` | Broker configuration and gates, portable onboarding inputs, generated root policy, agent skills, and hook settings | Canonical mandatory repository enrollment. `deploy` composes scaffold, gate drafting, embedded enhance deployment, verification, and certification; `deploy verify` is read-only and intended for required CI. Both run entirely from the installed binary pair. |
| Router `aethyme deploy bridge`, `deploy --local-only`, and `deploy verify --local-only` | A committed inert AGENTS/CLAUDE marker check plus clone-ignored full policy, skills, broker configuration, gates, and runtime state | Staged adoption surface. Inactive clones must not probe PATH, invoke hooks, install Aethyme, or warn. Active clones load `.aethyme/local/AGENTS.md` as mandatory through `.aethyme/local/enabled`; tracked-policy collisions are refused and verification is read-only. Covered by `repository_deploy_cli`. |
| `aethyme-enhance` crate deploy pipeline (`deploy.rs:TARGETS` + `agents.rs` root render; templates embedded at build time — Phase 2 flip, 2026-07-29) | fully generated `AGENTS.md`, `CLAUDE.md`, `.claude/skills/aethyme/SKILL.md`, `.codex/skills/aethyme/SKILL.md`, `.claude/skills/aethyme/references/*.md`, `.codex/skills/aethyme/references/*.md`, `.claude/hooks/aethyme-load-context.sh`, `.claude/settings.local.json` (merge-aware) | Embedded lower-level discoverability pipeline used by `aethyme deploy`; all generated commands resolve `aethyme` from PATH and require no source checkout. Repo-specific customization comes from `.aethyme/overrides/agents.json`; direct edits to `AGENTS.md` / `CLAUDE.md` are unsupported and flagged by `aethyme deploy verify --generated-only`. |
| `aethyme-enhance` crate `skills.rs` (`repo deploy-skills`, native since the Phase 3 flip 2026-07-29; the Python `src/indexing/skills.py` is deleted — its former `eval/repos.py` consumer was already removed with the eval stack) | `.codex/skills/<name>/*` for each runtime skill (embedded templates) | Compatibility path for static runtime-skill deployment. It renders the same PATH-resolved installed commands as canonical deployment; `--remove` also sweeps the legacy `eval` skill directory older tooling may have deployed. |

### Shell scripts in `packages/aethyme/scripts/`

| Script | Invokes | Failure mode |
|---|---|---|
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme deploy --generated-only and $AETHYME_ROOT/rust/target/release/aethyme deploy verify --generated-only` (native since the Phase 2 flip, 2026-07-29) | Setup fails if the router binary is missing (preflight-checked) or the generated-only deploy/verify commands are renamed/removed. |
| `scripts/eval/setup-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-graph-index` followed by `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli index --repo .` | Fresh Playground setup fails if either binary path changes, if fragments are not produced before engine indexing, or if `graph_store.redb` stops being materialized from fragments. |
| `scripts/eval/setup-playground.sh` | Writes local `.git/info/exclude` rules for `.aethyme/`, `.chau7/`, `.claude/`, `.codex/`, `AGENTS.md`, and `CLAUDE.md`; marks tracked generated artifacts `skip-worktree` if the source repo was already enhanced. | Playground agents can treat generated scaffolding as benchmark source if these local excludes drift or are omitted. |
| `scripts/eval/setup-playground.sh` | Asserts `<target>/.aethyme/graph/` and `<target>/.aethyme/graph_store.redb` exist after graph-index + engine-index. | False failure if the fragment layout or redb graph-store path changes. |
| `scripts/eval/verify-playground.sh` | `$AETHYME_ROOT/rust/target/release/aethyme-engine-cli`; checks deployed root guidance, skill, `.aethyme/graph/`, `.aethyme/graph_store.redb`, and local generated-artifact excludes. | False failure if binary path, fragment layout, redb graph-store path, generated root wording, or ignore contract changes. |
| `scripts/eval/verify-playground.sh` | Greps deployed root files, short SKILL.md, and references for command names (native `aethyme explore`, `aethyme explore-summary --from`, `analyze dead-code`, `facts function-usage`) and rejects ANY executable `python -m src.cli` guidance (Phase 2 template flip 2026-07-30: deployed commands spell `aethyme ...`; tolerated mentions are negative-guidance lines matching 'Do not run\|not a valid command\|was removed\|retired'). Since the Phase 5.5 projection flip (2026-08-01) `check_no_venv_python` also rejects any `.venv/bin/python` invocation in deployed guidance, wrappers, and hooks — comment/provenance lines tolerated. | False positive/negative on health check if templates change wording but verify-playground doesn't update its grep patterns. **Updated 2026-07-30 in lockstep with the template spelling flip and 2026-08-01 with the Phase 5.5 projection flip (Class 3 protocol); assertions mirrored in `aethyme-cli/tests/playground_hygiene.rs`.** |
| ~~`scripts/docs/generate-docs.sh`~~ (REMOVED 2026-08-01, retirement Phase 6) | Dead generator. Its OpenAPI block read `src/api/main.py` (deleted with the Gen-0 PostgreSQL lineage 2026-07-13), its CLI block iterated `src/cli.py`'s Click tree (empty since Phase 5, deleted in Phase 6), and every file it claimed to write — `docs/openapi.json`, `docs/reference/cli-generated.md`, `docs/reference/metrics.md`, `docs/INDEX.md` — is absent from the tree, so nothing consumed its output. It was also one of the last `python3` invocations inside `packages/aethyme`. | No replacement: `docs/reference/cli.md` is hand-maintained and `aethyme --help` is the live surface. Anyone running the script got two skipped sections and a metrics doc describing a Prometheus endpoint that no longer exists. |
| ~~`scripts/eval/run_codex_eval.py`, `scripts/eval/check_regression_gate.py`~~ (MOVED 2026-08-06, retirement Phase 7) | Both moved to `packages/aethyme-eval/scripts/` — eval tooling, in the package that owns eval tooling and stays Python by operator decision, because an arm's-length acceptance check should not share the measured system's toolchain. `packages/aethyme` is 100% Rust now, so staying was not an option either. Their contract test moved with them (`packages/aethyme-eval/tests/test_codex_eval_runner_contract.py`, 41 cases). The move touched path arithmetic that encodes **cardinal rule 1**: both files derived `package_root`/`monorepo_root` by counting `parents[2]`; they now name `TOOL_PACKAGE_ROOT` (the measured `packages/aethyme`, still the default `AETHYME_EVAL_TOOL_REPO`) and `MONOREPO_ROOT` (an eval target inside it is Aethyme itself and is refused) explicitly. | Out-of-repo automation invoking `packages/aethyme/scripts/eval/run_codex_eval.py` gets "No such file"; the new paths are `packages/aethyme-eval/scripts/{run_codex_eval,check_regression_gate}.py`, run with **aethyme-eval's** interpreter (`packages/aethyme/.venv` no longer exists). If the two root constants are ever miscomputed, the eval could silently run against Aethyme itself — the 41-case contract suite covers exactly this, passing `packages/aethyme` as a repo path the contract must refuse. Docs updated in the same commit: `docs/guides/playground-setup.md`, `docs/guides/eval-protocol.md`, `docs/reference/cli.md`, `packages/aethyme-eval/README.md`. |
| ~~`scripts/verify-grammar-provenance.py`~~ (PORTED 2026-08-06, retirement Phase 7) | The tree-sitter grammar provenance verifier now lives at `rust/crates/aethyme-testkit/tests/grammar_provenance.rs` — it audits the Rust crates' own grammar assets, so it is `packages/aethyme`'s concern and ported rather than moving to aethyme-eval. Same manifest shape, file-presence, MIT-license and sha256 checks, same error strings, same two modes. **No test ever invoked the script**; both modes were manual commands, so making the development mode a `cargo test` case is a strict gain. Release mode (`--require-pinned`) is `#[ignore]`d because it is documented as expected to fail until every grammar records a pinned upstream ref. | Anyone running `python packages/aethyme/scripts/verify-grammar-provenance.py` gets "No such file". Replacements: `cargo test -p aethyme-testkit --test grammar_provenance` (development) and `... -- --ignored` (release). Callers updated in the same commit: `docs/third-party-license-audit.md`, `rust/grammars/README.md`. |
| `scripts/migrate.sh` (REMOVED 2026-07-13) | Deleted with migrations/, alembic, and the Postgres deps in the final cloud-lineage sweep. | — |
| `scripts/start-api.sh` (REMOVED 2026-07-13) | Deleted with the Gen-0 PostgreSQL lineage (`src/api`, `src/graph`, tenant CLI commands `index/stats/ego/impact/search`). Any external caller of those entry points was already dead or must migrate to the engine CLI (`query`/`graph` commands). | — |

### Retired Python surfaces (kept as removal records)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`tests/` (pytest), `pyproject.toml`, the `ruff` and `pytest-local` gates~~ (REMOVED 2026-08-06, retirement Phase 7) | **The dev test stack is deleted; `packages/aethyme` is 100% Rust.** All 145 pytest cases were accounted for before deletion: 41 MOVED with the eval scripts to `packages/aethyme-eval/tests/test_codex_eval_runner_contract.py`; 102 PORTED to `rust/crates/aethyme-cli/tests/` (`enhance_cli.rs` 15, `autofix_cli.rs` 15, `skill_templates.rs` 13, `explore_summary_cli.rs` 11, `ai_ready_cli.rs` 9, `skills_cli.rs` 6, `playground_hygiene.rs` 5, `commit_hygiene_cli.rs` 5, `local_workflow.rs` 4, `intents_cli.rs` 1) and `rust/crates/aethyme-testkit/tests/` (`docs_hygiene.rs` 13, `pr_template.rs` 5); 2 NOT ported — `test_require_local_engine_or_skip_{enforces_when_requested,skips_by_default}` tested the pytest harness's own skip helper, and the Rust harness has no skip path (a failed binary build asserts), so strict mode is unconditional and there is no helper left to test. One assertion changed rather than moved: `test_python_examples_syntax` compiled `python` fences with CPython, which no interpreter remains to do; it became "a Python-free package must carry no Python fences" (`docs_hygiene.rs::no_unverifiable_python_examples`). `tests/support/` is superseded by the `aethyme-testkit` crate (`bins`/`invoke`/`repos`/`paths`), which is `publish = false` and dev-dependency-only, so it can never enter `cargo install`. `pyproject.toml` held only pytest+ruff config and nothing in the product path read it. | `pytest packages/aethyme` and `ruff check packages/aethyme` now find nothing to do — replacement is `cargo test --workspace --manifest-path packages/aethyme/rust/Cargo.toml`. `packages/aethyme/.venv` is no longer created by CI or assumed by any gate; delete any local copy. The `pytest-local` gate's trigger set did not vanish with it — `cargo-test` gained `packages/aethyme/{skills,docs,rust/grammars}/**`, `scripts/eval/*.sh` and `.github/pull_request_template.md` so a markdown-only entry still runs the suites that assert over those files (the same silent-narrowing failure the retired gate's own comment recorded). |
| ~~`scripts/migration/`~~ (REMOVED 2026-08-01, retirement Phase 6) | The migration parity harness: `golden-diff.sh` (frozen CLI stdout), `enhance-golden.sh` (deployed artifact bytes), `golden-commands.txt`, `normalize-output.py`, plus the `quality-probe` and `fix-probe` dev bins in the aethyme-quality crate that the Phase 4/5 harnesses drove. All of it was capture-before-flip / compare-after-flip tooling for a migration with two sides; there is no Python side left to compare against, every command in `golden-commands.txt` is native, and no golden corpus was ever committed (goldens were captured to scratch dirs per phase). `golden-diff.sh` also hard-required `$PACKAGE_ROOT/.venv/bin/python`, and `normalize-output.py` was one of the last Python files in `packages/aethyme`. | Nothing regresses unguarded. CLI stdout shapes are held by Rust unit tests per command module plus the implementation-blind subprocess suites in `rust/crates/aethyme-cli/tests` (`ai_ready_cli.rs`, `autofix_cli.rs`, `explore_summary_cli.rs`, `local_workflow.rs`). Deployed artifact bytes — what `enhance-golden.sh` guarded — are held by `aethyme deploy verify` canonical-match checks, `aethyme-cli/tests/enhance_cli.rs`, and `scripts/eval/verify-playground.sh`. The probe bins had no consumer outside the deleted harness; removing them also shrinks what `cargo install` puts on PATH. |
| ~~`src/`~~ (REMOVED 2026-08-01, retirement Phase 6) | **The entire Python package is deleted.** Audited module by module before removal: `src/models/` — zero consumers anywhere, and its `__init__.py` already imported a `.graph` module that did not exist (a husk that would have failed on first import). `src/cli.py` — a Click group carrying zero commands since the Phase 5 flip; its only consumers were the retired migration/doc tooling and one in-process test of its own explore tombstone. `src/contracts/` (`versions.py`, `graph_export.py`, `run_metadata.py`, `eval_artifacts.py`) and `src/indexing/repository_snapshot.py` — a closed loop: consumed only by `tests/contracts/`, which existed only to test them. Nothing wrote the version stamps they defined (the engine stamps its own `aethyme-explore-v1` / `aethyme-verify-targets-v1`), `eval_artifacts.py` had zero references of any kind, and **`packages/aethyme-eval` imports nothing from `src/`** — so the cross-package dependency the plan anticipated does not exist and nothing needed moving into aethyme-eval (operator decision 6, 2026-08-01: aethyme-eval stays Python and owns its own inputs). The plan's Improvement #2 duplication is resolved by subtraction: with the Python definitions gone the Rust side is the only definition, and the sync test dies with the duplication. `src/indexing/engine.py` — a 72-line build-if-stale helper used only by `tests/local/test_local_workflow.py`; moved verbatim to `tests/support/engine_binary.py`, where it is labelled as dying with the tests/local Rust port rather than left behind to keep the `src.` import namespace alive. Also swept in the same commit: `requirements-dev.txt`, and the untracked `src/scorecard/`, `src/rendering/`, `tests/scorecard/` `__pycache__` husks that made deleted modules superficially importable (the plan's long-tail-artifact risk, seen live in the 2026-07-17 audit). | `python -m src.cli <anything>` now fails with `No module named src` — a **hard break**, announced in README and AGENTS.md rather than shimmed (plan risk item: announce, don't assume). Every command has a native equivalent: `aethyme <command>`. Anything importing `src.*` fails at import; the 2026-08-01 audit found zero importers outside this package's own tests. The output cache under `AETHYME_CACHE_DIR` / `/tmp/aethyme-cache` that `engine.py:CACHE_ROOT` documented has no writer any more; `aethyme repo clear-cache` is native and clears the engine's own caches. |
| ~~`python -m src.cli ai-ready`, `src/scorecard/*`~~ (REMOVED 2026-07-30, retirement Phase 4) | The Python `ai-ready` command, the scorecard package (models/engine/formatters/detectors), and `tests/scorecard/` were hard-deleted after the router flipped `ai-ready` to the native `aethyme-quality` crate (byte-parity verified on the Phase 4 corpus). Entry points at deletion: the router's Python delegation (now dispatches natively), `tests/scorecard/` (replaced by Rust unit tests + `aethyme-cli/tests/ai_ready_cli.rs` implementation-blind subprocess tests), and `scripts/docs/generate-docs.sh`'s generic Click-tree iteration (unaffected — it lists whatever commands remain). `record_scan_metrics` (`src/scorecard/metrics.py`) was an in-process Prometheus emitter that has been a silent no-op since the Phase 0 `prometheus-client` dependency purge; it wrote nothing on disk and had no reader, so it retires with no native replacement. `pydantic` left `pyproject.toml` in the same commit (scorecard was its last importer). | Out-of-repo scripts invoking `python -m src.cli ai-ready` now get Click's unknown-command error; migrate to `aethyme ai-ready` (identical flags). **Byte-parity with the retired Python output no longer holds (#343, 2026-09-25):** #343 retired the CPython-compatibility JSON layer in `aethyme-quality`; the JSON keeps its keys, key order and shape, but non-ASCII text is now emitted as UTF-8 rather than `\uXXXX` escapes. Parse it as JSON; do not byte-compare it with archived Python output. Anything importing `src.scorecard` fails at import; the 2026-07-30 audit found zero such importers outside the deleted command (autofix never imported it). |
| ~~`python -m src.cli autofix`, `src/autofixers/*`~~ (REMOVED 2026-08-01, retirement Phase 5) | The Python `autofix` command, the autofixers package (safety, patch, github, `_log`, and the 5 fixers), and `tests/autofixers/` were hard-deleted after the router flipped `autofix` to the native `aethyme-quality` crate (fix side; stdout, exit codes, produced unified diffs, and post-apply trees byte-parity verified on the Phase 5 corpus). Entry points at deletion: the router's Python delegation (now dispatches natively); `tests/autofixers/` (replaced by Rust unit tests plus `aethyme-cli/tests/autofix_cli.rs` implementation-blind subprocess tests); `scripts/migration/dump-cli-surface.py`'s Click-tree iteration (unaffected — the Commands table is simply empty now); and `scripts/docs/generate-docs.sh`'s generic Click-tree iteration (same). `src/cli.py:normalize_fixes`/`FixRecord` were autofix-only glue and went with it. **This was the last Python command: the Click tree now carries zero commands and the router delegates nothing.** `src/cli.py`, `src/contracts/`, `src/indexing/` (a 72-line engine build bootstrap for the dev test harness), and `src/models/` are all that remain of `src/`; they retire in Phase 6. | Out-of-repo scripts invoking `python -m src.cli autofix` now get Click's unknown-command error; migrate to `aethyme autofix` (identical argument, flags, exit codes and on-disk effects). **Stdout is no longer byte-identical to the retired Python output (#343, 2026-09-25):** JSON keeps its keys, order and shape but emits non-ASCII as UTF-8, and the printed diffs are standard unified diffs. Apply them with any unified-diff tool; do not byte-compare them with archived Python output. Anything importing `src.autofixers` fails at import; the 2026-08-01 audit found zero such importers outside the deleted command. The broker's lockfile ignore list (`aethyme-broker/src/leases.rs`) mirrored `safety.py:LOCK_FILES` by comment only and now points at `aethyme_quality::fix::safety::LOCK_FILES`. |
| ~~`evals/tools/*.toml`, `src/eval/orchestrator.py`, `tests/local/test_eval_warm_phase.py`~~ (REMOVED 2026-07-13) | The eval harness, its tool manifests, and the eval warm-phase contract were removed with the evaluation stack (knowledge preserved in `docs/architecture/eval-mining-notes.md`; sources at git ref `16cfa5e`). The **engine daemon commands themselves** (`aethyme-engine-cli daemon status/start`, `engine-daemon.log`, the `listening on` marker) remain live engine surface — they just no longer have an in-repo eval consumer. | Out-of-repo eval scripts that invoked `aethyme eval ...` or `aethyme methodology ...` now fail with unknown command; rebuild from the mining notes if needed. |

### Single `aethyme` entrypoint (#31, 2026-07-14)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| `pyproject.toml [project.scripts]` (REMOVED 2026-07-14) | The pip console script `aethyme = "src.cli:main"` was removed — `pip install -e .` no longer installs an `aethyme` executable that shadows the Rust router. `python -m src.cli <cmd>` is the only Python-side invocation. | Out-of-repo scripts calling the *pip* `aethyme` from inside an activated venv now resolve to the Rust router (PATH) or nothing; migrate them to `python -m src.cli`. In-repo audit 2026-07-14 found zero such callers. |
| Rust router `aethyme` (installed with the paired release archive or Homebrew; Cargo remains a contributor fallback) | **Every command is native; there is no delegation path.** Repository deployment and versioned `upgrade plan/apply` migrations use templates and migration logic embedded in the installed router, so they never resolve an Aethyme source root. Canonical deployment records a tracked repository schema; local-only deployment records an ignored schema. `root show/set` remains a legacy developer-compatibility surface, not an installation prerequisite. Explore auto-start spawns the sibling `aethyme-engine-cli` from the same install directory. | If the paired engine binary is missing or version-skewed, explore auto-start fails its sibling check. Broker use fails closed when an enrolled repository schema is missing, incomplete, or newer than the router. If the installed router is absent from PATH, deployed repository wrappers and hooks cannot invoke the product; repository policy itself remains clone-portable. |
| `aethyme broker advanced resources run`, `aethyme broker advanced console run`, and readers/writers of the host-resource registry (supervised lifecycle hold, 2026-10-18; phase timings, 2026-10-29, #1226) | Request and report JSON stay at schema 1; writable opens atomically migrate the separate SQLite store from v1 to v2. Current read-only clients can still read v1 stores; older binaries fail closed when opening a v2 store. A run persists a generation-bound capacity hold before environment preparation or spawn, uses one-shot request IDs, and clears the hold only after its original and cleanup process groups are observed absent with successful cleanup, or after exact-generation operator reconciliation. Old v1 connections opened before migration remain fenced by SQLite triggers against releasing/reaping held allocations or overbooking held capacity; unused pool headroom remains available. Both run commands add `phase_timings_ms` to JSON reports and `phases_ms` to human output. The map contains wall-clock milliseconds for `resource_acquisition` (including lease wait), `environment_preparation`, `child_spawn`, `child_execution` (including supervision and lease renewal), optional `cleanup_execution`, and `lease_finalization`. Missing phases were not reached or were skipped. `.aethyme/logs/command-metrics.jsonl` records the growing phase map as phases finish, preserving partial timings if a later phase fails; its `duration_ms` remains end-to-end and need not equal their sum. | Consumers must tolerate the additive `phase_timings_ms` field and missing phases. A consumer that uses only the lease-holder PID to reclaim supervised capacity can start a second writer while the child group survives. A client must handle the store-version fence and retained hold by inspecting processes, cleaning exact owned resources, then reconciling the recorded generation. |
| Waiters on `aethyme broker advanced resources acquire` and `run`, gates with `resource_wait_seconds`, readers of `resources explain --json`, contention JSON, and command metrics, and every binary sharing the host-resource store (fair resource queue, 2026-11-13) | Writable opens add a `resource_waiters` table with `CREATE TABLE IF NOT EXISTS`; the host store version stays 2, so older binaries keep opening and validating the store. A waiting request records one row on first ordinary contention and is served in ticket (arrival) order per capacity pool and exclusive key. A waiter reserves only resources that were contended for it, keeps them for the rest of its wait, and reserves nothing while a conflict is on a TCP port. A request is granted only if its pool has room for the units earlier live waiters reserved plus its own (against the smallest declared limit), and never takes a key an earlier waiter reserved. A request that does not wait is ordered after every live waiter. Rows are removed on grant, non-waitable refusal and timeout; rows whose process is gone, whose deadline passed or whose heartbeat is over two minutes old are ignored at once and deleted by the next acquirer. `explain --json` adds `queue` (schema `aethyme.host-resource-queue/v1`: `queued`, `position`, `enqueued_at`, `waited_ms`, `times_skipped`, `resources[]` with `kind`, `resource`, `waiters_ahead`, `waiters_behind`, `units_ahead`, `units_requested`, `units_allocated`, `limit`, `holders[]`). `acquire --json` and `run --json` contention output add the same `queue` object; `run` waiting events append the position. `list --json` is unchanged; human `list` prints waiters. `.aethyme/logs/command-metrics.jsonl` adds `resource_queue` (`waited_ms`, `attempts`, `poll_ticks`, `queued`, `position_at_enqueue`, `times_skipped`, `reaped_dead_waiters`). | Consumers must tolerate the additive `queue` and `resource_queue` fields. Preparation-cache root initialization and local execution admission now wait up to five seconds in the queue instead of making one attempt. A non-waiting acquire that used to succeed beside a waiter can now fail with retryable `resource_contention`. Older binaries neither read nor write waiter rows, so their polling acquire can still take units an earlier waiter reserved; fairness is complete only once every running process uses a current binary. |
| Readers of retained resource reap diagnostics (introduced 2026-10-21, #765) | `aethyme broker gc reap --json` preserves existing report fields and adds `not_reaped[]` entries with `lease_id` and typed `reason: "not_proven_gone"` when a quarantined candidate’s holder cannot be proven gone. Human output reports the same lease ID and reason. This is diagnostic only: the lease remains quarantined and its allocations stay held; no PID or host path is included. | Consumers must inspect `not_reaped[]` to explain retained candidates. `not_proven_gone` means the process check was inconclusive; it proves neither that the holder is live nor that it is gone. Legacy Rust report and method shapes stay unchanged; detailed callers can request the additive report. |
| Readers of `aethyme broker gc reap --dry-run --json` (introduced 2026-10-24, #765) | The read-only preview has schema `aethyme.host-resource-reap-preview/v1`, `evaluated_at_ms`, `would_reclaim_capacity_units`, `candidates[]` with lease id, generation, holder PID, capacity units and projected state, and `not_reaped[]` with existing conservative diagnostics. Missing state returns empty without creating the registry; a retained host-resource schema-1 registry is refused without migration. Known managed-storage floors are read in place; schema-3 producer-linked capacity stays fenced. | A preview is observational and never grants removal or signalling authority. Actual reap rechecks live state. Named allocations stay quarantined, supervised holds and unresolved preparation stay fenced, and no process or container is stopped. Existing reap output is unchanged. |
| Authors of host-resource request JSON and `[[gate.resources]]` tables, waiters on `resources acquire`/`run`, and readers of contention reasons (introduced 2026-11-16) | A `capacity` resource accepts an optional `pressure` field whose only value is `"memory"`. With it, planning reads host memory pressure once per attempt and admits `limit` units at normal, half of `limit` rounded up at warning and 1 unit at critical; a request into a pool with no holder is always admitted. The field is omitted from serialized requests when absent, so digests of existing requests are unchanged; request and report schemas stay version 1 and the store schema is unchanged. A request kept out by pressure is `resource_contention` (waitable) with a reason beginning `host memory pressure is <normal\|warning\|critical>`. | A binary older than the field ignores it and applies the declared limit. A consumer matching contention reasons by exact text sees a new reason for pools that opt in. Holders already running are never affected. |
| Callers of `packages/aethyme/scripts/test-like-ci.sh` in a linked worktree, and of the new `packages/aethyme/scripts/rust-target-seed.sh restore\|publish` (introduced 2026-11-16) | A local (non-`--full`, non-archive) run with no `rust/target` first clones one from `<worktrees dir>/.rust-target-seed/<key>/target`, where `<key>` is 16 hex digits over `Cargo.lock`, `rustc -Vv` and `uname -sm`; a passing local run publishes that seed when none exists and keeps the two newest. Both steps are best-effort, print one `rust target seed:` line on stderr, never change the exit status, and do nothing in a main checkout. `AETHYME_RUST_TARGET_SEED_DIR` moves the store and `off` disables both. Each worktree still owns its own `target/`. |
| Windows x64 `aethyme.exe` and `aethyme-engine-cli.exe` release (introduced 2026-10-08, #357 Phase 1) | Native `aethyme.exe explore`, graph navigation over committed `.aethyme/graph` fragments and the local redb store, and `deploy --generated-only`. Broker and broker-backed graph lifecycle commands exit nonzero with an unsupported-platform message. `.github/workflows/windows-release.yml` runs `scripts/windows-release-smoke.ps1` against a temporary repository to verify supported routes and refusal behavior. The release publishes `aethyme-v<version>-x86_64-pc-windows-msvc.zip` with `.sha256`, a `SHA256SUMS` line and `<zip>.sigstore.json`, but **not** in `release-manifest.json`: `supported_platforms` and `artifacts` keep the five tarball targets, because released updaters up to 0.8.24 refuse a manifest entry that is not `aethyme-v<version>-<target>.tar.gz`. | Consumers may rely on this documented Phase 1 surface on Windows x64; commands outside it require WSL2 until a later support phase is approved. The release ZIP contains the paired native binaries. |
| Router reader commands `aethyme explore-summary --from <file>` and `aethyme verify-targets --from <file>` (native router surface; `explore-summary` added in python-retirement Phase 5.5, 2026-08-01, `aethyme_enhance::explore_summary_cli`) | Both read the SAME saved `explore --format answer-json` temp file, so the deployed quick start makes one explore call and feeds two readers. `explore-summary` prints the compact decision surface (`safe_to_use_as_answer`, `trust_policy`, `subsystems`, `top_verification_targets`, `verification_steps`, `observability.readiness`) byte-identically to the retired `.venv/bin/python` heredoc — no `schema_version` key by contract decision. Usage errors exit 2, failures exit 1; `--from -` reads stdin. Consumed by `skills/aethyme/SKILL.md`, `skills/aethyme/AGENTS.md`, and `skills/aethyme/references/explore.md`. No frozen-surface doc covers it: `cli-surface-v1.md` froze the *delegated Click* surface and retired with it in Phase 6, so this row plus the tests below are the whole contract. | Renaming/removing either command silently breaks the deployed quick start in every enhanced repo (Class-3: fails only when an agent runs it). Field/order changes in `explore-summary` break the skill's "inspect only these fields" contract. Covered by `aethyme-cli/tests/explore_summary_cli.rs` (implementation-blind, drives the built binary), `aethyme_enhance::explore_summary_cli` unit tests, and `verify-playground.sh` template greps. |
| Agents and tools reading `aethyme explore` output from a stale checkout (introduced 2026-10-06, #232) | When the answered checkout's `HEAD` is behind a fresher local ref (`aethyme/integration`, else the fetched `origin/HEAD`, `origin/main` or `origin/master`; no fetch), explore prints one `warning: this checkout is N commit(s) behind <ref>; …` line on stderr, adds an optional top-level `source_staleness` object (`head`, `reference`, `behind`, `linked_worktree`, `suggestion`) to answer-json, sets `safe_to_use_as_answer` and `trust_policy.safe_to_use_as_answer` to `false`, and the brief's second line starts `Stale checkout:`. A current checkout, one with no reference ref, or a non-Git directory gets byte-identical output. | An answer computed from a tree behind integration reports already-fixed code as missing (four wrong findings on 2026-09-17, a false graph alarm on 2026-09-27). Dropping the field or keeping `safe_to_use_as_answer` true lets that answer be acted on; making the key required would break consumers of current-checkout output. |
| `aethyme explore --format brief` (introduced 2026-09-25, recovery plan P4.6; shared front end `aethyme_engine::explore_cli` + `aethyme_engine::explore_brief`, so the router and `aethyme-engine-cli` print the same text) | One call that folds `explore` + `explore-summary` + `verify-targets`: renders the SAME answer document `--format answer-json` prints as text — a first line that always reads `Explore — a navigation aid, not an answer; verify the spans below before relying on them.` followed by `trust=`, `safe_to_use_as_answer=`, `status=` and `readiness=`; a `Why:` line from `trust_policy.reason`; up to 3 `Subsystems:` lanes; up to 2 `Verify:` steps; then the top 2 verified spans (`── <rank>. <path>:<start>-<end> (<status>; matched: ...)` and numbered source lines), each capped at 80 lines (a per-target budget, where `verify-targets --max-lines` is shared across targets). `--format brief` implies `--show-observability` and, when neither `--depth` nor `--detail` is given, `--depth 0`. `--repo` now defaults to the current directory for every format (additive: it was a required-option usage error). `answer-json` stays the default format and the machine surface; `explore-summary` and `verify-targets` are unchanged. Consumed by the generated root guidance (`skills/aethyme/AGENTS.md`), `skills/aethyme/SKILL.md` and `references/explore.md`. The brief is text for an agent to read, not a parsed contract, and carries no `schema_version`. | Removing `brief` breaks the one Explore line in every enhanced repository's generated guidance (Class-3: fails only when an agent runs it). Weakening the first line's verification warning would let an agent treat navigation hints as answers. Covered by `aethyme_engine::explore_brief` unit tests (warning line, two spans, per-target cap, empty answer), `aethyme-cli/tests/explore_brief_cli.rs` (built router; brief text, default `--repo`, answer-json still the default, unknown format exits 2), and the `explore` help snapshot. |
| Graph lifecycle and Explore performance JSON (introduced 2026-09-04) | `aethyme graph status --json`, `graph refresh plan/execute --json`, and `graph materialize --json` expose typed phase observations, bytes, counts, and peak RSS; `aethyme explore --show-observability` exposes process/query timings, store bytes, and peak RSS. `scripts/bench-graph-lifecycle.sh` consumes these fields in disposable non-Aethyme Playgrounds. Runtime evidence is intentionally excluded from refresh plan authorization digests and deterministic semantic-output comparisons. | Renaming phase or count fields breaks release-mode performance collection; including variable observations in authorization digests makes reviewed plans non-repeatable. The contract is covered by graph-storage/indexer unit tests, `graph_refresh_cli`, the Explore observability tests, and the harness self-target refusal. Field semantics and benchmark methodology live in `docs/guides/graph-performance.md`. |
| Graph-free Explore source search (introduced 2026-09-24, recovery plan P3.2/P3.3) | When the optional graph store is missing or unusable, `explore --format answer-json` keeps its `aethyme-explore-v1` shape (`status: degraded`, `degraded_reasons: [graph_store_<status>]`) but fills `navigation_hints` and the `bounded_source_fallback` subsystem's `top_verification_targets` with up to 8 ranked files from a wall-time-bounded (2 s default) full-content search of tracked plus untracked-not-ignored files. Each hint's `evidence` carries `line_refs[]` (`line`, optional `end_line`, `kind` = `definition`/`match`/`definition_line`, optional `symbol`), `matched_terms`, `term_coverage`, `path_role`, `symbol_match`, and `score`. **Output profiles (introduced 2026-09-24):** the default agent call (`--detail compact`, no `--show-observability`) omits every hint `evidence` key except `line_refs` (and `answer_rule` on a promoted hit) and every `observability` key except `readiness`; `--show-observability` or `--detail standard` restore the hint diagnostics, `--show-observability` also restores `source_fallback` and `graph_store`, and `source_fallback.scoring` (the BM25F constants) appears only with `--detail full`. Ranking, hint order, spans, `subsystems`, `trust_policy` and the top-level keys are the same in every profile, and `output_chars_estimate` is the exact byte length of the printed JSON (`aethyme_engine::explore::project_graph_free_output`). Readers of `observability.source_fallback` (the eval harness `packages/aethyme-eval/heldout/run.py`) must pass `--show-observability`, which they already do. **Answer safety (P3.4):** promotion is currently disabled, so graph-free `safe_to_use_as_answer` is always `false` and `answer[]` stays empty; `observability.source_fallback.answer_safety` still reports the rule verdict for measurement. When enabled, `safe_to_use_as_answer` is `true` only when the search was complete (every listed file read within the budget) and the top hit passes one rule: `exact_symbol_definition` (it defines a symbol whose name the request spelled out, as an identifier or as adjacent words, and no other ranked hit defines the same name) or `dominant_term_coverage` (its `term_coverage` is at least 0.8 and its score is at least 1.5x hit 2's, or it is the only hit). That hit then moves from `navigation_hints` to the single `answer[]` item with `status: content_evidence` and `evidence.answer_rule`; `trust_policy` becomes `answer_candidate` with `evidence_level: source_navigation`, `degraded: true`, and a reason stating it is not caller or impact evidence. Otherwise `answer[]` is empty, `trust_policy` is `verify_before_use`, and its reason names the blocking rule (`search_incomplete`, `no_hits`, `ambiguous_symbol_definition`, `low_term_coverage`, `no_clear_margin`). `observability.source_fallback.answer_safety` is `{safe, rule}`. `observability.readiness` is `{status: ready, reason: source_search_complete}` or `{status: partial, reason: <why>}`; `observability.source_fallback` reports `mode`, `listing` (`git`/`filesystem`), `listed_files`, `scanned_files`, `skipped_files`, `max_bytes_per_file`, `budget_ms`, `elapsed_ms`, `complete`, `reason`, `max_hits`, `terms`, and `symbol_index` (`cached_files`, `cache_hits`, `parsed_files`, `persisted`). The on-demand definitions index is cached under the host cache directory (`$AETHYME_HOST_CACHE_DIR`, `$XDG_CACHE_HOME/aethyme`, or `~/Library/Caches/Aethyme`) at `symbol-index/v1/`, never in the repository. The retired keys `source_fallback.tracked_only` and `source_fallback.max_files` are gone. | `verify-targets` anchors on the first `line_refs[].line`, so reordering line refs changes which span agents read first; it now inherits line refs from a hint when the same target first appears as a subsystem target without evidence. `explore-summary` prints `observability.readiness` verbatim, so new status values reach agents unfiltered. Readers that rank `answer[]` before `navigation_hints[]` see the same order as before P3.4; readers that read only `navigation_hints[]` miss the answer-safe top hit, which also stays first in the subsystem's `top_verification_targets`. Covered by `aethyme-engine` source-search unit tests, `aethyme-engine/tests/source_search.rs` (2,000-file budget, cold/warm cache, verify-targets spans), `aethyme-cli/tests/explore_summary_cli.rs::graph_free_explore_output_feeds_both_readers` and `::default_compact_graph_free_explore_still_feeds_both_readers`, and the output profiles by the `source_fallback` unit test `output_profiles_trim_diagnostics_but_never_the_ranking`; the answer-safety rule by the `source_fallback` unit tests on synthetic rankings (strong, ambiguous, weak, incomplete). |
| Graph materialization cache JSON and host artifact (introduced 2026-09-04) | `aethyme graph materialize --json` and refresh execute reports expose `cache.status`, `key_sha256`, and `artifact_bytes`. The host entry is keyed by source tree, fragment-manifest digest, engine/protocol version, and storage schema; its redb and metadata are immutable inputs copied into worktree-private staging before validation and rebinding. | Consumers may distinguish `not_needed`, `unavailable`, `hit`, `miss_stored`, and `miss_unstored`; cache paths are deliberately absent. Do not open a cached redb writable or hard-link it into a worktree. Corruption is a miss, committed fragments stay authoritative, and runtime cache status remains outside the reviewed refresh-plan digest. Covered by graph-storage cache tests and the cross-clone materialization integration test. |
| Generated broker protocol (`aethyme-enhance` crate `agents.rs:render_broker_protocol`, deployed into repo-root `AGENTS.md`/`CLAUDE.md` of broker-configured repos; same convention hand-condensed in this repo's root `CLAUDE.md`/`AGENTS.md`) | **Since 2026-09-25 (recovery plan P4.5) the root file carries only the four-command loop (`broker start --task`, then `broker submit --session <id>` and `broker finish --session <id>`, with remote writes through `broker advanced git`/`gh`); the full protocol is `references/broker.md`, spelled the #344 way.** Bare `aethyme` from PATH (`broker status`, `start` / `start --adopt` / `start --reuse`, `advanced leases claim|release`, `advanced exec`, `submit`, `finish` / `finish cleanup`, `advanced git`, `advanced gh`, `advanced ship`, `advanced operations reconcile`) plus gate-runner worker vars (`AETHYME_GATE_WORKER_ID`, `AETHYME_TEST_DB_SUFFIX`). Since 2026-07-17 the repo-local `rust/target/release/aethyme` fallback path is no longer rendered. v0.2.0 publishes the router and `aethyme-engine-cli` sibling in one archive and installs both through the stable-channel `install.sh`; source builds still require both Cargo packages. Promotion-overlap advisories are delivered through post-commit, common broker commands, and the pre-expensive-gate boundary; `broker status --json` is the inspection surface and the gitignored `.aethyme/broker-advisory.md` is persistence only. Acknowledgment, session close, and rebase do not resolve queue-entry exposure; only verified publication containment or confirmed external reconciliation does. | Agents in enhanced repos run broker commands that fail with "command not found" if the router binary is renamed or the paired-install story breaks. Agents and gate configs can also fall back to shared files/databases if the lease, guarded-exec, worker-suffix, or advisory-delivery guidance drifts. Advisories must remain non-blocking and must never expand selected gates. Re-run `aethyme deploy --generated-only --repo . --force` after protocol wording changes; parity assertions live in `aethyme-enhance`, lifecycle coverage lives in broker merge/ship tests, and implementation-blind text assertions live in `aethyme-cli/tests/enhance_cli.rs`. |
| ~~`aethyme broker advanced ship plan/execute`~~ (REMOVED 2026-11-12; introduced 2026-10-07, #369) | Hard-deleted with its `--only`, `--delivery`, `--plan`, `--sync-main` and `--break-glass` flags, the pull-request and local-main delivery routes, and the plan digest. The owner decided on 2026-11-11 that pull requests are the only landing path, and the command had zero invocations in seven days on the busiest repository. The `[delivery] default` key is still parsed and ignored, so existing configurations keep loading. The in-process `Broker::ship_plan` / `Broker::ship_execute_with_sync` API and its `ShipPlan`, `ShipExecutionReport` and related types were deleted with the promote engine (see "Local promotion removed" below); `aethyme deploy` publishes a first enrollment without promotion. | `aethyme broker advanced ship ...` now fails as an unknown command. Out-of-repo scripts that called it must land through `aethyme broker push --session <id> --pr`. |
| Broker wait and finish diagnostics (introduced 2026-10-08, #494) | `broker status` reports pending lease-release requests and unresolved coordinated operations; `broker leases wait <path> --session <id> [--timeout <seconds>]` emits periodic stderr progress naming the lease holder and elapsed time; `broker finish --session <id> [--timeout <1..86400 seconds>]` applies one deadline (default 60 seconds; 10 seconds until 2026-11-11) to the Git checks that decide whether to close, and reports the timed-out check with safe status and handoff guidance. Opening the broker (including its host-wide autonomous sweep) runs before the deadline starts, and worktree removal after the session closes runs without it, so a large worktree is never killed halfway through deletion. Lock-wait progress is best-effort: a failed holder lookup prints `holder unavailable` and the wait continues. `status --json` exposes the corresponding `lease_release_requests` and `coordinated_operations` records. | Removing these surfaces hides lease and write-lock contention or makes a slow finish look stalled; timeout preserves the session for explicit recovery. Coverage: `status_operations_cli.rs`, `lease_requests_cli.rs`, `finish_cli.rs` (including `finish_timeout_does_not_kill_worktree_removal_after_the_session_closes`), `repository_lock_reports_progress_while_waiting`, and `a_failing_holder_lookup_does_not_abort_a_progress_reporting_wait`.
| README quickstart and repository deployment guide | Presents the operator flow as: install the binary pair once -> review `aethyme deploy plan --diff` against the exact remote default -> authorize `deploy execute` by digest -> retain `aethyme deploy verify` in CI -> run `aethyme broker quick-test` -> start and submit broker sessions. The offline/manual `deploy` path remains explicit and does not publish. | Renaming/removing `deploy plan`, `deploy execute`, `deploy verify`, `broker quick-test`, `broker start`, or `broker submit` breaks first-run enrollment. Omitting committed policy leaves agents without mandatory broker instructions even when the executable is installed. |
| README operator verification row and broker CLI help (2026-07-28) | Presents `aethyme broker advanced verify-loop` as the first-class broker E2E command, with the `e2e` alias deprecated since v0.8.4. The command snapshots the local integration head, runs quick-test, doctor, and source tests when in the Aethyme checkout, then warns/fails if integration moved. Human output is the documented surface; `verify-loop --json` is best-effort unless promoted into `docs/json-contracts.md`. | Renaming/removing `broker advanced verify-loop` or the `broker advanced verify-loop` alias breaks operator runbook verification and makes moving-integration proof stale again. |
| ~~Broker CLI help for integration stability checks: `aethyme broker advanced integration wait-stable`~~ (REMOVED 2026-11-12; introduced 2026-07-28) | Hard-deleted together with `broker advanced integration status`, the `IntegrationStatusView` / `IntegrationStabilityReport` JSON shapes and the `Broker::integration_status` / `Broker::wait_integration_stable` API. The owner decided on 2026-11-11 that pull requests are the only landing path; both commands only described the local integration landing path. The `integration.may-move` status advice now points at `aethyme broker advanced agents`, `summary.commands` no longer names either command in any mode, adopt/reuse drift and finish recommend `aethyme broker status` where they named `integration status`, and `broker status` stays the inspection surface for the integration head and its relation to the default branch. | `aethyme broker advanced integration status` and `... wait-stable` now fail as unknown actions (`integration` keeps only `reconcile`). Out-of-repo scripts that read `integration status --json` must read `broker status --json` (`summary.integration_head`, `summary.integration_relation`, `advice[]`). |
| Broker CLI help for local source repair (updated 2026-08-23) | Presents `aethyme broker status doctor --fix-version` as the explicit source-checkout repair for stale installed binaries. The command installs and independently verifies both `aethyme` and `aethyme-engine-cli` from one temporary integration worktree. `doctor --json` retains the legacy top-level repair fields and adds ordered `commands` plus per-component `steps`; this JSON remains best-effort unless promoted into `docs/json-contracts.md`. | Removing or making this implicit brings back the stale-binary manual reinstall loop; changing the repair source can install local WIP, and reporting success before every install/verify step passes can leave a mixed product version. |
| Push wrappers, CI pollers, webhook workers, and future Chau7/MCP delivery adapters for production PR follow-up (2026-07-31) | `aethyme broker advanced pr check [--target <branch>] [--pr <number>] [--agent <name>] [--dispatch] [--json]` reads PR state via `gh`, stores a deterministic activity fingerprint in `.aethyme/broker.db`, writes a bounded prompt under `.aethyme/run/pr-follow-up/`, and may reuse/spawn a `Push2prod` broker session when `--dispatch` is explicit. JSON is provisional until promoted into `docs/json-contracts.md`. | Renaming/removing `broker advanced pr check`, changing marker strings (`thumbs_up`, `looking`, `none`), or changing decision/dispatch enum strings breaks automation that decides whether to send a prompt to an existing tab or start a new agent after a successful push. Git has no native post-push hook, so callers must run this from a wrapper/controller after push success rather than from `pre-push`. |
| Stable broker JSON command contracts (`docs/json-contracts.md`) | `aethyme broker status --json`, `aethyme broker advanced events --json`, `aethyme broker advanced metrics --json`, `aethyme broker advanced insights --json` (versioned separately by `INSIGHTS_SCHEMA_VERSION`, see `docs/insights-contract.md`), and `aethyme broker submit --json`. `aethyme broker advanced integration status --json` was removed on 2026-11-12 (hard delete; pull requests are the only landing path). | Scripts and dashboards consuming broker state can break if these commands, field names, or enum strings are renamed/removed without a versioned contract change. |
| Readers of lease overlaps: `broker status --json` (`overlap_pairs`), `broker advanced leases --json` and `broker agents --json` (`overlap_pairs`), `broker submit --json` (`lease_warnings`), ownership-violation reports (`warned_leases`, `conflicting_leases[].severity`/`reason`), and `lease.overlap` event consumers (changed 2026-09-30) | Overlaps are grouped per session pair and ranked by `git merge-tree`: `high` when the two sessions' current states conflict on an overlapping path, `low` otherwise or when unclassified (`classified: false` with a `reason`). `overlaps` and `summary.overlap_count` keep their per-path meaning; the summary sentence now counts pairs. `lease.overlap` fires once per pair on start and on severity change instead of once per path. The submit ownership audit blocks on another session's explicit lease only when that holder is actively working **and** the pair conflicts on the leased path; other leases move to `warned_leases` and do not fail the audit. Since 2026-10-01, in `[promote] mode = "verify-only"` repositories it never blocks: those leases also move to `warned_leases` (and submit's `lease_warnings`) with a reason naming `verify-only` and the coordination command. A pair whose session is mid-merge stays `classified: false` with the reason `session <id> is mid-<operation> with unresolved conflicts`. Since 2026-10-02 one rule (`LeaseRefusalPolicy`) decides every lease refusal: `leases claim`, `leases plan` (`would_conflict`), planned leases at `start --path`, guarded exec and submit refuse only on another session's explicit lease whose holder is actively working, never under verify-only (submit additionally requires the pair to conflict). `leases claim --json` adds `warnings[]` (the non-refusing overlaps of live sessions, each with `holder_status`, `reason` and the `note send` command); guarded exec no longer fails on a stale or idle holder's explicit lease, nor under verify-only. | A reader counting `lease.overlap` events as conflicts now sees far fewer, by design. A wrapper that relied on a bare explicit claim blocking another session's submit must now expect a warning instead unless the holder has a conflicting edit in progress. A wrapper that treated exit 3 from `leases claim` as "someone touched this path" must read `warnings[]` instead: implicit leases, idle or stale holders and every verify-only overlap now return exit 0. Classification must never rewrite another worktree's index: the broker reads its mtime as liveness. |
| Readers of duplicate-work signals (introduced 2026-10-01) | `broker start`, `start-agent`, `adopt` and `push --json` add `duplicate_work` (`session_id`, `status`, `task`, `reason`: `same_branch`, `same_pr` or `task_mentions_pr`, `pull_request`), omitted when empty. `broker status --json` adds `session.duplicate-work` rows (`warning` when both sessions are active or idle, `info` otherwise) and `session.mid-merge` rows (`warning`). `[leases] ignore` entries containing `/` are now exact repository-relative paths, and the rules are read from the committed default branch like `[promote]`. | Advisory only: nothing blocks on these fields. A wrapper that expects `start --json` keys to be fixed must tolerate the optional `duplicate_work` key. |
| Readers of lease liveness (introduced 2026-10-06, #360): `broker status --json` (`lease_liveness[]`), `broker advanced leases explain [<paths...>] [--json]`, `broker advanced leases plan --json` (`conflicts[]`/`owned[]` `liveness`, `liveness_evidence`) and `session.holder_gone` event consumers | Each lease row carries `lease_id`, `session_id`, `path`, `kind`, `liveness` (`active`, `idle`, `stale`, `released`, `unknown`) and `liveness_evidence` (`basis`: `holder_running`, `holder_gone`, `released`, `no_recorded_holder` or `process_table_unavailable`; `holder_pid`, `last_activity_at`, `stale_since`, `grace_ends_at`, `holds`). Liveness is bound to the holder process #542 records: `idle` after `[leases] idle_minutes` (default 30) without a broker command, `stale` once the process is gone. `session.holder_gone` (`pid`, `started`, `command`) is recorded once per holder by `broker status` and starts the `[leases] stale_grace_minutes` (default 15) grace. Past the grace (`holds: false`) the lease no longer makes `leases plan` conflict, no longer refuses a claim or a planned lease at `start`, and is still listed. `unknown` never releases a hold. | A wrapper that read only `owner_status` keeps working: the new fields are additive. A wrapper that treated a gone holder's lease as blocking forever now sees it stop conflicting after the grace; one that treated `unknown` as dead is wrong by contract. |
| Readers of lease release on finish (introduced 2026-10-06, #358): `broker finish --json` (`leases_held[].state`, `warnings`, `next_commands`) and `lease.released` event consumers | A verified terminal `finish` (status `closed` or `cleaned`) releases every lease the session holds in the transaction that closes it, before any physical cleanup, recording one `lease.released` event per lease with `reason: "finish"`, `lease_id` and `created_at` (the generation). `leases_held[].state` reads `released` as soon as the store confirms the session holds no active lease, even when the worktree is kept. A refused finish releases nothing. While a gate runs for the session (a live or unreadable `.aethyme/run/gates/<session>-<gate>.pid`), finish refuses with the warning `gate <name> is still running for this session; its leases stay held until it ends` and a retry command. A finish touches only its own session's leases, never another session's generation of the same path. | A reader that waited for `cleanup.completed` before treating leases as free can now act on `state: released`. A wrapper that finished sessions with a gate still running now gets a refusal and must retry after the gate ends. Plain `lease.released` events from `leases release` keep the `path`-only payload. |
| Lease release requests (introduced 2026-10-06, #359): `broker advanced leases request-release <path> --session <id> --reason <why>`, `leases ack <request-id> --session <holder>`, `leases decline <request-id> --session <holder> --reason <why>`, `leases wait <path> --session <id> [--timeout <seconds>]`, `broker status --json` (`lease_release_requests[]`, advice `lease.release-requested`) and the `lease.release_*` events | A request is its `lease.release_requested` event (the event id is `request_id`); one is recorded per other session holding an overlapping lease, and an identical pending request is returned rather than repeated. Each request reports `request_id`, `path`, `requester_session_id`, `holder_session_id`, `reason`, `requested_at`, `state` (`pending`, `acked`, `declined`, `granted`, `released`, `cancelled`), `resolved_at` and `resolution_reason`. Only the holder may `ack` (its leases on the path are released, each recorded as `lease.released` with `reason: "request_acked"`, `lease_id` and `created_at`, the same payload a finish records) or `decline` (they stay); anything else is refused with exit 3. A pending request is granted (`lease.release_granted`, reason `holder_stale`) when every holder lease on the path is `stale` past the grace (#360), releasing them with `reason: "request_granted"`; `unknown` liveness is never granted against. `broker status` runs that grant, lists pending requests and those resolved in the last day, and raises a `lease.release-requested` notice on the holder's session. The earliest event after a request among requester closure, a matching holder release and an explicit outcome determines its resolution. If an unresolved request's holder no longer holds the path but no matching release event can be parsed, it is reported as released with `resolution_reason: release_evidence_unavailable` and no `resolved_at`. Requests are advisory and do not block edits in separate worktrees; inspect the holder's task and relevant diff before following up. `leases wait` explicitly blocks and exits 0 (`released`), 10 (`granted`), 11 (`declined`), 12 (`timeout`, default 300 s), or 13 (`cancelled` when the requester session closes while a holder still holds the path), judging each holder by its latest request. | A wrapper polling `leases --json` can call `leases wait` instead. A wrapper treating any non-zero exit as failure must read 10 as success: the path is free. Requests live in the event log, so `events` retention bounds their history. |
| Readers of ownership claims: `broker status --json`, `broker status --summary`, `broker advanced ownership --json` (introduced 2026-10-03) | `aethyme broker advanced ownership claim <name> --session <id> --reason <text>` records which session drives a named operation (a release, a merge chain) in the additive schema-v46 `ownership_claims` table. `status --json` adds `ownership_claims[]` (`id`, `name`, `session_id`, `purpose`, `claimed_at`, `taken_over_from`, `holder_status`, `holder_agent`, `holder_short_name`, `last_active_at`, `last_operation_at`, `last_operation_reason`, `working`) and one `ownership.claimed` advice row (`info`) per claim, also in `--summary`. `ownership claim` exits 3 only while another session holds the name and is working (active, or a coordinated operation inside the idle window); otherwise it takes the claim over and `--json` returns the previous holder under `replaced`. Claims of closed sessions stop counting without a write. | Advisory only: nothing else blocks on a claim. A wrapper that expects `status --json` keys to be fixed must tolerate `ownership_claims`. A v42–v45 binary keeps using a v46 database and neither sees nor writes claims. |
| Readers of unpushed work: `broker status --json`, `broker doctor --json`, `broker finish --json`, and wrappers scripting `finish`/`finish close` (introduced 2026-09-30) | `status` and `doctor` gain an additive `unpushed_work` object (`push_session_branches`, `sessions[]` with `session_id`/`branch`/`head`/`unpushed_commits`/`oldest_unpushed_at_ms`/`severity`/`command`, optional `integration` with `branch`/`head`/`upstream_ref`/`unpublished_commits`/`on_no_remote`/`oldest_unpublished_at_ms`/`severity`), omitted when empty, plus `session.unpushed-commits` and `integration.unpublished-work` advice. `finish --json` gains `unpushed_commits` under `[delivery] push_session_branches = true`. `finish` and `finish close` accept `--abandon --reason <why>` and emit `broker.session.abandoned_unpushed`. | Under the push lane `finish` and `finish close` refuse sessions holding commits no remote has; a wrapper that closed sessions unconditionally now needs `broker push` first, or an explicit `--abandon --reason`. Counting inherited or patch-equivalent commits would report delivered work as at risk and teach operators to ignore the number. |
| Readers of the session start base: `broker start --json` (and `--cmd`), `broker status --json` (introduced 2026-09-30) | `start_base.evidence` gains `fetched_default_branch`, and `start_base.ref_name` may be `refs/remotes/<remote>/<branch>`. An additive, optional `start_base.bypassed_integration` (`ref_name`, `commit`, `reason` = `verify_only`/`behind_upstream`, `behind_default_commits`, `ahead_default_commits`, optional `recovery_command`) names the integration branch a start did not use. `status` gains an `integration.behind-upstream` advice row. `[promote]` is now read from the `.aethyme/config.toml` committed on the fetched default branch, falling back to the working-tree file. | A wrapper that assumed every session starts from `refs/heads/aethyme/integration`, or that read `diff_base` as an integration commit, now also sees upstream bases. A repository whose committed config differs from its main checkout's working tree now follows the committed one. Covered by `aethyme-broker/tests/start_base_cli.rs`. |
| Readers of integration-movement signals in `verify-only` repositories: `broker status --json` (`advice[].id`, `summary.may_move_integration`, `summary.commands`), `broker doctor --json` (`integration_movement`) (introduced 2026-10-01) | Under `[promote] mode = "verify-only"` (read from the default branch) `status` no longer emits `integration.may-move`, `integration.upstream-main-ahead`, `integration.fast-forward-available`, `integration.stale-promotions` or `main.external-writes` (`integration.behind-upstream` was already silent); `summary.may_move_integration` is `false`, the summary note reads `verify-only: submit does not move integration`, and `summary.commands` names neither `integration wait-stable` nor `integration status`; `doctor` omits `integration_movement`. `integration.unpublished-work` stays, without recommending integration commands. Since 2026-10-03, `integration.leftover-work` (severity `warning`; `notice` when a refreshed drift assessment finds the layer stale-only) reports commits integration holds that the published branch (`origin/HEAD`, else the tracked upstream) lacks, on every status rather than only `--refresh`, with the command `aethyme broker advanced integration reconcile --upstream <branch> --dry-run`. `auto` and `manual` repositories are unchanged. | A wrapper that waited on `integration wait-stable` whenever `may_move_integration` was true, or that treated `main.external-writes` as a reason to stop, no longer sees those signals where submit cannot move integration; each was false there, and `upstream-main-ahead` was reported as `blocked`. Covered by `aethyme-broker/tests/verify_only_main_cli.rs`. |
| Readers of failed broker commands: `broker.command.failed` event consumers (`broker advanced events --kind broker.command.failed`), `broker doctor --json` (`recent_command_failures`), anyone reading a coordinated `git`/`gh` failure's `Error:` line, and `broker advanced operations show <id> --json` details (introduced 2026-10-02; extended for #415) | Additive: the `broker.command.failed` payload gains `message`, the error the command printed with credentials cut (token prefixes such as `ghp_`, `token=`/`password=`/`Authorization:` assignments, and URL user-info) and capped at 500 characters; it is absent when the command printed no error. `doctor` adds `recent_command_failures[]` (`event_id`, `ts`, `session_id`, `command_surface`, `exit_code`, `failure_class`, `message`; last 24 h, newest 10), omitted when empty and never counted against health. A failed coordinated operation's error now ends with up to three lines of the provider's stderr: `coordinated <git\|gh> operation <id> failed (exit <n>): <stderr tail>`. In addition, failed local pre-push checks and every failed coordinated `git` operation (a refused push, a failed `merge --ff-only` during main reconcile) persist an additive `details.failure_output.stderr_tail` array in operation history, capped at 12 lines and 256 characters per line (plus a truncation ellipsis); terminal controls are removed and credentials redacted, and the field is omitted when stderr has no printable content. The pre-push check strips inherited inline Git config so its hook selection matches the actual push. `broker.command.succeeded` payloads are unchanged. The publication `report` still copies only allowlisted fields and never the message. | A wrapper matching the exact `coordinated … operation <id> failed` text must accept the added suffix. Failure output is bounded local diagnostics: consumers must keep it out of publication reports, and readers of operation details must tolerate the optional `failure_output` key. The message still keeps paths and task text, so event publishers must keep excluding `message`, as `report` does. Covered by `aethyme-broker/tests/command_failure_messages_cli.rs`, `aethyme-broker` failure-stderr unit tests, and the `pre_push` cases in `aethyme-broker/tests/operations_e2e.rs`. |
| Readers of host operation recovery IDs and pre-push failures (introduced 2026-10-08, #284) | `broker advanced operations show <id\|host-operation-id>`, `broker advanced operations reconcile --operation <id\|host-operation-id>`, and `broker advanced gates pre-push`: show/reconcile accept the 32-hex host operation id printed by a refusal (with or without the `hostop:` prefix); `show --json` returns the repository operation report when linked or a `{host_operation: ...}` object for a host-only record; reconcile routes a host-only id through the guarded host recovery path, which (like `broker unblock hostop:<id>`) refuses with exit 3 when the host row's remote key is not one of this repository's remotes, because the host database is shared by every repository on the host. Failed pre-push checks print failed gate names and a bounded output tail before exiting nonzero. | JSON readers must tolerate the host-only `host_operation` object, and wrappers must accept the host operation identifier form and the bounded failure output. The output is diagnostic, not a successful verification result. Covered by `blockers_cli.rs`, `gates_cli.rs`, `reconcile_usage_cli.rs`, and `operations_cli.rs`. |
| Exact-ref push inspection proposals (introduced 2026-10-10, #695) | `aethyme broker advanced operations reconcile --operation <id> --inspect-remote --reason <text> [--json]` accepts only an outcome-unknown linked Git write with a persisted exact-ref plan. It queries every planned destination without mutating remote refs, journals `remote_inspection` evidence (remote, classification, proposal, pre-push/proposed/observed SHAs), and returns `source: remote_inspection`, optional outcome, `reconciled: false`; `operations show --json` exposes the evidence at `reconciliation.remote_inspection`; the write barrier remains. The existing `--outcome` form remains the explicit operator assertion and journals `reconciliation.source: operator_assertion`, also exposed as `reconciliation.source` by show. | Consumers must treat a proposal as a current-state snapshot, not a final verdict; partial, changed, or unreadable evidence has no proposal. |
| Readers of the command history (`.aethyme/logs/command-metrics.jsonl`, `broker.command.*` events, `broker metrics`/`insights`) and the PR monitor (`scripts/adapters/aethyme-pr-monitor.sh`) (introduced 2026-10-06, #417) | A `deliveries claim` or `deliveries dispatch` that finds nothing to claim and exits 0 writes no metric line and no `broker.command.succeeded` event; a poll that claims, any failed poll, and `deliveries complete` are still recorded, and `deliveries list` is a read that records nothing. The PR monitor runs `deliveries list --adapter chau7 --json` after its tick and starts the Chau7 adapter only when that prints something other than `[]` or fails. | A history reader that counted empty polls as activity now sees none; they were 79% of all commands and buried real failures. Changing `deliveries list --json` to print anything but the literal `[]` for an empty outbox, or making it record telemetry, would start the adapter on every pass again. |
| Repository-wide PR subscriptions: `broker advanced watch repo start\|list\|show\|events\|pause\|resume\|stop`, `deliveries subscribe --repo-watch <id> --policy review\|notify`, `watch pr tick --json` (`repository_watches`), `deliveries dispatch --json` (`source`), `deliveries list --json`, `.aethyme/config.toml` `[watch.prompts.review] body`, and the PR monitor plus Chau7 adapter (introduced 2026-10-07, #606) | Additive schema v48 (`MIN_COMPATIBLE_SCHEMA` stays 47) adds `repository_watches`, `repository_watch_pull_requests` (the seen set), `repository_watch_events` (UNIQUE per watch, PR and kind), `repository_delivery_subscriptions` and `repository_delivery_outbox`. `watch pr tick --json` adds `repository_watches` (`due_watch_count`, `polled_watch_count`, `failed_watch_count`, `event_count`, `results[]` with `events` and `auto_watched`). Repository delivery ids are offset by 1000000000000 and `deliveries complete --id` routes on that; `dispatch --json` adds `source`; `list --json` mixes in items with `source: "repository"` and `event_id`, and still prints `[]` when both outboxes are empty. New events: `watch.repository.started`, `watch.repository.event`, `delivery.repository.subscribed`, `delivery.repository.<status>`. A review prompt template may name only `number`, `title`, `author`, `url`, `head`, `draft`, `repo`, `event`; `title` and `author` are rendered as quoted JSON strings and labelled untrusted. | Lowering the id offset would let a repository delivery id collide with a PR delivery id and complete the wrong row. Interpolating `title` or `author` unquoted would let a PR author write instructions into the reviewing agent's prompt. Printing anything but `[]` for an empty combined outbox would start the Chau7 adapter on every monitor pass (#417). |
| Every binary that opens `.aethyme/broker.db`: the schema v48+ repair pass (introduced 2026-10-08) | Migrations from v48 on apply idempotently: a column-only migration is skipped when its columns already exist, and every open checks that the objects of recorded migrations v48+ exist (`repository_*` tables, `coordinated_operations.agent_provenance_json`), re-creating any that are missing with `IF NOT EXISTS` inside one `BEGIN IMMEDIATE` transaction. A current database pays two read-only checks. `MIN_COMPATIBLE_SCHEMA` stays 47 (schema v50, a no-op marker for #660's collaboration fence, did not raise it). | A build from an unmerged branch once stamped its own "v48" onto a real database, so #611's v48 tables were skipped and #613's v49 failed with "duplicate column name", locking every broker command out. Dropping the repair pass or the column check brings that lockout back; never point a dev build at a real broker database. |
| Readers of hook routing health: `broker doctor --json` (`hooks_path`) and `aethyme deploy` stderr (introduced 2026-10-03) | Additive: `doctor` adds a `hooks_path` object (`configured`, `scope`, `origin`, `resolved`, `problem` one of `missing`/`not_a_directory`/`no_executable_hooks`, optional `shipped_hooks_dir`, `fix`) when the effective `core.hooksPath` makes git skip every hook: the path is missing, is not a directory, or holds no executable git hook while the repository ships `.husky`, `.githooks` or `.hooks`. A relative value resolves against the checkout root, as git resolves it. `doctor` text prints a `git hooks:` line with `resolved:` and `fix:`; `deploy` prints the same as `warning:` on stderr and still exits 0. Omitted when healthy; never counted against health; the git config is never changed. | `fix` is a suggestion for a human, preferring the repository's install script, then `npx husky`, then `git config [--<scope>] core.hooksPath <shipped dir>`; a wrapper must not run it unattended. A wrapper treating any `deploy` stderr as failure now needs the exit code instead. Covered by `aethyme-broker/tests/hooks_path_doctor_cli.rs` and `aethyme-cli/tests/repository_deploy_cli.rs`. |
| Readers of leftover integration work in `broker doctor --json` (`leftover_integration_work`) (introduced 2026-10-04) | Additive: in a `verify-only` repository whose integration branch holds commits the published branch (`origin/HEAD`, else the tracked upstream) lacks, `doctor` adds `leftover_integration_work`, the same advice object `status` reports as `integration.leftover-work` (`id`, `severity`, `reason`, `summary`, `evidence`, `commands` = `aethyme broker advanced integration reconcile --upstream <branch> --dry-run`). `doctor` text prints an `integration leftover work:` line with the evidence and `run:` the command. Omitted when integration holds nothing of its own, when there is no integration branch, and in `auto` and `manual` repositories; never counted against health. `doctor` reads the integration ref without fast-forwarding it and runs no reconciliation. | None: a new optional key. The command is a dry run for a human to read; a wrapper must not follow it with a real reconcile unattended. Covered by `aethyme-broker/tests/integration_drift.rs`. |
| Readers of a `verify-only` integration branch, the `broker.integration.refreshed` event and the `integration.refreshed` status advice (introduced 2026-10-06, #352) | Under `[promote] mode = "verify-only"`, `aethyme/integration` is a disposable verification base. `broker start`, `broker status` (both the current and `--refresh` views, not `--summary` and not read-only snapshots), `broker submit` and `broker sync` advance it onto the fetched default branch (`origin/HEAD`, else the tracked upstream) whenever it carries nothing that branch lacks, through the same reviewed-plan machinery as the post-merge cleanup (`integration reconcile`, compare-and-swap ref update, journaled). `status` only takes the strictly-behind case; the others also clean a layer whose recorded promotions are all proven upstream. Each advance appends `broker.integration.refreshed` with payload `trigger` (`start`, `status`, `submit` or `sync`), `branch`, `upstream_ref`, `from`, `to`, `cleaned_queue_entry_ids` and `explanation`, and the `status` call that advanced it reports the advice row `integration.refreshed` (severity `notice`, no commands). An integration holding work of its own is never moved, and `integration.leftover-work` still names the reviewed path. Nothing is fetched by `status` or `submit`; `start` and `sync` already fetch the default branch. `auto` and `manual` repositories are unchanged. | A wrapper that asserted a verify-only `submit` leaves `aethyme/integration` untouched now sees it follow the default branch; it never receives the session's work. A reader of the event log gets one new kind. Covered by `aethyme-broker/tests/integration_verify_base.rs` and `verify_only_main_cli.rs`. |
| Readers of a `verify-only` repository's integration branch once nothing maintains it (soft-retired 2026-11-12, Clean-Core Plan Phase 1) | Under `[promote] mode = "verify-only"` with a fetched default branch (`origin/HEAD`, else the tracked upstream), `aethyme/integration` is neither created nor moved: `broker status`, `broker submit`, `broker sync`, gates and reconcile no longer fast-forward it, nothing emits `broker.integration.refreshed` or the `integration.refreshed` advice row any more, and after a coordinated `gh pr merge` the `post_merge_cleanup` reports `state` `not_needed` (with `upstream_ref` and `fetch_operation_id`, no `cleanup`) instead of rebuilding it. An existing ref is left where it is, never deleted. `broker status --json` `summary.integration_head` (and the snapshot views) now carry the fetched default branch tip, so `integration.leftover-work` no longer appears in status (`doctor` still reports `leftover_integration_work`). The adopt/reuse `integration_drift` object compares against that branch: `integration_branch` names it (for example `origin/main`) and `safe_next_action` is `aethyme broker sync --session <id>` for `behind` and `diverged`, `aethyme broker status` for unowned `ahead`. `integration reconcile` and `doctor` still read the ref, unmoved (`integration status` and `integration wait-stable` were removed later the same day). A verify-only repository with no fetched default branch, and `auto` and `manual` repositories, are unchanged. | A wrapper that expected integration to follow the default branch in a verify-only repository now finds it where it was; read the default branch instead. A reader keyed on `integration.refreshed` or `broker.integration.refreshed` never sees them again. Covered by `aethyme-broker/tests/integration_verify_base.rs`, `integration_drift.rs`, `verify_only_main_cli.rs` and `post_merge_cleanup_cli.rs`. |
| Readers of lock and lease waits: `broker status --json` and `status --summary --json` (`waiters`), stderr of `broker finish` (introduced 2026-10-08, #494) | Additive: `status` gains an optional `waiters` array (see `docs/json-contracts.md`), read from per-wait records in `.aethyme/run/waits/` that a process blocked on the coordinated write lock, a gate owner lock or `leases wait` writes on first contention and removes when the wait ends; the human view adds a `Waiting on locks and leases:` section. The stderr wait lines are unchanged. A `finish` that exceeds `--timeout` now reads the session back and says which state it left: still open (retry with `--timeout <2x>`) or already closed (inspect `handoff`, do not retry); the message starts `finish timed out after <n>s waiting on Git (` instead of `... while checking Git (`. | Advisory only: nothing blocks on these fields. A wrapper matching the old finish timeout wording must match `finish timed out after` instead. Covered by `aethyme-broker/tests/submit_progress_status.rs`, `tests/finish_cli.rs` and the `waiters`, `gates` and coordinated-lock unit tests. |
| Coordinated-operation lanes, journal and stats readers: `broker advanced operations list/show/stats --json`, `broker status`, the head-of-line check of every coordinated write, and repository config `[coordination] session_hooks_hold_repository_lock` (introduced 2026-11-12) | Additive. A `git commit` run through `broker advanced git` that the broker proves is confined to its session (session's own linked worktree, `HEAD` = session branch, no leading global options, no declared `--scope`) is journaled with the reserved scope `session-commit:refs/heads/<branch>` and holds the lane `<repository>::git-ref/refs/heads/<branch>` plus the shared Git common-directory and repository compatibility lanes. A caller declaring a `session-commit:` scope is refused. `broker push` journals a session-branch push as `git push --no-follow-tags --porcelain -- <remote> <sha>:refs/heads/<branch>` (scope still `session-push:refs/heads/<branch>`) when the strict profile is proven, holding the same branch lane and the local ref lane `<git-common>::refs/remotes/<remote>/<branch>`; otherwise the old `push <remote> <sha>:<ref>` spelling and the repository-wide lane. `gh pr edit <n>` (without `--base`) and `gh pr ready <n>` hold `<repository>::pull/<n>`. `operations stats` gains `narrow_lanes` (`sample_count`, `session_branch_sample_count`, `lock_hold_ms`, `queue_wait_ms`), and `unrelated_contention` now treats `session-push:`, `session-commit:` and `git-ref/` branch scopes as known. `session_hooks_hold_repository_lock = true` (default false) restores the repository-wide lane for every coordinated commit, and for a session push whenever a `pre-push` hook exists; a `reference-transaction` hook always keeps commits, fetches and pushes repository-wide. A strict fetch or push spelling whose repository profile cannot be proven before it queues runs repository-wide and is journaled with scope `repository` (recomputation treats a Git record with that scope as wide); `broker push` also keeps its old spelling when `push.followTags` is set. Writer preference: before polling for its lanes, a repository-wide writer holds the exclusive lock `<git-common>::wide-intent` (Git) and `<repository>::wide-intent`; every narrow Git writer takes and releases both shared first, in the order intent (common, repository), common, destination, repository, lane. The wait is recorded as `coordination_timing.wide_intent_wait_ms` and the phase `coordinated_wide_intent_wait`. When `hooks_outside_lock` is on, the real push now inserts `--no-verify` right after `push`, not at the end of the argument list. | A reader that recomputes a lock scope from `command_json` alone cannot see a session commit's lane: it must also read the reserved `scope` prefix (as `stored_resource_scope` does), or it treats a running commit as repository-wide and could mark it `outcome_unknown`. Renaming the prefix, the config key or the stats fields breaks recorded-history recomputation, the opt-out, or stats consumers. An unknown-outcome session commit or push write-blocks its branch lane and every repository-wide write; an unknown repository-wide write still blocks every lane. Covered by the session-lane tests in `aethyme-broker/tests/operations_e2e.rs` (controlled hooks, `QueueWait::Refuse`), `a_session_push_holds_only_its_branch_lane_when_the_profile_is_proven` in `tests/session_push_cli.rs`, and the `git_scopes`, `operation_stats` and pull-request scope unit tests. |
| Narrow-lane decisions, lock waits and lock holders: `broker advanced operations list/show/stats --json` (`coordination_timing`, `narrow_lanes`), command metrics phase counts, and `.aethyme/locks/operations/*.holder.json` (introduced 2026-11-13) | Additive. Every coordinated Git `push`, plain `commit` and `fetch` write journals its lane decision in `coordination_timing`: `narrow_lane` (`taken` or `unavailable`), `narrow_lane_kind` (`push`, `commit`, `fetch`) and `narrow_lane_unavailable_reason` (null when taken), and counts the phase `<kind>_narrow_lane_taken` or `<kind>_narrow_lane_unavailable`. Reason codes are stable: `ambient_git_override`, `session_branch_not_plain`, `commit_outside_session_worktree`, `main_checkout_commit`, `foreign_repository_worktree`, `head_not_session_branch`, `evidence_truncated`, `common_directory_identity_unavailable`, `ref_evidence_unreadable`, `hook_directory_unreadable`, `reference_transaction_hook`, `pre_push_hook`, `push_follow_tags`, `push_follow_tags_unreadable`, `shallow_repository`, `not_strict_spelling`, `config_inventory_unreadable`, `push_url_or_mirror`, `custom_ssh_command`, `repository_extension`, `url_rewrite`, `ext_transport`, `custom_remote_program`, `nonstandard_remote_fetch`, `unrecognized_config`, `session_hooks_hold_repository_lock`, `declared_scope`, `no_live_session`, `force_with_lease_push`, `not_session_branch`, `admission_timed_out`, `probe_failed`, `unclassified` (a test keeps the last one unreachable from known refusals). `broker push` asks the same check before choosing its spelling; when it falls back to `push <remote> <sha>:<ref>` the operation re-derives and journals the reason. Since 0.8.90 the default-branch fetch `push` and `sync` run is journaled in the strict spelling (`fetch --atomic --no-tags ... -- <remote> refs/heads/<b>:refs/remotes/<remote>/<b>`) instead of `fetch <remote> refs/heads/<b>:refs/remotes/<remote>/<b>`, so it takes its tracking ref's lane (`fetch_narrow_lane_taken`) where the configuration proves the footprint; readers matching the old argv must accept both. `extensions.worktreeConfig` no longer refuses the strict profile (the inventory already reads the worktree's `config.worktree`); every other `extensions.*` key still does (`repository_extension`). `queue_wait_ms` now measures from `lock_wait_started_at` to the moment the last lock was held, so it includes every lane; the parts are `wide_intent_wait_ms`, `git_common_lane_wait_ms`, `local_ref_lane_wait_ms`, `repository_lane_wait_ms` and `lock_queue_wait_ms` (the final lock alone, the old meaning of `queue_wait_ms`). A write refused while waiting (`wide_intent_unavailable`, `local_git_lane_unavailable`, `local_ref_lane_unavailable`, `repository_lane_unavailable`, `lock_unavailable`) journals `coordination_timing` (no `lock_acquired_at`, `queue_wait_ms` = the wait until refusal) and `holder` (`description`, `operation_id`, `session_id`, `pid`, `scope`, `started_at`, `held_ms`, and since 0.8.90 `phase`, null for a journaled holder or one that named none). Every exclusive write-lock holder, journaled or not, writes `<lock>.holder.json` (`schema_version`, `kind` = `coordinated_operation` or `git_maintenance_lock_apply`; `submit_plan_reuse` only from binaries before 0.8.90, `command`, `pid`, `session_id`, `operation_id`, `acquired_at_ms`, and since 0.8.90 the optional `phase` and `phase_started_at_ms` an unjournaled holder updates as it moves through its steps, also on the common-directory lane it nests) while it holds the lock and removes it before releasing; a record whose pid is gone is ignored; the holder's `description` ends `; now <phase> (for <t>)` when a phase is recorded. `operations stats` adds to `narrow_lanes`: `taken_count`, `unavailable_count`, `unavailable_by_reason` (code to count) and `by_kind` (`push`/`commit`/`fetch` to the same three), counted from every row with a decision, refused rows included. `submit` no longer waits for another checkout's lifecycle claim while holding the write lanes for plan reuse: it gives the reuse up and plans again. Since 0.8.90 `submit` holds no Git lane while it plans, replays and captures the base tree: it reads the history view under a short shared hold of the common-directory and repository lanes (the form narrow writers hold, yielding to a waiting wide writer) and proves it unchanged under another before reusing the plan, from the operation journal (no repository-wide Git write queued after, or started since, the first reading); otherwise it builds the plan again. Because planning and replay Git calls honour replace refs and grafts, it proves the view unchanged a second time after the replay and base-tree capture; if that fails it reads the view afresh, builds and replays once more and proves that, and otherwise refuses (exit 3, `SubmissionHistoryViewMoved`: names the cause and says to rerun `broker submit`; nothing is verified). A repository with no canonical coordination key, an already overridden view or unopenable lane lock files keeps the earlier unvalidated behavior. Consumer-side readings wait up to 10 s for a repository-wide writer to release the lanes. Command metrics carry `submit_plan_reuse_snapshot_hold` and `submit_plan_reuse_validate_hold` (milliseconds the shared lanes were held) and the counts `submit_plan_reuse_reused`, `submit_plan_reuse_invalidated` (view not proven unchanged before reuse), `submit_plan_reuse_inputs_moved` (base or session input moved while planning), `submit_plan_reuse_unavailable` (view not readable before planning), `submit_plan_reuse_replay_validated` (replay proven the first time), `submit_plan_reuse_replay_rebuilt` (proven after the one rebuild), `submit_plan_reuse_replay_refused` and `submit_plan_reuse_replay_uncoordinated`. | Rows written before this release carry no decision and the old `queue_wait_ms` (final lock only); stats count them as before and leave them out of the decision counts. Renaming a reason code, a field or the holder-record name breaks stats consumers and the waiter's holder lookup. Covered by `every_unavailable_refusal_has_a_stable_reason_code`, `a_worktree_config_extension_keeps_the_lane_and_each_refusal_names_its_reason`, `an_exclusive_holder_is_recorded_next_to_its_lock_until_it_releases`, `a_refusal_behind_an_unjournaled_hold_names_the_holder_and_its_phase`, `shared_read_lanes_admit_narrow_writers_and_exclude_wide_ones` and `queue_wait_includes_every_lane_and_the_lane_decision_is_recorded` in `operations/git_scopes.rs`, `stats_aggregate_lane_decisions_by_reason_including_refused_writes` in `operation_stats.rs`, `other_sessions_git_writes_do_not_wait_while_submit_plans_and_replays`, `a_repository_wide_write_during_planning_rebuilds_the_plan` and `a_default_branch_move_during_planning_verifies_against_the_new_base`, `a_replace_landing_before_the_replay_rebuilds_and_verifies_the_real_tree`, `a_second_history_change_during_the_rebuild_refuses` and `a_wide_write_queued_before_planning_that_runs_during_it_rebuilds_the_plan` in `merge.rs`, and the session-push and session-commit lane tests in `tests/session_push_cli.rs`. |
| Readers of submit progress: `broker status --json` (`in_flight_submits`, `submit.possibly-stalled` advice), stderr of `broker submit` (introduced 2026-10-02) | `status --json` gains an optional `in_flight_submits` array (see `docs/json-contracts.md`), read from per-submit records in `.aethyme/run/submits/` that each running submit writes and removes. While it waits, `submit` prints its place in line for a verification slot and the slot holders, names the holder of a gate owner lock it waits for (the holder now writes `<worker> since=<ms>` into the lock file), and prints `submit still running: <phase> for <t> (total <t>; last progress <t> ago)` after each heartbeat interval of silence (`AETHYME_GATE_HEARTBEAT_SECS`, default 30). All progress goes to stderr; `--json` stdout is unchanged. | Advisory only: nothing blocks on these fields. A wrapper that parses `submit` stderr sees new line shapes; the owner-lock line changed from `waiting for N owner lock(s)` (printed even when the locks were free) to one naming the holder, printed only while actually waiting. Covered by `aethyme-broker/tests/submit_progress_status.rs` and the `submit_progress`, `gates`, `verification` and `file_lock` unit tests. |
| Readers of `broker start --json` / `start --reuse --json` / `start --adopt --json` session guidance (introduced 2026-10-01) | Additive: `guidance` (array of strings), `guidance_source` (`default`, `repository` or `disabled`) and an optional `guidance_warning`. Configured by `[session] guidance` in `.aethyme/config.toml`, read from the committed default branch. `start-agent --json` does not include them. | None: new fields only. Covered by `aethyme-broker/tests/session_guidance_cli.rs`. |
| Readers of `broker start --json` / `start --reuse --json` / `start --adopt --json` standing authorizations, and of `preparation.authorized_by_policy` there and in `broker submit prepare status --json` (introduced 2026-11-17) | Additive: `standing_authorizations` (`prepare_session_worktrees` and `disposable_test_environments` booleans, `lines` array of strings), present whenever `guidance` is, even when the agreement is disabled; `preparation.authorized_by_policy` (bool) is true only when `next_action` is the prepare command and `[authorization] prepare_session_worktrees` grants it. Configured by `[authorization]` in `.aethyme/config.toml` (both keys default `true`), read from the committed default branch; an unreadable configuration grants nothing. Deployed `AGENTS.md`/`CLAUDE.md` carry the matching paragraph. | None: new fields only. Covered by `aethyme-enhance/src/authorization.rs`, `aethyme-broker/src/session_guidance.rs` tests and the `aethyme-cli` JSON shape snapshots. |
| Readers of the planned-path open-PR advisory: `broker start --json`, `start --reuse --json`, and `broker adopt --json` (including reuse forms; introduced 2026-10-21, #760) | When the report's `planned_explicit_leases` set is nonempty, output adds optional `planned_path_pr_overlaps` (`cache_available`, `overlaps[]` with PR number/URL/changed paths, `unknown_prs[]`, `listed_at_ms`, `checked_open_pr_limit`, `listing_may_be_truncated`, and optional `warning`). It is omitted when that effective planned-lease set is empty, including for a reused session with no prior planned claims. The listing and range cache are read-only; unknown changed-path ranges remain unknown; the advisory never scans Git history, calls GitHub, infers overlap from task text, or blocks the command. | JSON consumers must tolerate the absent/additive field and must not treat unavailable, unknown, or truncated evidence as no overlap or as a conflict result. Covered by `aethyme-broker/tests/pr_overlap_cli.rs`. |
| Readers of worktree placement: `broker start --json` (`worktree_placement`), `broker advanced worktree-root --json` (introduced 2026-10-01) | `worktree_placement.source` and `preferred_source` gain `repository_config` (a `[worktrees] root` in `.aethyme/config.toml`, read from the committed default branch like `[promote]`). The plan gains optional `host_state_fallback_root` and `preferred_unavailable_reason`. When the configured root is missing, unmounted, inside the repository, below `worktrees.min_free_bytes`, or the section is invalid, `source` is `host_state` with `fallback_reason` naming why. Cleanup items for a worktree on an unavailable configured root have `disposition` `inspection_failed`. | A wrapper that assumed worktrees always live under the host-state directory or `AETHYME_WORKTREE_ROOT` now also sees the configured root. Covered by `aethyme-broker/tests/worktree_root_config_cli.rs`. |
| Broker callers allocating linked worktrees (extended 2026-10-24, #335) | `[worktrees].max_checkouts` defaults to 32 registered linked checkouts across all roots; explicit zero disables it. Primary checkout excluded, prunable and manual registrations counted. New allocations at the limit refuse before root preparation or checkout allocation under the lifecycle claim. Reuse does not allocate and remains available. | This is an allocation guard, not deletion authority or a filesystem byte quota. Strict Git inventory failure refuses allocation. Existing work, primary checkouts and unregistered directories are never removed by this guard. |
| Gate commands that start containers or volumes, and operators reading `broker status doctor plan`/`apply` (introduced 2026-10-07, #287) | Every gate command now receives `AETHYME_GATE_RUN_ID` (a run id unique per gate run) and `AETHYME_GATE_LABELS` (`aethyme.gate=<gate> aethyme.session=<id> aethyme.repo=<repository key> aethyme.run=<run id>`, space-separated `key=value`; the session value is empty for a run with no session). A gate command that starts a container or volume should apply them, e.g. `docker run $(printf -- '--label %s ' $AETHYME_GATE_LABELS) ...`. The gate pidfile `.aethyme/run/gates/<session>-<gate>.pid` gains a fifth field, the run id (`-` when unknown); older four-field pidfiles still parse. `aethyme broker status doctor plan [--json]` lists every debris item across stores (`schema_version`, `digest`, `runtime` {`name`, `available`, `error`}, `items[]` with `id`, `kind`, `name`, `session_id`, `gate`, `run`, `owner` `dead\|live\|unknown`, `action` `remove\|report`, `reason`, `clear`; `removable_count`, `unavailable[]`, `paired_recovery`): every blocker-registry entry (report only), `stale_worktree` items (report only), and containers/volumes labelled with this repository's key from a Docker-compatible CLI (`AETHYME_CONTAINER_RUNTIME` names it, `none` disables it; default `docker` then `podman` from PATH). A resource's owner is `dead` only when its session's pidfile for that gate is gone, names an exited process, or records a different run id; `live` when it records the same run; `unknown` otherwise (missing labels, unrecorded session, unreadable or run-less pidfile). `aethyme broker status doctor apply --confirm <sha256> [--json]` rebuilds the plan, refuses with exit 3 and removes nothing when the digest differs, otherwise removes (`docker rm -f`, then `docker volume rm`) only `remove` items and records `broker.doctor.removed` per resource; a failed removal exits 1. The bare `status doctor` text output points to `doctor plan`. | Renaming the env variables or the label keys strands every gate command that applies them, and their containers become `unknown` and are never cleaned. Treating `unknown` as removable, or dropping the apply-time rebuild and digest check, can remove a container a running gate still uses. Dropping the pidfile run field makes every running gate's resources `unknown`. Another repository's resources (a different `aethyme.repo`) must stay out of this repository's plan. |
| Blocker registry: `broker status` advice readers, operator runbooks, and wrappers that clear a blocked repository (introduced 2026-09-23, #276/#281/#286) | `aethyme broker unblock [--json]` lists current repository blockers in one id namespace (`op:<n>`, `hostop:<32-hex>`, `resource:<lease-id>`, `gatecache:<gate>@<tree>`, `pidfile:<session>-<gate>`, `action:<session>`), and `aethyme broker unblock <id> [--outcome <succeeded\|failed>] [--reason <text>] [--confirm <generation>] [--json]` clears one. `broker status --json` gains an additive `blockers` array, and the `blockers.present` advice names `aethyme broker unblock <id>` as its commands. A refusal exits 3 and changes nothing. Since 2026-10-22, stale explicit path leases remain visible in lease/liveness views but are not repository-wide blockers: whether a claim is refused depends on the requested path and the shared `LeaseRefusalPolicy`. `unblock lease:<id>` remains available for targeted recovery and still refuses when the stale holder's worktree exists. Since 2026-10-06 (#286) an `op:` blocker may carry `inspect`, the read-only command that decides its outcome (`git ls-remote <remote> <refs>` for a push, `gh pr view <n> ...` for a `gh pr` write). When a write blocker (`op:`/`hostop:`) and a quarantined `resource:` lease are present together, the listing adds `paired_recovery` (`blockers`, write blockers first; `steps`: inspect the remote, then each write's `clear`, then each lease's `clear`), each half's `cause` names the other, and `blockers.present` uses those steps as its `commands`. A cleared `op:`, `hostop:` or `resource:` blocker's report adds `still_blocked_by` (ids of the halves still present, omitted when none) and says "not recovered yet" in `action`. Since 2026-10-06 (#549) a `gh pr merge` or `gh pr update-branch` write whose stderr carries a `GraphQL: <message> (<mutation>)` error naming that command's own mutation is recorded `failed` with `failure_class: github_refused` and `evidence.graphql_mutation`, not `outcome_unknown`; GraphQL server-failure wording stays unknown. | Renaming an id prefix or `broker unblock` strands the `clear` commands already printed into advice, logs and runbooks. Clearing an outcome-unknown operation without an operator outcome, or dropping the `--reason` audit on verdict invalidation, reintroduces the guessing and the hand-written SQL `DELETE` this surface replaces. Dropping the cross-naming or `still_blocked_by` makes clearing one half of a killed write look like a full recovery again; widening the GraphQL refusal beyond a command's own mutation would mark a write that may have landed as failed, which makes a blind retry look safe. |
| Agents keeping a session on the current default branch: `broker sync`, `broker start --json`, `broker start --reuse/--adopt --json` (introduced 2026-10-01) | `aethyme broker sync --session <id> [--json]` prints `session_id`, `branch`, `outcome` (`already_current`/`synced`/`conflict`), `strategy` (`rebase`/`merge`/`none`), `before`, `after`, `default_ref`, `default_commit`, `behind_before`, `ahead`, `conflicts`, `fetched`, optional `fetch_note` and `manual_commands`, and emits `broker.session.synced`; a conflict or an unsafe tree exits with the refusal code and changes nothing. Since 2026-10-04 a `synced` report also carries optional `next_action` (`aethyme broker push --session <id>`), and the human line names the direction (`merged origin/main into <branch>` / `rebased <branch> onto origin/main`) and says nothing was pushed: sync only moves the local session branch. `start_base` gains optional `fetched`, `fetch_error` and `cached_ref_age_seconds`; `start` now fetches exactly the default branch before choosing a base (a bounded read of one ref, made before the session exists), reversing #450's "start never fetches". Implicit creation and registered-session reuse now refuse a failed default-branch refresh before checkout allocation or movement; intentional explicit bases remain pinned and local-only repositories need no remote. The adopt/reuse JSON gains optional `default_branch` (the push comparison) and `default_branch_note`. Successful reuse of an existing registered broker session with a selected base via `start --reuse --json` adds optional `base_freshness` (`target_ref`, `target_commit`, optional `source`, `relation` (`current`, `target_contained` or `fast_forwarded`), optional `previous_observation_age_seconds`, `previous_observation_stale`, `max_age_seconds`, and local `observed_at_ms`). Unknown provenance omits `source`; the age flag is informational and never skips ref or ancestry checks. `[worktree_freshness].max_age_seconds` defaults to 900 and follows the existing repository config source rule (committed default-branch config when available, otherwise the main checkout file). | Generated agent instructions tell agents to run `sync` before resuming a reused session and when `push` reports the branch is behind, so renaming the verb or its outcome values silently stops agents from catching up. |
| Agents re-adopting a closed or replaced worktree: `broker start --adopt`/`--reuse --json`, `broker adopt --json` (introduced 2026-10-06, #294) | Additive: when the worktree's most recent closed session (or the stale session `--replace-stale` replaces) is on the same branch and still owns commits at the current `HEAD` that integration lacks, the new session starts from that session's ownership boundary (its accepted checkpoint, else its baseline) instead of `HEAD`, and the report adds `carried_ownership` (`from_session`, `baseline`, `pending_owned_commits`); human output prints an `Ownership: carried from session N` line. Omitted when nothing is pending, so `diff_base`/`adoption_base` then equal `HEAD` as before. `finish` on such a session reports the inherited commits as unsubmitted and recommends `submit`. | A caller that assumed a re-adopted session's `diff_base` is always its `HEAD` misreads ownership. Dropping the carry silently disowns unsubmitted commits again: submit supersedes the entry without running a gate, and finish calls the worktree safe to remove. |
| Agents publishing work as they go (introduced 2026-09-30) | `aethyme broker push --session <id> [--pr] --json` prints `session_id`, `branch`, `remote`, `pushed_oid`, `previous_remote_oid`, `commits_pushed`, `uncommitted_files`, `uncommitted` (`modified`, `untracked_entries`, `sample`: up to five paths; since 2026-09-30 an untracked directory counts as one entry, as `git status` shows it, and `uncommitted_files` is `modified + untracked_entries`), `pr` (`url`, `number`, `state`, `created`, and since 2026-10-03 `ci_skips_drafts`, present and true only when the pull request this push opened has a base-branch workflow gating on `github.event.pull_request.draft`; or null), `pr_overlaps` (`pr`, `url`, `files`, `conflicting_hunks`), `pr_overlaps_unknown` (PR numbers) and, since 2026-10-01, `default_branch` (`reference`, `commit`, `head`, `behind`, `ahead`, `would_conflict`, `conflicting_paths`, `suggested_command`, or null) and an optional `default_branch_note`; since 2026-11-12 an optional `default_branch_fetch_age_seconds`, present only when the push did not fetch the default branch: either the comparison reused a default-branch fetch any broker command of the repository made within the last 60 seconds (never with `--pr`; `AETHYME_PUSH_FETCH_REUSE=off` disables the reuse), or, also since 2026-11-12, the optional `default_branch_fetch_skipped_unmoved` is present and `true` because the remote read the push makes under the coordination lock before pushing (the same single `ls-remote`, carrying one more ref pattern) showed the remote default branch at exactly the commit the local tracking ref already held, so a fetch would have changed nothing; the age then counts from the start of that read. The skip applies only without `--pr`, only when the remote's fetch URL and its single push URL are the same string, and never when the read failed or the remote default branch differs from or is missing beside the tracking ref (a read that shows it moved fetches even when a recent fetch could be reused); `AETHYME_PUSH_FETCH_SKIP_UNMOVED=off` makes every push without `--pr` fetch unless it reuses a recent fetch. A skip records the read's start as the repository's last default-branch fetch time unless a newer fetch is recorded. The pre-push value journaled for unknown-outcome recovery is unchanged: the extra ref never enters the push plan; since 2026-11-12 also an optional `pr_lookup_listing_age_seconds`, present only when `[review] run_on_push` needed the session's pull request and this push took it (number, url, draft flag) from the open-PR listing its own overlap check had just fetched instead of a separate `gh pr list --head` lookup (never from a cached listing, never with `--pr`; absence from the listing still falls back to the lookup); and emits `broker.session.pushed`. Since 2026-10-03 the draft it opens takes the base branch's default pull request template (`.github/`, root or `docs/` `pull_request_template.md|txt`) filled from the commits' Problem/Decision/Rationale/Validation sections, and carries any `Contract decision:`/`Contract justification:` commit lines verbatim (ticking the matching template checkbox), with or without a template. `broker advanced git|gh` refusals for a missing `--destructive` or `--repo` append a `run instead:` line with the corrected command (`--repo` inferred from the targeted remote, else `<owner/name>`); the original reason text and exit code 3 are unchanged. `broker status --json` also adds `session.behind-main` advice rows (`warning` when merging the default branch would conflict, `info` when merely behind). In `[promote] mode = "verify-only"` repositories `broker submit --json` reports `verified_against` (`source` `upstream` or `integration`, `reference`, `commit`, optional `fallback_reason`). `broker status --json` adds `session.pr-overlap` advice rows (severity `warning` for conflicting hunks, `info` for the same files on different lines). It refuses (exit code for refusals) unless `[delivery] push_session_branches = true` is committed on the default branch. The library exposes `Broker::session_push_state(id)` (`branch`, `head_oid`, `remote_oid`, `unpushed_commits`, `oldest_unpushed_at_ms`) for status surfaces. | Generated agent instructions tell agents to run this after each commit, so renaming the command, the policy key or the JSON fields silently stops agents from publishing, and work piles up in worktrees again. Widening what it pushes beyond the session's own `agent/*` branch turns a durable repository authorization into one over shared refs. |
| Operators and automation relying on retained closed checkouts, and readers of auto-cleanup reporting (introduced 2026-10-07, #588) | The unattended sweep (`broker open` budget, its daily cadence, `aethyme broker gc sweep`) now **removes** a closed, broker-owned checkout and its proved session branch, without a reviewed digest, when four proofs hold and are re-checked under the GC lock immediately before removal: every owning session is closed; no process has a path open in it, no lease that still holds covers it and no gate runs; it has no tracked/staged/untracked change, no stash made on its branch, no rebase/merge/cherry-pick/revert/bisect in progress and no protected standalone ignored file; by default only recognized or explicitly configured output directories are disposable without inspecting their contents; unknown ignored directories are retained, while `[cleanup] strict = true` retains all ignored leftovers and disables automatic artifact sweeps; `.DS_Store` and recognized artifact files or links remain disposable only outside strict mode; and every commit is contained in the fetched remote default branch (`origin/HEAD`) by ancestry or the verbatim content/patch proof -- local branches and integration do not count. Containment depends only on HEAD and the remote tip the pass pinned, so the under-lock re-check reuses the selection's proof when HEAD is unchanged and proves it again when HEAD moved; once the pass's budget is spent, checkouts after the first are deferred to a later pass (#460). Anything undecidable keeps the checkout. `[cleanup] auto_remove = false` turns it off; `[cleanup] keep` pins worktrees; an invalid `[cleanup]` table turns it off and is reported. Each removal emits `broker.cleanup.auto_removed`. `gc sweep --json` adds `auto_cleanup`, `gc plan --json` adds a reporting-only `auto_cleanup` (the last pass, outside the digest), and `broker status` adds a `cleanup.auto-removal` advice row. The primary checkout and non-broker worktrees are never removed. | A workflow that kept a finished checkout around to inspect it later must pin it with `[cleanup] keep` or set `auto_remove = false`. Loosening any proof -- counting integration or a local branch as contained, treating an unknown lease or a missing `lsof` as idle, or skipping the under-lock revalidation -- deletes work that exists nowhere else. |
| Operators and automation resolving closed worktrees cleanup refuses (introduced 2026-09-30; plan schema v3 / archive schema v4 separates content confirmation from delivery evidence) | `aethyme broker finish cleanup resolve <session-id> --archive [--json]` prints a read-only plan (`schema_version`, `digest`, canonical `repository_path`, `branch_ref`, nullable `branch_tip`, `disposition`, `head`, `delivery_targets`, `landing`, `unlanded_commits`, patch sizes and SHA-256s, `untracked_files[]`, `ignored_paths[]` with policy classification, protected `ignored_files[]`, `ignored_policy_sha256`, `archive_root`, `apply_command`, `discard_command`); plan schema v3 uses the v5 content digest, binding repository, checkout, branch tip, preserved content, ignored policy, archive schema v4 and the bundle cap. Delivery observations are separate; `landing.state` is `not_evaluated_for_recovery`, not a delivery verdict, and recovery skips exact-content landing searches. Clean sessions without direct ancestry proof report `unproven_provenance`; squash/replay delivery is not inferred. Clean ancestry-proven sessions are directed to ordinary cleanup. `--confirm <sha256>` writes a verified recovery archive (`commits.bundle`, `staged.patch`, `unstaged.patch`, `untracked/`, protected standalone ignored files and symlinks under `ignored/`, `manifest.json`, `README.md`) under `<host state>/recovery-archives/<repository-key>/`, then rechecks the content digest under the lifecycle claim, proves omitted Git prerequisites remain reachable from current delivery targets, and verifies the archive against its original evidence snapshot immediately before removal. Advancing delivery refs alone need not invalidate confirmation; content or branch changes, lost history and unavailable evidence retain the checkout. Bundle stdout streams to an exclusive partial file under a hard 1 GiB cap; stderr and bundle verification/list-heads output are capped. Manifest v4 records `branch_tip` plus `bundle_bytes` and `bundle_sha256` when a bundle exists and requires both fields iff it exists. A cap or deadline refusal keeps the worktree and leaves the bounded partial. Known-minimum disk admission includes the cap when a bundle is needed plus the configured reserve, but excludes allocation overhead and concurrent writes. Current brokers read manifest v1/v2/v3/v4, retaining the frozen v2/v3/v4 digest serializations for legacy archives; brokers without v4 support retain new archives as unowned. Replan confirmations after upgrading. V1 remains readable without ignored-file inventory; v2 requires a valid `plan_digest`, `ignored_paths`, `ignored_files`, and `ignored_policy_sha256`. GC treats any archived ignored file as uncommitted work and refuses unknown or malformed manifest/archive contents. | Renaming archive files or manifest fields strands the restore steps written into existing archives. Ignored directories remain opaque and follow the current cleanup policy; strict-policy leftovers refuse archival. Removing the worktree before read-back and the final plan check, or failing to bind ignored files or bundle bytes to the archive identity, can lose data.
| Readers of `aethyme broker gc storage plan --json` branch inventory (introduced 2026-10-08, #335 phase, #630) | `schema_version` becomes 3. Each owned root gains `session_branch_inventory_complete` (bool) and, when non-empty, `session_branches[]` (`name`, `head`, `session_ids[]`, `uncleared_session_ids[]`, `checked_out_paths[]`, `ownership`: `session_ledger`, `unknown` for an `agent/` ref no ledger row names, or `shared` for the integration or default branch a ledger row names; `retention_reason`). `summary` gains `broker_branch_ref_count` (session_ledger only), `unclaimed_agent_branch_ref_count` and `incomplete_branch_inventory_root_count`. A root whose local refs could not be enumerated gets a blocker and proposes no candidate. The inventory is reporting only: it is not part of the plan digest and authorizes no removal. | A consumer that pins `schema_version == 2` must accept 3; the added fields are optional. Treating `unknown` as "orphaned", an absent `session_branches` on an incomplete root as "no branches", or `shared` as session-owned would let a later cleanup delete a ref the broker cannot vouch for. |
| Cleanup resolve planners, batch operators and restore tooling (introduced 2026-10-29) | Plans add `target_origin`, `recovery_archive_schema_version`, optional `minimum_archive_retention_ms`, optional `retirement_allow_adopted`, optional `retirement_witness_sha256`, nullable `checkout_branch_ref` / `checkout_branch_tip`, and `recovery_root_proofs[]` (`ref_name`, `object_id`, `available_from_delivery`). Optional retirement fields are omitted from ordinary plans. `branch_ref` / `branch_tip` remain the registered session identity; the checkout fields describe its current symbolic HEAD, or null for detached HEAD. An attached checkout tip equals `head`. Schema-5 retirement confirmations bind all root identities and their availability from the exact delivery snapshot; schema 6 uses the stable confirmation described below. | A branch mismatch does not permit dropping either root. Preserve explicit mode and root evidence when applying or rendering. Ordinary schema-4 content confirmations remain compatible; schema-5 retirement uses a separate confirmation domain. |
| Recovery archive readers, scanners and GC (introduced 2026-10-29) | Archive schemas 1–4 retain their prior interpretation. Schema 5 requires a hashed schema-2 `retirement.json` receipt with `archive_created_at_ms` equal to manifest `created_at_ms`, stable `content_identity_sha256`, delivery-target and root-proof hashes, `recovery_root_proofs[]` (the same root vector as the plan), current checkout branch identity, and immutable `minimum_archive_retention_ms` of at least 15 days. Commit bundles contain roots unavailable from the recorded delivery snapshot; roots already available remain represented in recovery metadata. GC protects each archive until its creation time plus its immutable minimum age, even after commits land or current policy is lowered. | Older strict readers refuse schema 5 and retain it as unsupported. A delivery-only change may restart an owned incomplete staging attempt only after fresh content/ownership identity validation; changed content or identity remains protected. Unavailable roots must remain verified in the bundle after source objects are pruned. Landing evidence or a shorter configured archive window cannot override its minimum age. Landing requires every recorded recovery root, not only checkout HEAD, to be represented on current delivery targets. Restore both the registered session and actual checkout identities; do not assume one branch tip represents them both. |
| Retirement archive writers and readers (extended 2026-10-29, schema 6) | New retirement archives use manifest schema 6 and retirement receipt schema 3. Confirmations bind preserved content, repository/session/path ownership, branch identities and the immutable retention floor; ordinary forward delivery-ref progress does not invalidate them. Original delivery targets and root proofs remain recorded. Before removal, every captured bundle prerequisite must still be covered by a current delivery target. Verified archives pin prerequisites under `refs/aethyme/recovery-prerequisites/v1/<session>/<confirmation>/<target-set-hash>/<index>` before checkout removal. Canonical final names are `<session>-<head12>-retirement-v6-<confirmation>`; renamed copies are not owned archives. | Schemas 1–5 retain their existing validation. Older readers treat schema 6 as unsupported and cannot reclaim it. Re-plan retirement confirmations after upgrade. Prerequisite pins keep incremental bundles recoverable in their owning repository; they do not make an exported bundle self-contained. GC releases exact expected pins only after complete archive removal and a complete bounded sibling-reference scan; uncertainty retains pins. A crash between archive removal and pin release can retain orphan pins. The minimum retention floor remains at least 15 days. |
| Spawned checkout ownership witnesses (schema 3, introduced 2026-11-02) | New macOS witnesses persist no-follow volume UUID observations for root, Git pointer, Git admin and common directory. Cohort and automatic-maintenance cross-remount comparison requires saved and current UUIDs plus identical inodes, canonical paths and original root-generation digest. Apply-time snapshot equality remains exact. Schema-2 records retain device/inode checks; no UUID is synthesized for an old record. | Older strict readers refuse schema 3 and must retain the checkout. A missing or different UUID, replaced object, changed path or generation remains a refusal. Linux and captures without stable UUIDs keep schema 2. Since 0.8.96 a renumbered schema-2 witness and a finish-cleanup recovery proof are judged by one ownership re-proof (see "One ownership re-proof for automatic removal, GC and finish"); a proof saved without UUIDs resumes through the device-renumber rule, except an orphan one, which stays kept for inspection. Schema-3 witnesses keep their UUID comparison. |
| Automatic checkout compaction maintenance (introduced 2026-10-29) | `[retention] compact_closed_worktrees` defaults to false. Opt-in permits a bounded background recovery attempt for a closed Spawned checkout with a complete current cohort and its original persisted owner witness. Archive retention, explicit keep, live ownership, pins and final identity checks remain required. The foreground maintenance budget does not admit archive creation. | No automatic adoption or ownership-witness synthesis. Legacy and adopted retirement requires exact reviewed operator targets, with `--allow-adopted` for adopted checkouts. A deferred or failed compact attempt is not successful checkout removal. |
| Operators retiring an exact reviewed checkout batch (introduced 2026-10-29) | `aethyme broker finish cleanup resolve <session-id>... --batch --archive [--allow-adopted] [--confirm <sha256>] [--json]` plans or applies 1–4096 unique positive IDs, sorted canonically. Schema v1 plans expose `repository_path`, `allow_adopted`, `digest`, `targets[]` (`session_id`, nullable `worktree_path`, nullable `target_origin`, nullable `confirmation`, `already_removed` (bool), nullable `preserved_file_bytes`, nullable `unlanded_commits`, nullable `blocker`) and `apply_command`. `already_removed` marks a fully cleaned ledger row with an absent path; it grants no deletion authority and does not increment `removed_count`. Recreated paths remain protected. Blocked inventories use null counts, never zero measurements. Byte estimates exclude commit bundle bytes. The batch digest binds repository, exact IDs, origin, path, per-target content confirmation and adopted opt-in; blocker prose is diagnostic. All target confirmations are rebuilt before the first archive or removal; each allowed item then uses the existing verified-archive apply path. Partial completion reports `results[]` (`session_id`, `already_removed` (bool), nullable `outcome`, nullable `error`), `removed_count`, `remaining_session_ids` and `plan_digest`, with nonzero CLI status when any target remains. No wildcard expansion occurs at apply time. `--batch` also supports a single remaining target; `--retire` selects compact single-target recovery with an item digest. Ordinary single-target resolve behavior remains compatible. | Partial completion is not full cleanup. Replan remaining exact IDs; do not reuse the old confirmation with a reduced list. Adopted targets require explicit `--allow-adopted` and retain their branches. Unsafe, active or uninspectable targets remain protected. Neither batch confirmation nor a missing ownership witness permits raw deletion. |
| Agents calling `aethyme broker advanced issues sub-issue` (#1221) | `sub-issue add|remove <parent> <child>... --repo <owner/name> --session <id> --reason <authorization> [--json]` resolves child issue numbers to GitHub database IDs before writing, then journals each native sub-issue mutation through the coordinator. The command requires a session, a repository matching the checkout origin, and explicit authorization; it reports the number of completed mutations if a later mutation fails. JSON success is an array of `{parent, child, operation}`. | Additive CLI. This helper does not provide issue-set manifests, sessionless policy, or automatic retries. |
| Automations and wrappers calling `aethyme broker advanced issues upsert`, repositories writing `[automation]` in `.aethyme/config.toml`, and anyone reading issues carrying `<!-- aethyme-outcome:<fingerprint> -->` (introduced 2026-11-01, #1238) | `issues upsert --repo <owner/name> --fingerprint <key> --title <t> --body-file <f> [--label <l>]... [--session <id>] [--json]` keeps one issue per outcome: with no open issue whose body holds the exact marker it opens one (body redacted, cut at 16 KiB, marker appended; title single-line, 200 chars); with one open it adds a comment (cut at 4 KiB) at most once per `issue_comment_interval_minutes`; `--resolve [--body-file <f>]` comments and closes the open issue, or does nothing. `--json` prints `aethyme.outcome-issue/v1`: `repository`, `fingerprint`, `action` (`created`, `commented`, `rate_limited`, `resolved`, `nothing_to_resolve`, `refused`), `issue_number`, `issue_url`, `code` (with `refused`), `message`, `operation_ids` (with `--session`). Policy, read from `.aethyme/config.toml` at the default branch's remote-tracking commit only: `[automation] issues = "allow" | "deny"` (default and absent: deny), `issue_labels` (the only labels allowed; default none), `issues_per_day` (opens plus comments per UTC day, default 10; resolving is never counted) and `issue_comment_interval_minutes` (default 360); an unknown key or other value is an error. Refusals write nothing to GitHub, are recorded locally and exit 3: `automation_issues_denied`, `label_not_allowed`, `daily_budget_exhausted`, `reconciliation_pending` (an earlier create's outcome is unknown and its issue is not yet visible; after 15 minutes without it a new create is allowed). Errors exit 1 with `invalid_fingerprint` (1-128 of `A-Za-z0-9._:/-`), `invalid_repository`, `invalid_request`, `invalid_automation_policy`, `github_failed`, `io`. The journal is `.aethyme/run/automation-issues.json` (schema 1, under `automation-issues.lock`); every write is preceded by a pending entry there, and an interrupted create is adopted by marker on the next run, never repeated blindly. With `--session` writes run through the coordinated GitHub operation layer (scopes `issues/outcome/create`, `issues/<n>/outcome/comment|close`); without it the broker runs `gh` itself. Library entry points: `upsert_outcome_issue` and `upsert_outcome_issue_for_repository`. | Additive. The marker text is the identity: never edit it out of an issue body. Covered by `aethyme-broker` `outcome_issues::tests` and the `broker advanced issues` help snapshot. |
| Automations calling `aethyme broker advanced outcomes report`, repositories writing `[[automation.outcomes]]` in `.aethyme/config.toml`, and readers of `status doctor --json` `automation_issue_refusals` (introduced 2026-11-01, #1238) | Rules, read with the `[automation]` policy from the default branch's remote-tracking commit: `[[automation.outcomes]]` with `source = "<kind>:<name>"` (at most 64 chars, lowercase letters, digits, `.`, `_`, `-`; unique), `issue` (default true), `labels`, `notify_owners` (default false) and `paths` (repository-relative globs); an unknown key, bad source, duplicate or absolute path makes the policy invalid. `outcomes report --source <s> --failed --title <t> [--body-file <f>] [--key <k>] [--commits <from>..<to>] [--session <id>] [--json]`: with `notify_owners` and `--commits` it resolves owners as `broker advanced owners` does, narrowed to `paths`; with `issue` and a GitHub `origin` it upserts the issue for fingerprint `outcome:<s>[:<k>]` (`<k>` 1-64 of `A-Za-z0-9._-`) with the body (or title) plus a `### Who to ask` list naming each owner's session, branch, pull request and `live, notified` or `ended`, and the rule's labels; then it sends each live owner the automation note `<title> — tracked in <url>` (or `— no issue filed (<code>)`) from `automation:<kind>.<name>` with the fingerprint, so a repeat while unread is not sent again. `--recovered [--body-file <f>] [--key <k>]` resolves the issue. `--json` prints `aethyme.outcome-report/v1`: `source`, `state` (`failed`/`recovered`), `fingerprint`, `issue` (the `aethyme.outcome-issue/v1` result, when the upsert ran), `owners` (as in `owners --json`), `actions[]` with `kind` (`owners`, `issue`, `note`, `resolve`), `status` (`done`, `deduplicated`, `skipped`, `refused`, `failed`), optional `target`, `code` (`no_github_origin`, `no_commits`, refusal codes) and `message`. Exit 0 when no action was refused or failed, 3 after printing the report otherwise; exit 3 `no_outcome_rule` for an undeclared source, 1 for an invalid policy or request. Each report appends one `automation.outcome.reported` event with source, state, fingerprint and action kinds, statuses and codes, never text. `status doctor --json` gains `automation_issue_refusals[]` (`at_ms`, `repository`, `fingerprint`, `code`), the refusals the local issue journal keeps (the latest 20), never counted against `healthy`. Budget outcomes (#1239) use the same rules (2026-11-02): when the default branch declares `source = "budget:environments"`, each sustained breach and recovery of `[budgets.outcomes]` is reported through it with the metric as the key (fingerprint `outcome:budget:environments:<metric>`, or a sanitized prefix plus a 12-hex digest for a metric name that is not already 1-64 of `A-Z a-z 0-9 . _ -`), the rule's labels instead of `[budgets.outcomes] labels`, no commit range, and an `automation.outcome.reported` event; the rule alone opts in. Without the rule, `issue = true` keeps writing directly under `env-budget:<repository>:<metric>`. `env budgets --act --json` `outcomes.actions[].fingerprint` names whichever was used. |
| Automations and wrappers calling `aethyme broker advanced owners` to find whom to tell about a failure (introduced 2026-11-01, #1238) | `owners --commits <from>..<to> [--path <glob>]... [--json]` is read-only and local. A single commit `<c>` reads as `<c>^..<c>`; `...` ranges, option-like endpoints, unknown revisions and absolute `--path` values exit 1 with `invalid owners request`. `--path` globs (`:(glob)` pathspecs, repository-relative) narrow the commits; at most the newest 200 are resolved (`truncated`). Each commit is traced to the first-parent commit of `<to>` that landed it; a `Merge pull request #N from <owner>/<branch>` subject gives the pull request and branch, a `subject (#N)` squash gives the pull request. Sessions come from the recorded pull request links for the origin's GitHub `owner/name`, else every session that used the branch, finished ones included. `--json` prints `range`, `paths`, `repository`, `commits[]` (`sha`, `subject`, `landing`, `pull_request`, `branch`, `session_ids`), `owners[]` (`session_id`, `status` as `broker advanced agents` derives it, `live` = neither exited nor closed, `branch`, `pull_request`, `task`, `agent_identity`, `commits`), `unowned` and `truncated`. |
| Automations and wrappers calling `aethyme broker advanced note send --from automation:<name>`, and readers of `note list --json` (introduced 2026-11-01, #1238) | `note send --from automation:<name> --to-session <id> --message <text> [--fingerprint <key>] [--json]` needs no session and refuses `--session` alongside `--from`; `--fingerprint` is refused without `--from`. The name is 1-64 of `[a-z0-9._-]`, starting with a letter or digit; the message is one trimmed line of at most 1,000 bytes; the recipient must exist and not be closed (exit 1 otherwise). `--json` prints `note` (`id`, `sender`, `recipient_session_id`, `message`, `fingerprint`, `created_at`, `acknowledged_at`) and `deduplicated`, true when an unread note from the same sender with the same fingerprint already existed and was returned instead. `note ack --session <recipient> --automation-id <id>` acknowledges one; `--id` and `--automation-id` together exit 1. `note list --json` gains `automation_notes` (newest first), and `unread_count` now counts unread session and automation notes together. Storage is the additive broker table `automation_notes` (schema v61, compatibility floor unchanged at v60: older binaries neither show nor acknowledge these notes). |
| Readers of `broker advanced resources pressure --json` and `broker status doctor --json` watching shared preparation-cache rotation recovery, and `submit prepare` error readers (introduced 2026-11-02, #1236) | `resources pressure` always includes `preparation_rotation_recovery[]`, an empty array when no recovery is recorded. `doctor` adds the same array, omitted when empty, and prints one `preparation-cache rotation recovery <state>: ...` line per record. The array is never counted against `doctor` health. Each record has `schema` (`aethyme.preparation-rotation-recovery/v1`), `cache_root`, `repository` (64-hex store name), nullable `retired_generation` (the retired store name), `state` (one of `in_progress`, `stalled` or `complete`; an `in_progress` record not updated for 120 s reads as `stalled`), `entries_unlinked_total` (summed across attempts of the same generation), `attempts`, nullable `remaining_entries` (a bounded count taken when an attempt stops: `0` once complete, null when not counted), `remaining_entries_is_lower_bound`, nullable `last_progress_at_ms`, `updated_at_ms` and nullable `last_stop_reason`. Completed records are omitted after 24 h. `submit prepare` writes the records atomically under `<host state>/preparation-rotation-recovery/` beside the host resource database, after each recovery slice and when an attempt ends. Readers only read them, at most 64 files of 16 KiB each, and never create state or walk a cache store; a missing or malformed record reads as no recovery recorded. Prepare-time recovery now runs in 5 s slices: it continues while each slice unlinks entries, renews the lease between slices, and stops on a slice with no progress, a non-time failure, or a 600 s ceiling. Its `StorageUnavailable` reason now reads `shared-cache rotation recovery is incomplete after unlinking <n> filesystem entries in <k> recovery slice(s) over <s>s: <cause>; progress is kept between attempts (...); <remaining>`. |
| Readers of `broker advanced resources pressure --json` and of the pressure advice attached to a final resource-contention refusal (introduced 2026-11-16) | Each `groups[]` entry adds `agent_tree_footprint_bytes`: the sum over the group's process tree of each process's physical footprint (private memory whether resident, compressed or swapped out), or null when the platform has no such ledger or any process in the tree cannot be read. Groups are now ranked by this figure first, then by `agent_tree_rss_bytes`, then by oldest inactivity. The schema stays `aethyme.resource-pressure/v1`; every existing field keeps its name, type and meaning. See `resource-pressure-diagnostics.md`. |

| Operators and automation expiring recovery archives (introduced 2026-09-30; bounded manual planning) | `aethyme broker gc plan --json` adds `recovery_archives[]` (`path`, `session_id`, `head`, `created_at_ms`, `landed`, `estimated_bytes`, `reason`) to the digest-bound candidates and a reporting-only `recovery_archive_inventory` (`root`, `count`, `estimated_bytes`, `oldest_created_at_ms`, `unowned[]`); both are omitted when empty. `gc apply --json` reports `recovery_archives_removed[]` and emits `broker.gc.recovery-archive-removed` per archive. `gc storage plan --json` adds a reporting-only `recovery_archives[]` (`path`, `archive_count`, `broker_archive_count`, `estimated_bytes`, `oldest_created_at_ms`). Retention key `recovery_archive_days` (default 30, `0` keeps all). Default `gc plan` and fresh `gc apply` planning use one shared cooperative 10-second budget; a deferred inventory produces no apply digest. Any nonempty recovery-archive directory is deferred by default because verification may hash a bundle up to 1 GiB. `gc plan --full-scan` opts into the existing verification path without the default time and artifact-discovery entry budgets; artifact discovery still stops at six directory levels. The emitted `gc apply` command repeats that option. Apply with an existing journal resumes that authorized journal without re-planning. | Proposing an archive the broker cannot vouch for, or one holding uncommitted changes before its retention period, deletes work that exists nowhere else. Treating a partial/default-deferred inventory as complete could omit archive candidates from a review; adding candidates to the digest when there are none would invalidate every reviewed plan. |
| `.github/workflows/release.yml` (tag `v*` push), `.github/workflows/homebrew-tap.yml` and `scripts/publish-homebrew-tap.sh` | The release job builds both binaries, signs the release manifest with the tag workflow identity, and attaches the manifest, formula, checksums, installer and archives. For stable tags it calls `homebrew-tap.yml` (also `workflow_dispatch` with input `tag`, the retry path), which verifies that signature, regenerates `aethyme.rb` with `--tag` and `--source-sha` bound to the tag's commit, byte-compares it to the release asset, and runs the publisher. The publisher validates the release URLs and digests (a `version` line is optional), then makes one Contents API `PUT` carrying the formula's current blob SHA and reads the formula back byte-for-byte; an identical formula is a no-op, and writes are refused outside GitHub Actions. Pull requests touching these files rehearse against the latest stable release and the live tap with `--dry-run` and the read-only job token. | Dropping the blob-SHA precondition lets a stale run overwrite a newer formula; dropping the read-back reports success for bytes nobody checked. Renaming the workflow, its `tag` input or the script breaks the documented retry. Routing the write through a broker session on the runner adds nothing (its database is empty) and fails on the session's repository check. |
| Root `install.sh`, published as a versioned and `releases/latest` asset | Discovers stable v0.2+ releases through `release-manifest.json`; maps macOS/Linux host architecture to a supported target; downloads the exact versioned paired archive and checksum; validates checksum, archive members, and both binary versions; stages both executables under `~/.local/bin` by default. `--verify-signature` additionally verifies the Sigstore bundle and binds the reviewed installer file to its signed digest. | Renaming manifest fields/assets, either archive member, the GitHub workflow identity, or the stable channel breaks installation. The offline `release_installer` fixture covers successful update, bad archive checksum, matching signed installer, and tampered installer refusal. |
| `aethyme update` / `aethyme self-update`, and readers of `broker status --json` (introduced 2026-10-06, #293/#251; extended 2026-10-18, #846) | `aethyme self-update [--version X.Y.Z]` is `aethyme update apply`: it plans and executes in one step. `update plan` and `apply` accept `--version X.Y.Z` (that release's own manifest; a mismatch is refused). `update execute` and `apply` verify the release manifest's Sigstore bundle (`release-manifest.sigstore.json` beside the manifest, identity `release.yml@refs/tags/vX.Y.Z`) whenever `cosign` is on PATH; `--require-signature`/`--verify-signature` fail without it and `--no-verify-signature` skips it, as in `install.sh`. The execution report adds `signature_verification` (`verified`, `skipped_cosign_missing`, `disabled`, or `installer` for `update bootstrap`). `start`/`start --adopt` record a `session.install_recorded` event; `status --json` adds an `install.replaced` advice row (severity `warning`) for each live session whose recorded build differs from the build running `status` (by version and exact full revision, including tracked dirty state, when both builds use the new format; by commit when either side has legacy describe metadata and both commits are known; otherwise by package version and legacy `describe` only), with the old and new build in `evidence`. The new `revision` field and CLI `build_revision` expose the full HEAD OID with optional `-dirty`; `describe` remains as a compatibility alias. | A wrapper parsing the update report must tolerate the new field. A signature failure exits non-zero before the archive is downloaded and leaves the active pair untouched; a pair whose binaries disagree is refused while staging, before anything is switched. `install.replaced` is advice only: nothing is locked or refused when the pair changes. Renaming the event kind or advice id breaks status readers. |
| Agents and wrappers reading a submission's verdict: `broker submit` exit codes and `--json` `gate_verification.status`, `broker status --json` advice ids, and `broker finish` warnings (introduced 2026-10-01) | A submission is **deferred**, not rejected, when every gate that did not pass was stopped by a host fault the broker observed itself (`gate_outcomes[].host_fault: true`): `gate_verification.status` is `deferred`, the queue entry stays `submitted` with `"deferred": true` in `details_json`, nothing is promoted, the command exits 6, `broker status` raises `session.latest-submit-deferred` (reason `submit_deferred`, severity `blocked`), and `finish` says to free the resource and resubmit rather than to wait. A genuine failure anywhere is still a rejection, text in a gate's own log never defers, and a second broker timeout on the same tree is a verdict. | A wrapper that treated a non-`failed` verification as success, or a `submitted` entry as in flight, must now check `deferred`. Deferring on log text again would let a change whose test prints "timed out" be deferred forever; the merge, gates and status tests pin each rule. |
| Agents and wrappers reading `aethyme collab <command> [--json]` (introduced 2026-10-10, #680, experimental) | A new top-level command group: `status`, `enroll [--write]`, `capture recover\|abort\|receipt`, `gc plan\|apply --confirm <digest>\|resume`, `context --path ...`, `brief attach`. Each `--json` output is one object with a versioned `schema` (`aethyme.collab-<command>/experimental-v0`); refusals and failures are `aethyme.collab-error/experimental-v0` with `code` and `next_action` on stdout; see `docs/json-contracts.md`. Disabled unless `[collaboration] capture` is `advisory` or `required` with a valid `project`: then `status` reports `enabled: false` and every subcommand but `status` and `enroll` exits 3 with `collaboration_disabled`. Under `capture = "required"`, any `[collaboration]` key other than `capture` and `project` is refused as `unknown_setting`, for `submit` as well; under `advisory` it is ignored and reported as `ignored_settings` (in `submit --json`'s `collaboration_capture` and in `collab status`), so it never stops an advisory submit; without capture it changes nothing. `status` writes nothing; only `broker submit` creates state, and other subcommands refuse with `not_initialized` until it exists. `enroll --write` edits only the current worktree's config. Integrity failures exit 1. On Windows `collab --json` prints the error object with `unsupported_platform`, exit 3. No subcommand runs in the background or contacts a service. Windows reports the broker as unavailable. | Experimental: the field sets may change while the schemas say `experimental-v0`; check `schema` first. A wrapper must treat exit 3 as "refused, read `code` and `next_action`", not as failure to retry. Covered by `aethyme-cli/tests/collab_cli.rs`. |
| Agents, wrappers and repositories using `aethyme env` and `.aethyme/environments.toml` (introduced 2026-10-29, #1209) | A new top-level command group. `env validate [--json]` checks `.aethyme/environments.toml` (schema 1) statically and exits 0 when valid, 4 with every diagnostic otherwise; `env plan [--profile <name>] [--json]` compiles a profile and prints its start order, snapshot digest, the reservation and capacity `HostResourceRequest`s it would submit, and the host-resource coordinator's advisory plan for each (`null` with `host_state: "absent"` when no host resource database exists). Output objects carry `schema` `aethyme.env-validate/v1` / `aethyme.env-plan/v1`; refusals are `aethyme.env-error/v1` with `code` and `next_action` (missing file: exit 3 `not_configured`). Diagnostics have stable `code`s plus `file`, optional `line`/`column`, `profile`, `component`, `field`, `message`, `fix`. Host pool limits are read from `<host state>/environments.toml`. Both commands are read-only: nothing is spawned or written. Request ids are `<env_id>:reservation` / `<env_id>:capacity`; lease pools are `env:<repository key>:<pool>` or `env:host:<pool>`. `.aethyme/environments.toml` is classified as gate policy. #1222 adds `env status [<id>]`, `env list [--all]`, `env history <id> [--limit N]`, `env logs <id> [<component>] [--tail N]` (schemas `aethyme.env-status|list|history|logs/v1`, read-only; a host with no `environments.db` reads as empty, unknown ids exit 3 `not_found`) and `env supervise [--ensure]` (`aethyme.env-supervise/v1`). Host files under the host state directory: `environments.db` (own `meta.schema_version`, 1; a newer version is refused with `schema_too_new`, it never touches `host-resources.db`), and `environments/` holding `supervisor.lock`, `spawn.lock`, `supervisor.json` (`identity {pid, pgid, start_time}`, `started_at_ms`, `version`), `supervisor.log` and `<env_id>/<component>.log` (rotated to `.log.1` past 8 MiB). #1210 fixes the contract between Aethyme and repository commands: every command runs as `/bin/sh -c` from the worktree root with a cleared environment (only `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `SHELL`, `TERM`, the component's `pass_env` names, its literal `env`, and `AETHYME_ENV_ID|NAMESPACE|PROFILE|WORKTREE|COMPONENT|HOOK_ATTEMPT`, `AETHYME_ENV_PORT_<C>`, `AETHYME_ENV_URL_<C>`, `AETHYME_ENV_RECEIPT_<C>`); a hook passes data on by printing one `AETHYME_RECEIPT {json object}` line (last wins, max 16 KiB); hooks run in their own process group under a 600 s deadline, or a component's positive `hook_timeout_seconds`, and a stopped hook's partial output is kept in its log; `process` components run in their own group and are stopped with SIGTERM, `stop_grace_seconds`, then SIGKILL, only when their recorded `(pid, pgid, start_time)` still matches. Containers started by `compose` / `container` carry labels `aethyme.repo`, `aethyme.env`, `aethyme.component` (compose via a generated override file), so `broker doctor plan` finds leaks. Engine order: component `engine` / `[engine] default`, then `AETHYME_CONTAINER_RUNTIME`, then `docker`, `podman`; compose uses `<engine> compose` when the plugin answers, else standalone `docker-compose` / `podman-compose`. #1211 adds the lifecycle: `env up [--profile P] [--wait S] [--reconfigure]` (`aethyme.env-up/v1`: `env_id`, `phase`, `namespace`, `ports`, `variables`, `last_error`), `env down <id>` and `env destroy <id> --retry | --abandon --reason <text>` (`aethyme.env-down/v1`, `abandoned`), `env sweep` (`aethyme.env-sweep/v1`, `acted_on`). Error codes: `reconfigure_required`, `wrong_phase`, `capacity_timeout`, `not_found` (exit 3); driver codes such as `hook_failed`, `unhealthy` (exit 1). An environment holds a reservation lease (request id `<env_id>:reservation:<n>`, namespace + ports) for its life and a capacity lease (`<env_id>:capacity:<n>`) while running; grants live in `environments/<env_id>/{reservation,capacity}.grant.json` (0600). Leases are released only after every destroy succeeded; a failure leaves the environment `destroy_failed` with leases held. `broker advanced resources release|reconcile` now refuse a lease an environment owns with an `environment_owned` message naming `aethyme env down|destroy`. Every broker worktree removal (`git::worktree_remove_registered`: finish, cleanup, auto-cleanup) first destroys that worktree's environments and fails with the reason, keeping the worktree, when it cannot. `environments.db` schema 2 adds `main_root`; the sweep destroys environments whose worktree is gone from a `git archive` export of the recorded commit and retries failed destroys up to 5 times. #1212 adds `env park <id>` (`aethyme.env-park/v1`) and `env wake <id> [--wait S]` (`aethyme.env-wake/v1`); `env up` on a parked environment wakes it. Since #1294 `env restart <id> [<component>]` (`aethyme.env-restart/v1`: the `env status` fields plus `restarted[]`, in start order) stops and starts one component, or every running one in dependency order, under the environment's operation lock, keeping capacity, reservation, ports, namespace, data and every other component, and re-checks health; it records a `restarted` note with `components`. Only an active environment restarts: otherwise exit 3 `not_active`; an unknown component is exit 3 `not_found`. A stop that cannot be proven quarantines the environment (`restart stop unproven`); a failed start or health check leaves it active with `last_error` naming the component and a `restart failed` note. Park stops every component (proven) and releases only the capacity lease, keeping namespace, ports and provisioned data; an unprovable stop goes to `quarantined` and keeps capacity. Wake acquires capacity first: without it the environment stays `parked` with `last_error.code = capacity_timeout` (exit 3); a failed start or health check rolls back to `parked`; a reserved port taken by another process fails with `port_in_use: port N`. Outputs add `parked_at`, `units_to_wake` and `pool`. Capacity request ids are `<env_id>:capacity:<phase_generation>`, one per wake. #1213 adds `env exec <id> -- <command>` (stdio passed through; exit status is the command's; refusals are one `aethyme.env-error/v1` JSON line on stderr; wakes a parked environment; injects `AETHYME_ENV_ID`, `AETHYME_ENV_NAMESPACE`, `AETHYME_ENV_PORT_<C>`, `AETHYME_ENV_URL_<C>`; the command keeps the caller's process group so ^C reaches it), `env touch <id>` (`aethyme.env-touch/v1`), `env pin <id> --for <60..86400> --reason <text>` and `env unpin <id>` (`aethyme.env-pin/v1`). Activity is the mtime of `environments/<env_id>/activity` (no database lock) and `last_activity_at`; `env exec` holds `environments/<env_id>/busy/<pid>-<start_time>` while its command runs (live exactly while that pid runs with that start time; unreadable markers count as busy for at most 24 h). A component `activity_probe` runs (5 s timeout) only when every cheaper signal says idle; exit 0, a timeout or a failure to run counts as active. The supervisor parks an `active`, unpinned, non-busy environment idle for longer than its `idle_seconds`, recording `parked after idle`; pins and unpins are recorded as same-phase transitions. #1214 adds the wake-on-request proxy, served by the supervisor on `127.0.0.1:4180` (`AETHYME_ENV_PROXY_PORT`), routing by `Host`: `<component>.<namespace>.<route_domain>` and `<namespace>.<route_domain>` for the first routed component. Active routes forward to the reserved port (bytes spliced both ways after the head, so WebSockets work); a parked environment is woken and the request held, else `503` with `Retry-After` and JSON naming the environment and `aethyme env logs`; `static` components are served from `serve_dir` (no `..`, no symlink escape). Control: `GET /__aethyme/routes` (`aethyme.env-routes/v1`, `ETag`/`If-None-Match` -> `304`) and `POST /__aethyme/wake/<namespace>` (`Authorization: Bearer` + `environments/proxy.token`, owner-only; `401` without it, `404` for an unknown namespace, never creates). `env routes [--json]` prints the table, the proxy address and the token path. See `docs/guides/environments-proxy.md` (Caddy reference). #1215 adds pool keys `preempt = "none" | "park_lru"` (default `none`, unchanged behaviour) and `min_active_seconds` (default 120), and profile `priority` (default 0). With `park_lru`, `env up` and `env wake` park the least-recently-active eligible environments of the same lease pool before acquiring capacity, only if together they free the whole shortfall; pinned, busy, ref-owned, non-active, recently started and higher-priority environments are never victims. Victims are parked (never destroyed) with transition reason `preempted` plus a same-phase note carrying `beneficiary`, `shortfall_units` and `pool`; the beneficiary gets a `preempted others` note listing `victims`. #1272 mirrors each preemption into the broker events of every repository involved (each victim's and the beneficiary's recorded main root, once each) as kind `environment.preempted` with payload {`beneficiary`, `beneficiary_repository`, `victims[]` {`env_id`, `repository`, `units`}, `pool`, `shortfall_units`}; a repository whose broker database is missing or at another schema is skipped with a warning. #1216 adds `env infer [--range <rev-range>]` (`aethyme.env-infer/v1`: `base`, `inference` {`minimum`, `rank`, `fired[]` {`rule`, `min_profile`, `reason`, `paths`}, `unmatched`, `changed`} or `report` {`commits`, `profiles`, `top_rule`}). `[[rules]]` globs use `globset` with literal separators over the diff against the merge base with `aethyme/integration` (else `origin/HEAD`/`main`/`master`), uncommitted and untracked files, and both sides of renames; Git output is read with `-z`, so non-ASCII paths match verbatim, and `--range` compares a merge with its first parent. With no base at all, only uncommitted changes against `HEAD` count and `env up` adds `inference_warning`. `env up` without `--profile` uses the inferred minimum; a lower `--profile` exits 3 `below_inferred_minimum` naming the fired rules; a higher one exits 2 `reason_required` unless `--reason` is given, which is recorded as a `profile above inferred minimum` note; without `--profile`, an environment of this worktree that already meets the minimum is reused rather than a cheaper one started beside it. Only after the new environment is up are this worktree's environments ranked below the minimum destroyed and listed in `replaced`; one that is pinned, busy (`env exec` running) or mid-transition, or whose `down` fails, is kept and listed in `kept_below_minimum` as `[env_id, reason]`. `env up` output gains `inference`; `env status` gains `inferred_minimum` and `below_inferred_minimum`. #1273: `broker status` adds a warning advice row `environment.below_inferred_minimum` (with `session_id`, the fired rules as `evidence`, and `aethyme env up --profile <minimum>` as the command) for each live session whose best live environment ranks below the minimum its changes infer; only sessions with a live environment are inferred, and when the status budget is spent the check is listed in `deferred_checks` as `environment_inference`. #1217 adds `env shared up|redeploy|destroy <name> [--force]` and `env shared list` (`aethyme.env-shared/v1`; redeploy `result.outcome` is `unchanged`, `redeployed`, `rolled_back`, `deferred` or `in_progress` (another redeploy of the environment holds its exclusive key); `rolled_back` carries `serving`, which is `null` when the previous revision did not restart. A `[[shared]]` environment is owned by `ref:<name>@<ref>` (`ref` = `default_branch`, `integration` or a ref), runs from a `git archive` export under `environments/shared/`, and is never destroyed by session or worktree cleanup; only `env shared destroy` or its removal from the default branch's `environments.toml` destroys it. Redeploy is hold-and-swap: an `environments/<env_id>/swap.in-progress` marker makes the proxy hold requests, the environment is parked, its snapshot swapped in place (same reservation, namespace, ports) and woken; the new revision's `build` hooks run first in its own export while the old one serves, and a failed build parks nothing (note `redeploy build failed`); a revision whose component set changed is recreated rather than swapped; a failed new revision rolls back, recording `last_error.code = redeploy_failed` with `failed_commit` and `serving`. The swap marker records its owner (pid and start time) and holds requests only while that process lives. The supervisor destroys a shared environment only when the default branch lacks the environments file or the `[[shared]]` entry; an unreadable or unparsable file skips the tick, and a changed `profile` replaces the environment. The supervisor redeploys when the ref moves, at most every `min_redeploy_interval_seconds` (default 300). Profiles with `uses_shared` get `AETHYME_SHARED_<NAME>_URL` and `AETHYME_SHARED_<NAME>_PORT_<C>` in every component, and a shared environment is not idle while a consumer is active. Since #1274 `AETHYME_SHARED_<NAME>_URL` goes through the wake-on-request proxy, via the stable alias host `<name>.<repo12>.shared.<route_domain>` (written to the serving slot's `route-alias` file) when it resolves to loopback, and `AETHYME_SHARED_<NAME>_DIRECT_URL` keeps the raw address; waking a parked consumer first brings its shared environments up. A redeploy is blue/green when the pool has room: the second slot `<owner>#b` comes up beside the serving one, the alias and the `serving` pointer under `environments/shared/` move to it, and the old slot is destroyed, never parked (`redeployed` note `strategy: blue_green`); when the pool is full it falls back to the in-place hold-and-swap. A `[[shared]]` entry with `stateful = true` (a database, whose data a second slot would not have) is never redeployed blue/green and always holds and swaps in place. Since #1297 a `[[shared]]` entry may set `redeploy_on = "inputs"` with `inputs` globs (default `"ref"`, unchanged behaviour; `missing_inputs` without them, `invalid_glob` for a bad one): a ref move redeploys only when the digest of the files matching `inputs` plus `.aethyme/environments.toml` (path, mode and blob id, read from the commit's tree) differs from the serving commit's; otherwise nothing restarts, `env shared redeploy` reports `unchanged` with the new commit, the revision is written to `environments/<env_id>/inputs-revision` and a `revision recorded` note carries `serving`, `revision` and `reason: "inputs unchanged"`. The supervisor runs the shared sync on its own thread, so a slow build never delays a tick, and the sync retires an idle non-serving slot. The proxy now also holds requests while an environment is stopping, waking or starting. Since #1296 a routed component may declare `paths = [{ prefix, to }]` (`to`: another routed component of the profile, `shared:<name>` from `uses_shared`, or `http://host[:port]`; validated as `paths_without_route`, `invalid_path_prefix`, `duplicate_path_prefix`, `invalid_path_target`): the proxy sends a request under the longest matching prefix (the prefix itself or followed by `/`, `?` or `#`) to that target, prefix kept, waking a parked target like a host route and rewriting `Host` only for `http://` targets; `env routes` / `GET /__aethyme/routes` route entries gain `paths[]` {`prefix`, `host` | `url`}, omitted when empty, and compiled snapshots gain `paths` only when non-empty, so existing digests are unchanged. #1218 adds resource accounting: the supervisor samples active environments every 60 s (one bounded `ps -A -o pid=,pgid=,rss=,%cpu=` snapshot per tick, summed per process group: method `ps_rss`; `<engine> stats --no-stream` for containers: `engine_stats`; `none` when nothing runs) into `environments.db` table `environment_samples` (last 200 per repository and profile; additive, store schema unchanged). `env list --json` environments gain `memory` (`bytes`, `cpu_pct`, `method`, `sampled_at`, or `null`); unknown is `null`, never `0`, and it is `null` unless the environment is active with a real sample from the last three sampling intervals. Container stats are one bounded call per engine per tick; `ps` runs under `LC_ALL=C`; SI units (`kB`, `MB`) are powers of 1000; a process group whose recorded leader is gone or recycled is unknown. `env calibrate [--ram-fraction F]` (`aethyme.env-calibrate/v1`: `calibration` {`host_memory_bytes`, `engine_vm`, `ram_fraction`, `budget_bytes`, `unit_bytes`, `profiles[]` {`p50_bytes`, `p95_bytes`, `peak_bytes`, `proposed_units`}, `pools`, `insufficient`, `withheld`} and `diff`) proposes units and pool limits as a unified diff and never writes; with no profile at 10+ samples it exits 3 `insufficient_history`. When any capacity-consuming profile has fewer than 10 samples, or the budget is below one unit, nothing is proposed and `withheld` says why (mixing rescaled and old `units` would misstate capacity). `pools` lists only repository pools, which share the budget evenly; host pools are sized in host state. #1219 adds content-addressed caches under `environments/cache/`: a component with `build`, `cache_inputs` and an output (`build_output`, or `serve_dir` for `static`) is built once per input digest (component, command, contents of tracked and unignored files matching `cache_inputs`, `cache_env` values, OS/arch) and materialised into other worktrees with a copy-on-write clone where possible (log line `build cache hit <digest>`); a component with `template`, `template_destroy` and `template_inputs` gets one template per digest and `provision` receives `AETHYME_ENV_TEMPLATE_ID`. Builds and templates of one digest are coalesced under the host exclusive key `aethyme-env-build|template:<repository>:<component>:<digest>`. `env cache list|prune [--all]` (`aethyme.env-cache/v1`) lists entries with `in_use` and evicts unused ones LRU down to `AETHYME_ENV_CACHE_MAX_BYTES` (default 8 GiB) or all with `--all`; templates are evicted only by a successful `template_destroy` (run in the cache directory with the component's `env` and `pass_env` and the hook timeout, so it must not need the creating worktree), otherwise kept and reported; prune takes each entry's key before evicting it and evicts nothing (`refused`) when it cannot tell what a live environment uses. A cached build runs with only its declared inputs (`env`, `pass_env`, `cache_env`), never the environment's `AETHYME_ENV_*` bindings; `build_output` and `serve_dir` must be worktree subdirectories (`invalid_path`), and `template` requires `template_inputs` (`missing_template_inputs`). The cache is not yet charged to the managed-storage ledger. Since #1275 `broker gc plan --json` reports this repository's part of the cache as `environment_cache` (`root`, `complete`, `refusal_code`, `budget_bytes`, `host_total_bytes`, `build_entries`, `build_bytes`, `in_use_entries`, `in_use_bytes`, `reclaimable_bytes`, `template_entries`, and `refused`; omitted when the repository has no entries). An incomplete host inventory has a refusal code and reason, null totals, and no eviction candidates. The bounded no-follow inventory and descriptor-relative eviction are supported on Linux and macOS; other platforms report an incomplete inventory. `environment_caches[]` candidates include `path`, `repository`, `component`, `digest`, cache-root and entry filesystem identities, the observed `cache_key` when available, estimated sizes, `last_used_at_ms`, and `reason`; these fields are digest-bound. A resumable apply journal records `deletion_started` before it removes any entry content, and the list is omitted when empty: the unused builds `env cache prune` would evict, least recently used first while the whole cache is over its budget. Their bytes count in `estimated_reclaimable_bytes` and `estimated_build_output_reclaimable_bytes`. An operator-invoked `gc apply` evicts each under its key, never one in use and never one used since the plan (kept and reported in `failures`), and reports `environment_caches_reclaimed[]`; templates stay with `env cache prune --all`. A saved candidate from before filesystem-identity binding is retained with `environment_cache_identity_missing`; identity is never inferred from current disk state. If only such candidates remain, `gc apply --json` sets `recovery_action` to `aethyme broker gc abandon --confirm <digest>`; that command archives the journal without removing retained data, after which the operator can review a fresh plan. Since #1292 a listening process, compose or container component may set `entry_path` (a URL path starting with `/`, else `invalid_path`; other components refuse it as `field_not_supported`): it is appended to that component's `AETHYME_ENV_URL_<C>` (in its commands, `env exec` and `env up`'s `variables`), `env up` gains `urls` (component to URL) and `env status <id>` gains the same `urls`; the field is left out of the compiled snapshot when unset, so existing digests do not change. Since #1295 `env up --reconfigure` reconfigures in place when the namespace and port keys are unchanged and the environment is active or parked: it parks, destroys only changed and removed components, swaps the snapshot (same `env_id`, reservation and ports), provisions added and changed components, and wakes, keeping unchanged components' provisioned data and receipts; otherwise it destroys and recreates as before. Either way, destroying a component that holds data (`provision` or `template`, or `compose`/`container`) exits 3 `recreate_required` naming the components unless each is passed as `--recreate <component>`. A `reconfigured` note records `strategy` (`in_place` with `kept`, `recreated`, `removed`, `added`, or `recreate`). Components accept `optional = true` and `enabled_by` globs (`enabled_by_without_optional`, `depends_on_optional` diagnostics); an optional component runs only when named with `env up --component <name>` (unknown names exit 2 `unknown_component`), enabled by a changed path, or already part of the running environment, and its port stays reserved while it is off, so selecting one adds it in place without `--reconfigure`. `env up`/`status` `ports` list only running components; compiled snapshots gain `optional`, `enabled_by` and `reserved_port_keys` only when set, so existing digests are unchanged. Since #1239 `environments.toml` accepts `[budgets.seconds]` (metric name to a positive whole number of seconds; any name, so hooks can report their own) and `[budgets.memory_mib]` (declared profile to MiB; `invalid_budget`, `unknown_profile`), the orchestrator times every step it drives into `environments.db` table `environment_timings` (additive; the newest 200 per repository, metric and outcome, failures apart): `up.<profile>` for a warm up (waking a parked environment), `up.cold` for one that provisioned, `park`, `down` (not `--abandon`), and `provision|start|health.<component>`, plus each non-negative number in a provision receipt's top-level `metrics` object under its own name. `env budgets [--json]` (`aethyme.env-budgets/v1`: `repository`, `metrics[]` {`metric`, `unit` `seconds`|`memory_mib`, `budget`, `count`, `failures`, `p50`, `p95`, `latest`, `trend` {`older_p50`, `newer_p50`, `change_pct`} or `null` under 4 values, `status`}) reports every budgeted or recorded metric plus `memory.<profile>` from the #1218 samples; `status` is `ok` (p95 within budget), `watch` (only p50 within), `over`, `no-data` or `no-budget`. Read-only. A wake records `start|health.<component>` as a cold up does. Timings are not recorded when `CI` is set (to anything but empty, `0` or `false`) unless `AETHYME_ENV_TIMINGS=1`; `AETHYME_ENV_TIMINGS=0` turns them off anywhere. `env calibrate` adds `budgets[]` {`metric`, `unit`, `key`, `current`, `proposed`, `count`, `p50`, `p95`, `reason` `raise`|`tighten`} and `units_withheld`: for a declared budget with 10+ successful runs it proposes the p95 plus 50% headroom when p50 is over (`raise`) or when that is under half the budget (`tighten`), as lines in the same `diff`; with budget proposals it no longer exits `insufficient_history` for thin memory history, and reports `calibration: null` with the reason in `units_withheld`. `[budgets.outcomes]` (`issue` default `false`, `windows` default 3 and at least 1, `failure_rate_pct` 1–100, `labels`; `invalid_budget`) makes `env budgets --act` (and the supervisor, every 15 minutes, for each repository with environments on the host) carry out breach outcomes: a window is an evaluation in which a budgeted metric has new runs; a breach is `over`, or more than `failure_rate_pct` of at least 5 runs failed; after `windows` consecutive breaches it upserts one issue per repository and metric through `broker advanced issues upsert` (#1238, fingerprint `env-budget:<12 hex of the repository key>:<metric>`), under the repository's trusted `[automation]` policy, and resolves it once the metric is `ok`; `watch` holds. A refused or failed write leaves the issue unmarked, so the next window retries it. State lives in `environments.db` table `budget_outcomes` (additive). `--act` adds `outcomes` {`repository`, `windows`, `actions[]` {`kind` `upsert`|`resolve`, `metric`, `reason` `over`|`failure_rate`|`recovered`, `consecutive`, `report`, `fingerprint`, `status` `done`|`failed`, `detail`}} to the `env budgets` JSON, exits 3 `outcomes_not_enabled` without `issue = true`, and needs a GitHub `origin`. `broker status doctor --json` adds `environment_budgets_over` (the same metric objects, only `over`, omitted when empty; reported, never counted against health, and listed in `budget_cut` as `environment_budgets` when the inspection budget runs out). The supervisor is one per host user, started only by `--ensure` (later by lifecycle commands), in its own process group, and exits after `AETHYME_ENV_SUPERVISOR_LINGER_SECONDS` (default 300) with nothing to supervise; status reports `running`, `absent` or `unproven` from the published record, never by probing the lock. Since #1289 a profile may have no components ("no environment needed", the target of rules for diffs that need nothing): `env validate` no longer reports `no_components`, `env plan` shows an empty `start_order`, and `env up` on it exits 0 with `nothing_needed: true` and `profile`, registering, reserving and starting nothing (no supervisor either). Since #1290 a `[[rules]]` entry accepts an optional `exclude` glob list (same `globset` syntax as `paths`; an invalid one is `invalid_glob` on `rules.<name>.exclude`): a changed path matching `exclude` never fires that rule, so it is neither listed in that rule's `fired[].paths` nor counted as matched by it, while highest-rank inference across rules is unchanged. On Windows `env --json` prints the error object with `unsupported_platform`, exit 3. The host `<host state>/environments.toml` accepts `max_pin_seconds` (the longest new `env pin --for`; default and ceiling 86400, valid values 0 or 60..=86400; 0 disables new pins, a nonzero value outside the valid range refuses `env pin` with `invalid_host_setting` (exit 3), malformed settings refuse with `host_settings_invalid` (exit 3), unreadable settings refuse with `host_settings_unreadable` (exit 6), and a request above a valid cap exits 2 with `pin_too_long`) and `max_exec_busy_seconds` (unset by default; nonnegative seconds including 0, which caps a proven-live command immediately). Lowering `max_pin_seconds` affects only new requests: a stored `pinned_until` remains effective until expiry or explicit unpin. A busy cap applies only when the marker proves its process is still running; at or beyond the cap, the command and marker remain but the supervisor may park the environment and records `exec busy cap reached` (`env status` idleness reports `busy_capped: true`). An unknown, unreadable, malformed, mismatched, or otherwise unproven marker/process observation remains busy indefinitely and is never aged out by either cap; only proven process absence permits stale-marker removal. A missing busy directory remains idle, while other directory or entry-read errors remain busy. Because the host file uses strict unknown-key rejection, upgrade every binary that reads it before writing either new key; older readers reject those keys. Since #1521-#1524 (2026-11-08): `supervisor.log` lines start with an RFC 3339 UTC timestamp (`2026-11-08T10:00:00.123Z`), the supervisor logs `aethyme env supervisor started: version V, pid P, linger Ns, tick Mms` and `aethyme env supervisor exiting: <reason>`, and a line repeated within 10 minutes is written once and then summarised as `<line> (repeated N more times in 10m)`. On exit it writes `environments/supervisor-exit.json` (`pid`, `version`, `started_at_ms`, `exited_at_ms`, `reason`: `idle linger elapsed (Ns with nothing to supervise)`, `SIGTERM`/`SIGINT`/`SIGHUP`, or `panic: <message>`) and withdraws `supervisor.json`. The `supervisor` object of `env status` gains `last_exit` (same fields; when absent) and `installed_version` (when running a different version than the reading binary); a `supervisor.json` whose process is gone without an exit record reads as `absent` with `last_exit.reason` `exited without recording a reason (killed, crashed, or the host restarted)` and no `exited_at_ms`. A failed destroy (orphaned or not) is retried with backoff (30 s doubling, at most 30 min) up to 5 times, and the last failure logs that it gave up. Each hook phase logs to `<env_id>/<component>.<phase>.log` (`build`, `provision`, `template`, `stop`, `destroy`); `<component>.log` keeps the runtime output; `[aethyme]` marker lines become `[aethyme <RFC 3339>] ...` and each hook run opens with `<label>: started`. `env logs` accepts `--phase run|build|provision|template|stop|destroy` and `--run last`; each `logs[]` entry gains `phase`, and without `--phase` the runtime log and every existing phase log are listed. `last_error` and failure notes from a hook (`hook_failed`, `hook_timeout`, and `redeploy_failed` caused by one) gain `log` {`path`, `offset`}, the byte offset of the run's start marker. The host file accepts `[logs]` `max_bytes` (default 8388608, at least 65536) and `keep` (rotated files per log, default 1, at most 100, 0 discards a full log); rotations are `.log.1` (newest) to `.log.<keep>`. Shared environments: a successful redeploy clears `last_error` and its `redeployed` note carries the cleared error as `resolved_error`; a build that failed to run or be cleaned up (`io_error`) is retried once after a `redeploy build retried` note; the redeploy decides between blue/green and hold-and-swap from the record as it is after the build, and waits up to 60 s for a wake or park under way before swapping; every `env shared up|redeploy|destroy` ends by removing each export directory under `environments/shared/<repo>/<name>/` (12-hex names and their `.staging`) that no live slot runs from, retrying a removal that fails and leaving it to the next operation if it still does; the shared `build` goes through the build cache, so `cache_inputs` with `build_output` restores an unchanged build; a hook timeout's message names `hook_timeout_seconds`. `[logs]` follows the same rule as the other host keys: upgrade every reader before writing it. | Later environment issues (#1222, #1210-#1219) add lifecycle subcommands to the same group; existing keys are additive-only within `v1`. A wrapper must branch on `schema` and treat exit 4 from `validate` as "file invalid, read `diagnostics`". Covered by `aethyme-broker` `environments::tests` and the `env` help snapshots. Since #1291 a `static` component accepts `spa_fallback = true` (rejected on other runtimes as `field_not_supported`): a GET or HEAD of a missing extensionless path inside the served directory serves its `index.html` with `200`; missing assets and ineligible existing paths retain `404`; the route table (`/__aethyme/routes`) gains `spa_fallback: true` on such routes, and the field is left out of the compiled snapshot when false so existing digests do not change. |
| Repositories declaring `[[prebuild]]` in `.aethyme/environments.toml`, readers of `aethyme env prebuild [--run [--name <n>]] --json`, and automations keyed on `prebuild:<name>` outcomes (introduced 2026-11-02, #1237) | `[[prebuild]]` (`name`, `profile`, `component`, `ref` default `default_branch`, `derive_from_previous`, `when` of `host_idle` / `inside:HH:MM-HH:MM` / `outside:HH:MM-HH:MM`, `quiet_minutes` default 15, `keep` default 2, `timeout_seconds` default 10800) is read from the default branch only. An older binary refuses an environments file that declares it (`deny_unknown_fields`). The supervisor checks every 5 minutes and stays up while a prebuild is building or waiting for its window; it keeps a host-owned `git clone --shared` checkout per prebuild under `<host state>/environments/prebuild/<repo>/<name>/checkout` (no worktree is registered in the repository) and computes the template digest exactly as `env up` does, so a prebuilt template is the one `env up` finds. The build runs the component's `template` under the digest's existing exclusive key with `AETHYME_PREBUILD_NAME/_REF/_COMMIT/_DIGEST` and, when deriving, `AETHYME_PREBUILD_PREVIOUS_ID/_DIGEST`, but no environment ports. State lives in a new `environments.db` table `prebuild_state` (created with `CREATE TABLE IF NOT EXISTS`; `STORE_SCHEMA_VERSION` is unchanged, so older binaries still open the store); each run is also an `environment_timings` row `prebuild.<name>` with env id `prebuild:<name>`. `env prebuild --json` is `aethyme.env-prebuild/v1` with `prebuilds[]` (`name`, `ref`, `profile`, `component`, `when`, `state`), and `--run` reports `result` in `unchanged`/`published`/`held`/`waiting`/`built`/`failed`/`error` and exits 1 on `failed` or `error`. A failure is reported through `[[automation.outcomes]] source = "prebuild:<name>"` (no key: one issue per prebuild, owners from the last good commit to the tip) and is resolved by the next published template; without the rule nothing is written. Keep-N pruning evicts only that component's older templates through the same locks and use checks as `env cache prune`; `env cache prune --all` still evicts an unused prebuilt template. |
| Repositories setting `max_pin_seconds`, `max_exec_busy_seconds`, `template_keep` or the `[[prebuild]]` keys `artifact`, `max_memory_pct` and `preemptible` in `.aethyme/environments.toml`, and readers of `env prebuild --run --json` and the environment snapshot (introduced 2026-11-02, #1366) | Top-level `max_pin_seconds` (0 or 60..=86400) and `max_exec_busy_seconds` are the repository's keep-awake caps: the tighter of host and repository applies, so a repository can only lower its host's caps. They are recorded on the compiled snapshot (`max_pin_seconds`, `max_exec_busy_seconds`, omitted when unset) outside the snapshot digest, and `env up` refreshes them on an existing environment without a reconfigure. `env pin` refuses a longer pin with `pin_too_long` naming the repository cap. A component's `template_keep` (at least 1) keeps its newest N templates: a new template evicts older ones through `template_destroy` unless a live environment uses them, and every template marker records `keep`, which `env cache prune` (with or without `--all`) honours by never evicting the newest `keep` of a component. A `[[prebuild]]` also takes `artifact = "template" | "build"` (default: the template if declared, else a cacheable `build` with `cache_inputs` and an output, published as the same build cache entry `env up` restores), `max_memory_pct` (1-100; macOS `kern.memorystatus_level`, Linux `MemAvailable`; an unreadable reading does not block) and `preemptible` (the hook is stopped when an environment becomes active during a `host_idle` build or memory passes the cap; `env prebuild --run` reports `result = "preempted"`, the state goes back to `waiting`, nothing is reported as a failure). `DriverError` gains `hook_preempted`. A successful default-branch fetch by `broker push`/`sync`/`start`, and the target refresh after a brokered `gh pr merge`, records the default branch's declared prebuilds (`prebuild_state` rows with status `new`) and starts the environment supervisor if none runs; `AETHYME_PREBUILD_NUDGE=0` turns that off. A prebuild in state `new` keeps the supervisor up until it has been looked at. While a build or template hook holds a cache key it writes `<host state>/environments/cache-holders/<sha256 of key>.json` (`pid`, `until_ms`), removed when the hook ends; a waiter for that key waits until `until_ms` plus 60 s when the holder is alive and that is past the usual 30 minutes, so an `env up` outlasts a long prebuild of the same digest. Older binaries refuse an environments file that uses any of these keys. |
| Readers of `broker submit --json` and its stdout in repositories that opt in to collaboration capture (introduced 2026-10-09, #660, experimental) | `[collaboration] capture = "off" \| "advisory" \| "required"` and `project = "<key>"` in `.aethyme/config.toml` (committed default branch first, as `[promote]`). Off, absent or a missing section: no new output, no collaboration state written. Advisory: after the legacy verdict, `submit --json` gains a final `collaboration_capture` object (`schema` `aethyme.submit-capture/experimental-v0`; see `docs/json-contracts.md`) and text output gains one `collaboration capture (advisory): ...` line; verdict, `promoted` and exit code are unchanged. Required: the capture runs first and, unless acknowledged, submit prints `{"submitted": false, "collaboration_capture": ...}` and exits 3 before any queue entry. An unknown `capture` value, a non-table `collaboration` or an unreadable config that mentions it is refused (`unsupported_policy`, `config_unreadable`). If the session moves after a required capture, submit exits 3 with `{"submitted": false, "error": {"code": "captured_head_moved"}, "collaboration_capture": ...}`. Under `required`, every promotion (`submit` auto-promotion, `broker promote --entry`, re-verification, queue drain) of a head without an acknowledged required capture fails with `CaptureRequiredForPromotion` (exit 3). The report carries `config_source` (`committed` or `working_copy`) and a path-free `detail`. **Schema fence:** broker schema v50 is a compatible no-op. A #660+ binary raises a repository's `broker.db` `min_compatible_schema` to 50 when the committed default-branch config says `capture = "required"`, on writable opens and in status, submit and promotion. It records `meta.collaboration_fence` and `meta.collaboration_fence_source`. Snapshot opens (hooks, pre-commit, read-only status) never write it. `broker status --json` gains an optional `collaboration_fence` object (`min_compatible_schema`, `reason`, `source`, `state`: `active` or `pending`), a `collaboration.fence-pending` advice row while pending, and a `Collaboration fence:` text line when active. | A reader that requires the exact legacy key set must allow one extra trailing key when a repository opts in. A wrapper treating exit 3 as a lease or ownership refusal must also read `collaboration_capture` (or the promotion error) when the repository requires capture. Binaries older than #660 ignore the section. Once a #660+ binary has opened a required repository, every pre-#660 binary is refused with `SchemaTooNew`, for every command, and turning required off does not undo it. Until that first open, an older binary can still submit uncaptured (`local-v3-l2-optin.md`). Covered by `aethyme-broker/tests/collaboration_submit_cli.rs`. |
| Agents, wrappers and CI reading graph-integrity verdicts: `broker submit --json` `graph_integrity`, `broker status --json` advice ids, brokered `gh pr merge` stderr, and gate runs (introduced 2026-10-06, #280, #292) | Graph integrity is advice on every landing lane and never refuses. `broker submit` runs its gates and promotes whatever the verdict; its `graph_integrity` object gains `verdict` (`disabled`, `fresh`, `stale` or `unknown`; `incompatible` and `error` statuses read as `unknown`, never `fresh`) and, when there is something to say, `advice`. `broker status --json` raises `graph.stale` (severity `warning`) or `graph.unknown` (`notice`) for a session whose latest recorded check was not fresh, with the tree and up to five stale paths as evidence. Gate runs (`gates run`, the pre-push hook) record the verdict and print `[graph-integrity] <advice>` on stderr instead of failing with `GraphIntegrityRejected`, which no path produces any more. A brokered `gh pr merge` in a repository whose committed graph is authoritative checks the pull request's head commit, records a `graph.integrity_checked` event for the session, and prints `[graph-integrity] pull request #N: …`; a head not available locally reads as `unknown`. | A wrapper that treated a submit rejection or a failed gate run as "graph is stale" must read `verdict`/`advice` or the status advice instead. Making the verdict block again on either lane brings back #292's asymmetry, and reporting an unverifiable graph as fresh hides exactly the case the check exists for. |
| Agents and wrappers running read-only coordinated commands: `aethyme broker advanced git|gh ... -- <read>` (`--effect read` or a command classified as a read) (introduced 2026-10-06, #555) | A GitHub read with no `--queue-timeout` and no `--no-wait` is bounded at 30 s; a Git read is bounded at 90 s. `AETHYME_BROKER_READ_BUDGET_SECS` overrides either default, an explicit `--queue-timeout` still wins, and `--no-wait` keeps its existing 30 s admission bound. Long-running GitHub reads skip only the automatic default budget: `gh ... watch`, any `gh` read with `--watch`/`-w` (`gh pr checks --watch`), `gh run view --log`/`--log-failed`, `gh run|release download`, and `gh api --paginate`; caller-selected bounds still apply. A read takes no repository lock and no unreconciled write blocks it. On expiry the provider child is killed, the journal row is `failed` with `failure_class: coordinated_operation_timeout`, `remote_outcome: not_applicable`, `preparation_ms` and `provider_wait_ms`, and the command exits 6 (environment, not 5) with a message that starts `read-only operation <id> for <repo> got no answer within its <budget> budget`, then gives the time spent preparing and the time spent waiting for the provider (`git` or `gh`) to respond, and says the read changed nothing and can be retried. Writes keep the wait their caller chose, and a timed-out write still exits 5 with its reconciliation guidance. | A wrapper that treated every coordinated timeout as exit 5 and ran reconciliation would reconcile a read that changed nothing. For a caller that left the budget unbounded, dropping an exemption cuts a watch, log stream, download or paginated list at the 30 s GitHub default. Raising a provider's default above the caller's own timeout brings back the kill with no diagnostic that #555 reported. |
| Gate log readers: the `broker submit` failure-tail replay, operators reading `.aethyme/logs/gates/<gate>-<tree>-<worker>.log`, and any wrapper that tails a gate log (introduced 2026-09-13, #177) | Every gate log now opens with one line prefixed `aethyme gate environment: ` naming the `git` the gate will run and stating whether a PATH wrapper the probe condemned was dropped from the PATH handed to gate commands. The broker's own tail renderer holds that prefix out of the replayed "gate output" block so aethyme's line is never reported as output the gate produced. | A reader that assumes the first line of a gate log is the gate's own first line of output misattributes it; changing or dropping the prefix breaks that partition and brings the mislabel back. |
| Gate log readers, as above (introduced 2026-09-26, schema 43; timeout diagnostics extended for #413) | Every executed gate log now carries a second `aethyme gate environment: ` line after the git line, e.g. `aethyme gate environment: load 1m 12.3/10 cpus, 23.4 GiB free` (`unknown` for a value the platform would not report). It uses the same prefix, so the tail renderer, which partitions by prefix rather than by line number, holds it out of the replayed output. When a native timeout fires, its diagnostic includes the configured/effective timeout and the measured one-minute load per logical CPU at start and at timeout (`unavailable` if that reading fails). Load-admitted gates scale the configured deadline by the start load per CPU, between 1x and the admission threshold, with a global 4x ceiling; the effective timeout rounds up to whole seconds. | A reader that skips exactly one environment header line would now treat it as gate output; match the prefix instead. Consumers must tolerate the extended timeout diagnostic and treat its measured load as context, not as a verdict on the code. The timeout remains bounded to at most 4x its configured duration. |
| `.aethyme/gates.toml` writers in every repository that defines gates, plus any generator that emits one (introduced 2026-09-27; timeout scaling extended for #413) | A `[[gate]]` may set `max_load_per_cpu`, a positive number (integer or float). Before an expensive gate starts, the broker waits while the one-minute load average per logical CPU is above that threshold (default `3.0` for any gate with `cost >= 3` that does not set it), bounded by `resource_wait_seconds`, then runs anyway; progress lines read `gate <name> waiting for host load: ...`, `gate <name> admitted after <n>s waiting for host load: ...`, or `gate <name> host load wait bound of <n>s reached (...); running anyway`. For a load-admitted gate with `timeout_seconds`, the deadline scales by the start load per CPU, never below the configured duration and capped by both the admission threshold and a global 4x ceiling; it rounds up to whole seconds. If load cannot be measured, the configured deadline is used. The key enters the execution-definition hash only when set, and is not part of the portable gate-scope manifest. | An older broker ignores the key, so the gate starts without waiting (today's behaviour) and uses the configured timeout unchanged. Zero, negative, non-finite or non-numeric values are a `gates.toml` parse error, which fails every gate command in that repository until fixed. A wrapper that treats a quiet minutes-long pre-start pause as a hang should read the progress lines. |
| Review ledger `detail` readers: `aethyme broker advanced review ledger`, the pull-request comment the projection renders, and any wrapper reading the `detail` column of a review row (introduced 2026-09-13, #173) | A review a provider refused now carries `<classification>: <the provider's own words>`, where the classification is one of `quota_exhausted`, `rate_limited`, `provider_error` or `unknown`. The same refusals surface as `review_refusals[]` and a `review.provider-refusals` advisory in `aethyme broker status --json`. Every other writer of `detail` -- a rule name, a debounce window -- is unchanged and deliberately does not parse as a refusal. | A reader that treats the whole `detail` as prose loses the classification; one that treats every `detail` as a refusal invents causes for rows no provider ever saw. Changing or dropping the `<class>: ` prefix breaks that partition and returns a refused review to being indistinguishable from one nobody was ever asked for, which is the deadlock in #173. |
| `[review.routing]` policy writers: `.aethyme/config.toml` in every repository that enables review routing, plus any generator that emits one (introduced 2026-09-13, #175) | A route may declare `[review.routing.route.<type>.on_refusal.<class>]`, a one-hop escape taken when the *previous* attempt at that dimension refused with that `RefusalClass`. Absent means today's behaviour exactly: no refusal reroutes anything. A fallback table accepts `backend`, `mention`, `instructions`, `max_concurrent` and `stale_after_minutes` -- not a nested `on_refusal`, which is a parse error rather than an ignored key. | A generator that emits an `on_refusal` whose backend equals the route it escapes is refused at load with `FallbackIsNoOp`, not silently accepted, because a no-op escape reads as a configured recovery and behaves as a second refusal. A reader that assumes `ReviewRoute` is a flat table breaks on the nested key; a writer that nests a chain gets a parse error by construction. |
| `[review.classification]` and `[review] pr_size` policy writers, readers of `aethyme broker advanced review plan --json` (`change`) and `aethyme broker push --pr --json` (`pr_size`), and anything that reads Aethyme's PR labels (introduced 2026-10-07, #584, #239) | `review plan` adds a `change` object: `size` (`files_changed`, `lines_added`, `lines_deleted`, `churn`, optional `excluded[]` with `path`/`why`, optional `binary[]`), `signals` (`sensitive_paths`, `contract_surface` tagged `state` = `clear`/`touched`(+`symbols`)/`unknown`, `gate_policy`, `workflows`, `migrations`, `dependency_manifest`, `from_fork`, `first_time_contributor`, `authored_by_model`, and an optional `unknown[]` naming inputs that could not be determined: `contract_surface`, `generated_attributes`), `tier` (`trivial`/`normal`/`large`), `computed_risk`, `risk`, `risky`, `reasons[]`, `thresholds` (`trivial`, `pr_size`). With `[review.projection] enabled = true` the projection adds `aethyme/size:<tier>` (`label_size`, default true), the risk label shows the measured risk raised by any `Risk:` declaration, and the owned comment gains one size/risk line. `broker push --pr --json` adds an optional `pr_size` (`max_files`, `max_changed_lines`, `files`, `changed_lines`, `over_threshold`, `configured`) and warns above it; it never refuses. Generated agent guidance states `pr_size` only when configured. | Renaming a signal, a tier or a JSON field silently breaks rules and dashboards keyed on them. Generated-file exclusion reads the BASE commit's `.gitattributes`, so a change cannot mark its own files generated. Treating an `unknown` contract scan as clear would let anything that relaxes review act on a scan that never ran. Making the push refuse on size would turn an advisory threshold into a gate. |
| `[[review.trigger.rule]]` writers using the #584 conditions and actions, readers of `review plan --json` (`rule_actions`), and readers of waived ledger rows (introduced 2026-10-07, #584) | Rules accept `min_tier`/`max_tier`, `min_files`/`max_files`, `min_churn`/`max_churn`, `signals`, `none_of_signals`, `waive = [...]` and `labels = [...]`; `require` now defaults to empty and a rule must carry at least one of `require`, `waive`, `labels`. `review plan --json` adds `rule_actions` (`waivers[]` with `review_type`/`rule`/`reason`, `refused_waivers[]` with `review_type`/`rule`/`why`, `labels[]` with `label`/`rule`). `review run` writes each planned waiver through the `review waive` ledger record at the current head, attributed to `aethyme review rule `<name>``, and projects it as the `waived` state and the `aethyme/review:waived` label. The broker refuses automatic waivers for `from_fork`, `first_time_contributor`, `contract_surface`, `gate_policy`, `workflows` and `sensitive_paths`, and whenever `change.signals.unknown` is non-empty (now also naming `from_fork`/`first_time_contributor` when the provider did not answer, and the path signals when its file list is truncated); re-reads the head before writing and writes nothing if it moved; revalidates rule terms at planning time; rejects wildcard dimensions and waive rules requiring a guarded signal at load; and lets `require` beat `waive`. | Relaxing a guardrail through configuration, or treating an unknown contract scan as clear, would let a rule excuse review on exactly the changes that most need it. A waiver not bound to the head would survive a push that changed the code. |
| `[review] run_on_push` writers and readers of `broker push --json` (introduced 2026-10-07) | `[review]` accepts `run_on_push` (bool, default false) and `run_on_push_budget_secs` (1..=600, default 60); any other unknown scalar key is still rejected. With `run_on_push`, `broker push` (with or without `--pr`) looks up the branch's open pull request (`gh pr list` now also reads `isDraft`; `pr.draft` appears in the JSON when true) and, for a non-draft one, runs `broker advanced review run --session <id> --repo <owner/name> --pr <n> --from-provider --json` as a child process. `push --json` gains `review_run`: `{performed: true, pull_request, actions, rule_comments, decisions}`, or `{performed: false, skipped}` for a draft or missing pull request, or `{performed: false, pull_request, error, rerun}` on failure or when the budget ran out (the run then continues in the background, logging to `.aethyme/logs/review-on-push-pr<n>.log`); failures also print a `warning:` line on stderr. The push itself never fails because of it. | Running the review on push is what lets a repository classify and comment without a scheduler; renaming the keys silently stops it. Killing a run at the budget instead of detaching it would leave a coordinated write with an unknown outcome and write-block the repository. |
| `[review.comments.<name>]` and rule `comment` writers, readers of `review plan --json` / `review run --json` (`rule_comments`), and anything that reads pull request comments carrying `<!-- aethyme:comment:<key> -->` (introduced 2026-10-07, #596) | A `[[review.trigger.rule]]` may carry `comment = { template, key, on_unmatch = "keep" \| "delete" }`; `[review.comments.<name>]` holds `body`, whose `{{variable}}` names are limited to `tier`, `risk`, `files_changed`, `lines_added`, `lines_deleted`, `churn`, `signals`, `reasons`, `rule`, `pr_number`, `head` (full 40-character SHA, added 2026-10-07), `head_short`, `base`. Load rejects an unknown variable, an unclosed `{{`, a missing template, an invalid or duplicate key. `review plan` and `review run` add `rule_comments[]` (`key`, `rule`, `decision` = `create`/`update`/`none`/`delete`/`suppressed`, optional `why`, optional `body`). `review run` creates with `pr comment`, edits with `api -X PATCH repos/<repo>/issues/comments/<id>` and deletes with `api -X DELETE …` (declared destructive), each through the coordinated lane with a reason naming the rule and key. A comment counts as the broker's only when its first line is the marker and its author is the `gh api user` login. | The marker is how a later tick finds its own comment: changing it orphans every comment already posted and doubles them. Adopting a marked comment by anyone else would let a rule overwrite or delete it. Interpolating text the pull request controls would let its author make the broker mention people or post links. |
| `[review.trigger]` policy writers and every reader of the broker database (introduced 2026-09-13, #172) | A `[review.trigger.schedule.<type>]` table may declare `freshness = "head"` (the default, and what every dimension has always done) or `"head_and_base"`, which additionally invalidates a completed review when the base commit moves. Schema v36 adds a nullable `base_commit` to `review_requests` and to `pull_request_observations`; `review run` fills it from the provider's `baseRefOid`, falling back to the local ref. | A reader that assumes `review_requests.base_commit` is `NOT NULL` breaks on every row written before v36 -- and the null is load-bearing, not incidental: `head_and_base` reads an absent base as *unproven*, so it re-reviews rather than trusting a comparison nobody made. Backfilling the column from today's branch tip would assert exactly that comparison and silently pass stale reviews. Any process holding an older `aethyme` binary cannot open a v36 database until it is reinstalled. |
| Every reader of `review_requests.state` in the broker database: adapters that close a review row, the PR projection, and any external dashboard over the ledger (introduced 2026-09-13, #172) | `state` gains a seventh value, `waived`: one dimension excused by a person at one head, written only by `aethyme broker advanced review waive` and carrying `waived by <who>: <reason>` in `detail`. Schema v37 rebuilds `review_requests` to widen the state CHECK, carrying v36's `base_commit` with it. `review state --state waived` is refused. | A reader that matches the six previous states exhaustively and treats the default as an error breaks on a waived row; one that treats the default as *settled* is correct, and one that treats it as *reviewed* is the defect this state exists to prevent -- `waived` must never be rendered, counted, or exported as `satisfied`. A waiver is scoped to (repository, pull request, review type, head commit) by the existing unique index, so it expires when the head moves; a reader that caches it per pull request reintroduces the standing exemption. Any process holding an older `aethyme` binary cannot open a v37 database until it is reinstalled. |
| Operators and wrappers reading dependency preparation (introduced 2026-10-02; extended 2026-10-27) | `aethyme init --json` reports gain an additive `prepare` field (the phase-4 `prepare.draft` check; absent when `.aethyme/prepare.toml` already exists). `.aethyme/prepare.toml` accepts optional `[shared_cache] max_bytes` (omission supplies an 8 GiB logical-byte rotation threshold per repository; explicit `0` disabled it until 2026-11-17 and now means the same 8 GiB default; refused without a `repository_shared` step) and `root = "auto" | "host" | "worktree_volume"` (absent meant `host` until 2026-11-18 and now means `auto`; other values are refused). `worktree_volume` selects a repository-owned cache under the worktree placement filesystem and requires its ownership marker and matching filesystem identity. An explicit `AETHYME_PREPARATION_CACHE_ROOT` keeps precedence, but a cross-filesystem override is refused. Changing roots does not migrate or remove prior cache data. `aethyme broker submit prepare --json` now reports schema version 2. When rotation occurs, `shared_cache_rotation` includes `key`, `budget_bytes`, nullable `bytes_before`, `observed_bytes_lower_bound`, `bytes_after`, `removed`, `inventory_complete`, and typed `rotation_reason`. A complete over-budget inventory reports an exact number and `budget_exceeded`; a time- or entry-limited inventory reports `bytes_before: null`, `inventory_complete: false`, and `inventory_time_limit` or `inventory_entry_limit`. `inventory_complete` describes the size scan; `removed` reports whether rotation finished. | Configuration and preparation-state schemas remain version 1; the JSON report schema advances to 2 because `bytes_before` is nullable. Strict consumers must handle null and the two inventory-limit reasons before interpreting rotation data. Since 2026-11-14 an inventory that stops at its time or entry limit under budget no longer rotates (see the `submit prepare` metrics notes); an early stop over budget rotates with `budget_exceeded`; depth, filesystem, special-node and liveness failures still refuse. The cache-only pinned remover independently refuses special nodes and leaves durable recovery debt. Rotation is checked before preparation, covers all digest keys of that repository, and remains subject to existing writer leases, identity checks and durable recovery; it is not an in-flight write quota. Worktree-local steps acquire no shared budget. Adding `[shared_cache]` changes the trusted gate/prepare policy digest, so the next prepare needs `aethyme broker advanced trust` once, like any policy edit. Renaming `shared_cache.root`, `shared_cache.max_bytes` or `shared_cache_rotation` breaks configurations and wrappers that set or read them. |
| `aethyme collab project association propose/confirm/revoke` and every Local collaboration store reader/writer (experimental T03 association) | Proposal names the invoking and peer ProjectKeys, independently approved ProjectIds, exact committed descriptor RecordIds and anchor snapshots. Each explicit proposal carries a bounded, capability-gated nonce in its canonical record, so a new proposal for the same bindings gets a new exact RecordId. `propose --write` writes only `.aethyme/project-association.json`; `confirm --confirm <RecordId>` reloads both committed descriptors and both approved local bindings, then writes the canonical association object and one row to the invoking project’s schema-31 store. The peer store is read-only. `revoke` changes only the invoking store’s row; association rows and object roots are retained. A revoked RecordId is immutable; re-association requires a fresh proposal and exact confirmation. | A URL or matching tree never substitutes for either independent binding. Consumers must preserve the local and peer sides in order and reject stale descriptor IDs, changed anchors, equal ProjectIds, duplicate active pairs or revoked rows. Pre-schema-31 GC writers cannot see these canonical object roots and must refuse writes to the raised compatibility floor. Confirmation creates no cross-database transaction and grants no source, execution, acceptance or transitive association authority. |
| Operators and generators writing managed dependency-preparation profiles (introduced 2026-10-17) | `.aethyme/prepare.toml` may opt into `[managed_storage.steps.<step>.output_growth_bytes]` with a positive integer byte bound for every exact declared output; `repository_shared` steps also need a positive `shared_cache_growth_bytes`. A missing profile keeps legacy configuration behavior. An otherwise valid, trusted request with an opt-in profile returns typed `ManagedStorageNotReady` before starting any command until reservation, root adoption and old-writer fencing are integrated. The minimal configuration is in `local-v3-storage-lifecycle.md`. | This is a fail-closed planning surface, not an active quota or reservation. CI pins exact positive bounds and the production `Broker::prepare_session` refusal with a command-created sentinel; a guard-removal mutation must fail that sentinel assertion. Do not tell operators that declared bytes currently cap writes. |
| Operators and automation writing `integration reconcile` resolution files, and readers of its JSON plan (introduced 2026-10-07, #464) | Additive: schema-2 `unrecorded_resolutions[].disposition` accepts `tracked_elsewhere` with exactly one of `tracked_branch` (string) or `pull_request` (integer); both fields are forbidden for the other dispositions, and `upstream_commit` is forbidden for this one. The broker reads the branch head from the upstream remote with a bounded `git ls-remote` (and, for `pull_request`, the open PR's `headRefName`/`headRefOid` with the trusted `gh pr view`), requires that head to be fetched, and accepts the commit only when it is an ancestor of that head or a first-parent commit on it carries the same patch id. `plan.commits[].unrecorded_resolution.tracked_elsewhere` reports `remote`, `branch`, `remote_commit`, optional `pull_request`, `containment` (`ancestor` or `patch_id`) and `matching_commit`; such commits are not replayed. `resolution_template.field_contract.unrecorded_dispositions` lists the new rule, and template rows carry `tracked_branch`/`pull_request` only when set. | Accepting a branch the remote does not report, a closed pull request, or a raw-diff match would let reconcile drop work that exists nowhere else. A reader that treats every non-`preserve_and_replay` disposition as "content already upstream" must now also accept "content preserved on another ref". |
| Readers of `broker status --refresh --json` and `broker status doctor --json` at scale (introduced 2026-10-06, #460) | Both stop their per-session Git work at an inspection budget (15 s by default, `AETHYME_STATUS_INSPECTION_BUDGET_MS` overrides it) instead of running until the caller's timeout. `status --refresh` adds the cut checks (`dirty_worktrees`, `unpushed_commits`, `promoted_conflicts`, `integration_drift`) to `deferred_checks`, emits one `status.inspection-budget` notice naming them and the sessions not inspected, and, for a cut drift assessment, gives the unbounded `integration reconcile --upstream <ref> --dry-run`; a cut assessment omits `integration_reconciliation` rather than reporting a partial one. `phase_timings_ms` gains `checkouts`, `unpushed`, `promoted_conflicts` and `integration_drift`. `unpushed_work` gains `not_inspected_sessions[]` (omitted when empty). `doctor --json` gains `phase_timings_ms` and `budget_cut[]` (omitted when empty). The bounded cleanup-eligibility pass behind both now also stops a landing search mid-worktree at its budget and lists that worktree as not inspected. | A session in `not_inspected_sessions`, or a check in `deferred_checks`/`budget_cut`, is unknown, not clean: a reader that counts only `unpushed_work.sessions` or the dirty total would report unread work as absent. `integration reconcile` itself never takes a deadline, because a partial classification must not drive a ref change. |
| Readers of `broker status doctor --json`, `broker advanced worktrees --json` and `gc plan --json` (introduced 2026-10-08, #460, #625) | The status and doctor inspection budget drops from 15 s to 7 s, and `AETHYME_STATUS_INSPECTION_BUDGET_MS` can now only lower it, never raise it. Additive fields: `status doctor` adds `deferred_checks`, and `blocker_sources_unavailable` when blocker enrichment ran out of budget. `advanced worktrees` stops Git inspection, inventory and sizing at a shared 7 s budget (was 10 s for sizing only), adds `deferred_checks` (`worktree_discovery`, `worktree_inspection`, `git_inventory`, `worktree_size_records`, `worktree_sizing`, `prunable_registrations`), lists every checkout it did not reach as a row with `state: "not_inspected"`, and counts that row in `unique_work_count`; `reconciliation.complete` is `false` (omitted when `true`) when the root scan stopped early. `gc plan` and the doctor retention report add `deferred_checks`, and a partial scan reports `budget_verdict: "unknown"` instead of `within`, and `clears_retained_bytes_budget: false`. Field documentation: `broker-inspection-json.md`. | A `not_inspected` row, a nonempty `deferred_checks`, or `complete: false` is unknown, not clean: a reader that treats an absent row or a zero count as "nothing at risk" would report unread work as removable. `not_inspected` is a new row `state` value, so a reader with an exhaustive match must accept it. Mutations reached from a bounded report (the verify-only integration refresh behind `status`) run outside its deadline, so a ref write is never cut off midway. The report deadline and the finish deadline (#494) are one per-thread deadline: nested scopes keep the earlier instant, and lifting it for a mutation lifts both. |
| Callers that act for a broker session, and readers of `broker status --json` (introduced 2026-10-05, #393) | `submit`, `push`, `sync`, `finish`, `close`, `advanced git`/`gh` and `start --reuse` accept `--take-over`. A session is held by the agent runtime process (`claude`, `codex`, or `AETHYME_AGENT_PID`) that started or adopted it, recorded as a `session.holder_bound` event. `status --json` adds a `session.foreign-process` advice row (severity `warning`), a `foreign_processes` phase timing, and `foreign_processes` in `deferred_checks` when the bounded `lsof` scan overruns 500 ms. | Those commands now exit 3 (refused) when a different live agent process holds the session, naming the holder. A wrapper that drives sessions it did not start from a separate long-lived `claude`/`codex` process must pass `--take-over` once, or set `AETHYME_AGENT_PID` to the holder. Callers with no agent runtime ancestor (scripts reparented to launchd, CI) are never refused. |
| Readers of coordinated-operation history (introduced 2026-10-07, schema v49, #393) | New rows include the additive `agent_provenance` JSON field with `schema_version`, caller and holder process pid/start/program basename, the caller's process ancestry through the agent root, the holder-binding event id, and session agent identity. Full process command lines are omitted because they may contain credentials. The nullable `agent_provenance_json` database column leaves legacy rows without the JSON field. | Consumers should ignore additive JSON fields. Schema v49 remains readable by v47 binaries because the new column is nullable and existing named-column writes continue to work; the compatibility floor remains v47. |
| Agents and wrappers deleting or rewriting a branch through `aethyme broker advanced gh` (introduced 2026-10-06, #393) | Before anything is journaled, `gh_ref_guard` compares tokens against an allowlist; it does not interpret gh options. **No ref write:** a short list of `(command, action)` pairs (for example `pr view|list|checks|comment|review|ready`, `issue view|list|status|comment|edit|close|reopen|create|lock|unlock|pin|unpin|transfer|delete` (not `issue develop`, whose `--checkout` runs a local git fetch and checkout), `run *`, `repo view|list`; never `browse` or `config`, and never with `--web`/`-w`, which runs a configurable browser;); `run download [<id>] [-n <name>] [-p <pattern>]` and `release download [<tag>] [-p <pattern>]` are accepted only from the session worktree root, and any destination (`-D`, `--dir`, `-O`, `--output`, `--clobber`, `--archive`, an extra operand) is unverifiable), exact `pr close <N>` and `pr reopen <N>`, each optionally with one `--comment <text>`/`-c <text>` whose text does not start with `-` (2026-10-07: without this, closing or reopening a pull request through the broker was refused outright once shared branches were protected), `pr edit` without a base option, and `gh api` reads made only of one non-GraphQL endpoint plus `--paginate`, `--slurp`, `-i`/`--include`, `--silent`, `-q`/`--jq`, `-t`/`--template` and `-H`/`--header` with an `Accept:` or `X-GitHub-Api-Version:` value. **Resolved:** exactly `pr merge <N> --merge|--squash|--rebase [-d|--delete-branch] [--match-head-commit <sha>]` (each once, nothing else), exactly `pr update-branch <N>` (its head is checked), `pr edit|create` with one separate `--base <name>`/`-B <name>` (the base is checked; any other base spelling is unverifiable), and exactly `api -X DELETE repos/<owner>/<repo>/git/refs/heads/<branch>` for the `--repo` target with plain branch characters. Every merge, with or without `-d` (a repository may delete heads on merge), reads `gh pr view <N> --json headRefName,headRefOid,baseRefName` once, checks the head and the base, and runs with `--match-head-commit <headRefOid>` injected (a caller sha must equal it), so GitHub refuses if the head moved; the API delete runs byte-identical. Both require the target to be this checkout's `origin` on github.com and no `GH_HOST` elsewhere. Another live session's branch is refused (exit 3; `--cross-session <owner>` overrides). **Writes that cannot touch a ref**, accepted without acknowledgement when the target is this checkout's `origin`: exactly `api -X <METHOD> repos/<owner>/<repo>/<path>` with one `-X`, plain `-f`/`-F key=value` fields (no `@file`), optional `--silent` and `-q`/`--jq`, and `<path>` one of `issues/<n>/comments` (POST), `issues/comments/<id>` (PATCH, DELETE), `pulls/<n>/comments` (POST), `pulls/<n>/comments/<id>/replies` (POST), `pulls/comments/<id>` (PATCH, DELETE), `pulls/<n>/reviews` (POST), `pulls/<n>/reviews/<id>` (PUT, PATCH, DELETE), `pulls/<n>/reviews/<id>/events` (POST), `issues/<n>/labels` (POST, PUT, DELETE), `issues/<n>/labels/<name>` (DELETE), and `issues/<n>` (PATCH of `title`, `body`, `state`, `labels[]`, `assignees[]` only), with decimal `<n>`/`<id>` and plain label names. `--destructive` rules are unchanged. **Everything else** that is not on the no-write list (clustered or `=`-joined flags, repeated flags, `--`, `pr close -d`, `pr update-branch --rebase`, `pr checkout`, `repo sync|rename|edit`, any other `gh api` method, fields or body, GraphQL, headers, encoded or slash-variant endpoints, aliases and extensions) is refused unless `--destructive --ref-write-acknowledged` is given, which the journal records; when a shared branch exists, an unclassifiable GH write is refused even with those flags. A lookup failure or malformed answer refuses. gh runs by an absolute path resolved once, in release builds only from `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` and `/bin`, and only if it is owned by root or the current user and writable by neither group nor others (debug builds, which only the test suite runs, also search `PATH` so fixtures can stand in a fake gh). The gh child (and any git it starts) inherits only auth, locale, proxy, certificate and `AETHYME_*` variables; `PATH` is exactly those four directories, `HOME` comes from the passwd entry, and it gets `GH_PROMPT_DISABLED=1`, `GH_NO_UPDATE_NOTIFIER=1`, `GH_PAGER=cat`, `GH_BROWSER=false`, `GH_EDITOR=false`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and command-scope `core.hooksPath=/dev/null`, `core.sshCommand=ssh`, `core.fsmonitor=false`, an empty `credential.helper` and `protocol.file.allow=never`. `pr create` must name its head with one separate `--head`/`-H` pair so gh never pushes; `extension install|upgrade` and `gist clone` are unverifiable; Known limits: merges triggered indirectly (review or label automation, workflows) are not guarded; `pr merge -d` runs gh's local branch cleanup in the session worktree (bounded by the head check and `--match-head-commit`); the repository's `include.path` cannot be disabled for gh's git; whether an extracted download asset can overwrite a linked worktree's `.git` file is unverified. | Wrappers using other spellings of these commands, `gh api` writes outside the allowlisted endpoints, or `-X GET`/field-based reads must switch to the exact forms or pass `--destructive --ref-write-acknowledged`. Every merge and `pr update-branch` adds one `gh pr view` read, a merge's executed argv gains `--match-head-commit`, and a foreign head or base is refused; `browse`, `config` and `--web` now need acknowledgement; the gh child no longer sees editor, browser, pager, host, config-dir or `GIT_*` variables. |
| Agents and wrappers that delete, force-update or advance shared branches through `broker advanced git`/`gh`, and agents running `broker start --adopt` (introduced 2026-10-07, #284 proposal 9, #393) | The repository's **shared branches** are its integration branch (`[promote] branch`, default `aethyme/integration`) and its default branch as `refs/remotes/origin/HEAD` (else the main checkout's upstream) names it; when neither resolves, `main` and `master` are both treated as shared. Matching is by exact branch name from the repository, never from a session record. A command that **deletes or rewrites** a shared branch is refused before anything is journaled (exit 3, "refusing to delete or rewrite <branch>: it is the repository's shared default or integration branch"), with no override: an explicit Git push refspec that deletes (`:dst`, `--delete`) or forces (`+src:dst`, `--force`/`-f`/`--force-with-lease`); implicit and set-expanding pushes (configured bare pushes, wildcard refspecs, `--all`/`--branches`/`--mirror`/`--prune`) are refused before journaling because they may include a shared branch. Full `refs/remotes/...` destinations are not branch heads. The guard also checks `branch -d`/`update-ref`, a gh API ref delete, `pr update-branch` and `pr merge` whose head is a shared branch. A plain fast-forward push to the default branch, and a merge **into** it, are advances: allowed, and never attributed to a session that happens to record the shared branch. `broker start --adopt` refuses to register a **new** session on the repository's main checkout (exit 3, "refusing to register a session on the main checkout", compared by the path git resolves plus device and inode) unless the operator explicitly passes `--allow-main-checkout` (environment variables do not authorize the exception); re-entering a session that already lives there (`--reuse`, `--take-over`) is unaffected. | A script that deleted or force-pushed the default or integration branch through the broker is now refused; do that outside the broker on purpose. A wrapper that adopts the main checkout for a new session must pass `--allow-main-checkout`. A session that adopted the main checkout before this change no longer blocks every merge into main as "belongs to live session N". |
| Readers of `broker start --json` and `broker adopt --json` tab resolution (introduced 2026-10-06, #496) | `tab_rename.status` gains `not_applicable` when the session has no nonblank tab name (from `--tab-name`, `AETHYME_SESSION_TAB_NAME` or `AETHYME_CHAU7_TAB_NAME`, or stored on a reused session); `start` and `adopt` launch no Chau7 tab, so they skip the MCP bridge. `start-agent` is unchanged. Existing `renamed`, `pending` and `refused` outcomes remain; an explicit tab name still triggers the lookup. | JSON consumers must accept the new status and should not retry it as a pending rename: the command did not launch or identify a tab. Covered by `aethyme-broker/tests/adopt_cli.rs::start_without_a_chau7_tab_skips_mcp_lookup` and `::adopted_reused_session_without_a_chau7_tab_skips_mcp_lookup`. |

| Readers of refreshed `broker status --json` and callers of `broker submit --session <id>` (introduced 2026-10-08, #640) | Status adds a `session.checkout-drift` advice row (`blocked`) when a live session's checkout branch or HEAD no longer matches its recorded branch identity; evidence names the recorded and actual branches and both heads. Dirty-worktree summaries count changes in the associated checkout, including an adopted primary checkout. Submit refuses before queueing when the current branch or HEAD no longer matches the recorded session branch, and rechecks the identity after planning; it never transfers ownership or chooses a replacement branch. | Consumers of `advice[].id` must tolerate `session.checkout-drift`. Preserve any checkout changes before restoring the recorded branch or closing and re-adopting under the actual branch; the refusal leaves no queue row. Covered by `aethyme-broker/tests/merge_e2e.rs` branch-drift status and submit tests. |

| Readers of `broker status --json` and callers of `broker submit --session <id>` (introduced 2026-10-10, #691; since 2026-11-12 every mode verifies only, see "Local promotion removed") | An unknown or non-string `.aethyme/config.toml` `[promote].mode` warns and falls back to `verify-only`; status adds `promote.mode-invalid` (`warning`), and submit includes the explanation in the existing `promotion_suppressed` value. Known modes and the default `auto` are unchanged. | Consumers of `advice[].id` must tolerate `promote.mode-invalid`. A typo cannot re-enable promotion; correct the configured mode to restore the intended policy. Covered by `aethyme-broker/tests/promote_mode.rs::unknown_mode_warns_in_status_and_submit_and_falls_back_to_verify_only`. |

### Environment build-cache and GC coordination (#1300)

Cached-build readers/builders and `broker gc apply` use the same host
exclusive key: the repository and cache key already used by build lookup. GC
retains an entry without a matching key rather than inferring a lock from its
path. It records deletion start, removes and syncs `.complete` before deleting
payload, and resumes only while that marker stays absent and the planned
directory identity and cache key still match. Readers therefore cannot
materialize an output after payload deletion starts. Filesystem inventory and
removal also stop when mount identity cannot be checked or changes during
descent.

Interactive `aethyme env cache prune` uses the same identity-bound inventory
and exact existing cache-key rule. Its descriptor-relative filesystem handling
and pinned-directory template hook runner are supported only on macOS and
Linux; on other platforms the command refuses before deleting entries or
running hooks.

### Affected gate-selection timing (#481)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Readers of `aethyme broker advanced gates affected --session <id> --json` and `.aethyme/logs/command-metrics.jsonl` | `--json` still prints the bare selection array (`[{gate, triggered_by}]`), unchanged. `--json --timings` opts into an object with `selected_gates[]` (that same array), `phase_timings_ms` (`graph_read`, `manifest`, `selection`, `lock_wait`), `phase_budget_ms` (5,000) and `over_budget_phases[]`. Phases above budget are named in a warning: on stdout in human output, on stderr with plain `--json` so stdout stays parseable. Every run records the same phase fields in its `gates.affected` command-metric row. This selection path bypasses the repository-wide graph-integrity lock, so `lock_wait` is zero. | Making the object the default would break every parser that expects an array; `aethyme-broker/tests/gates_cli.rs::affected_gates_json_stays_an_array_and_timings_are_opt_in` pins both forms. The command now emits a command-metric line with its timing report. |

### Worktree inventory report (introduced 2026-09-26, #335)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Stable broker JSON command contract (`docs/json-contracts.md`) | `aethyme broker advanced worktrees --json` reports disk usage and Git registration state, including detached, locked, and prunable worktrees. `git_registered` separates a confirmed absence from unreadable inventory; see the field contract for exact omission and error behavior. Since 2026-10-06 (#559) sizing is bounded by default: each row adds `size` (`measured`, `recorded` with `size_measured_at_ms`, or `unmeasured`), and the report adds `size_scan` (`bounded` or `measure`) and `unmeasured_count`; `--measure` walks every checkout. Work classification always completes. | Treating unreadable inventory as unregistered can hide Git state; treating a prunable registration as deletion permission can lose work. Consumers must preserve the distinction and treat the report as diagnostic. An `unmeasured` row's `bytes`/`inodes` are 0 and mean nothing, and the totals are floors while `unmeasured_count` is non-zero; summing them as complete under-reports disk use. |

### Branch audit report (introduced 2026-10-03)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Operators and scripts deciding which local branches to delete: `aethyme repo branches audit [<repo_path>] [--remote <name>] [--no-gh] [--json-output]` | Read-only. Each `branches[]` row has `name`, `tip`, `classification` (`on-remote`, `contained`, `merged-via-pr`, `local-only`), `evidence`, `protected` and `protected_reasons`; `local-only` rows add `local_commits[]` and `uncovered_commit_count`. Top level: `remote_source`, `remote_warning`, `default_branch`, `remote_tips_missing_locally[]`, `gh`, `summary`, `worktrees[]` (`branch`, `dirty_paths`, `prunable`, `broker_managed`). | Only `local-only` means unique work. A `local-only` row with `evidence.note` has a merged PR whose content did not match: read those commits before deleting. Non-empty `remote_tips_missing_locally` means containment was unprovable for those tips. The report never grants deletion permission for `protected` branches. |

### Console lifecycle (introduced 2026-09-27, #374 section 3)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Scripts and dashboards reading `aethyme broker advanced console status --json` | Each `running[]` row adds `checkout` (`present`, `missing`, `unknown`), `holder_alive` (bool or null), `stale`, `stale_reasons` (`worktree_missing`, `holder_gone`, `lease_expired`), and `stop_command` (set only when stale). The top level adds `stale_count`. | `checkout: unknown` means no verified marker, not a missing worktree; reading it as stale would ask for stops that `console stop` refuses. `stop_command` is null for a healthy console, so a consumer must not run it unconditionally. |
| Operators and automation calling `aethyme broker advanced console stop <lease-id> [--force] [--dry-run] [--wait <duration>] [--json]` | Sends SIGTERM to the supervising `console run` only, which stops the server's process group, releases the lease, and removes the marker. The JSON reports `outcome` (`stopped`, `would_signal`, `refused`, `signal_failed`, `signalled_not_released`), `reason`, `health`, and `holder_identity` (`proven`, `replaced`, `unproven`). The exit code is 0 only for `stopped` and `would_signal`. | `--force` relaxes only the refusal for a non-stale console. It never overrides an expired lease, a missing marker, or an unproven holder, so a caller that retries with `--force` after `refused` gets the same refusal. On a host without wall-clock process start times (Linux) every holder is `unproven`, so stop always refuses there; stop the supervisor manually after checking `ps -p <pid>`. |

### Managed gate cache provenance (introduced 2026-09-27, #391)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Readers of gate outcomes: `aethyme broker advanced gates run --json` (one `GateRunOutcome` per gate) and any wrapper auditing where a gate's artifacts came from | Each outcome of a gate with `[gate.managed_cache]` carries `managed_cache.sources_refreshed`: the number of the tree's files (tracked plus untracked, non-ignored; symlinks and deleted paths skipped) whose mtime the broker advanced to "now" before the command ran, so Cargo rebuilds every workspace member from the tree under test instead of reusing another worktree's artifacts from the shared `CARGO_TARGET_DIR`. Content is untouched, so the tree hash the verdict is keyed on does not change. A refresh that fails records the gate as `error`/`environment` without running the command. | Additive field. A reader that rejects unknown keys in `managed_cache` breaks. Removing the refresh, or making it best-effort, reopens #391: a gate can execute a foreign tree's library and record pass or fail against its own tree. Gates run by a broker older than the release carrying this field still share artifacts across worktrees. |
| Readers of gate outcomes and command metrics for gates with `[gate.managed_cache]` (introduced 2026-11-21) | A managed cache measured above its `max_bytes` before a run first moves its superseded cargo build units out of the tree: an older generation of a unit (same package, same unit kinds in `.fingerprint/<package>-<hash>/`) whose fingerprint nothing has read since the newest generation was built, oldest first, its fingerprint first, then its `deps/` and `examples/` files and its `build/` directory, until the tree is at most three quarters of `max_bytes` or 5 s have passed. The in-band step only renames, into one `<host cache>/.aethyme-storage-evicting-<pid>-<ms>-pruned` directory per prune; the storage worker deletes it in the background on its next pass (it removes every `.aethyme-storage-evicting-*` entry of the host cache root), and a gate whose host has the storage worker off (`AETHYME_STORAGE_LEDGER=0`, or an ephemeral host state) deletes it inline. `managed_cache.rotated_before_run` is `true` only when the tree is still over `max_bytes` after that, and then the whole tree is rotated as before (renamed and deleted inline). `managed_cache.bytes_before` stays the size measured before pruning. When it moved anything the gate prints `gate <name> pruned <n> superseded build units (<bytes> bytes) from managed cache <key>`, and the rotation line names the size after pruning. The prune records the staged units in `<cache>/.aethyme-pruned-units`; the next gate counts those cargo rebuilt with the same hash and removes the record. Command metric lines gain span `managed_cache_prune` (and `managed_cache_prune_inline_delete`), zero-duration counts `managed_cache_prune_staged_unit`, `managed_cache_prune_deadline_stop` and `managed_cache_prune_rebuilt_unit`, and attachment `managed_cache_prune` (`candidates`, `units`, `bytes`, `deadline_stopped`, `deletion_deferred`, and `rebuilt_after_previous_prune` on the gate after a prune). The prune runs inside the gate, under the cache's exclusive host resource lease the gate already holds. | Additive. A reader that took `rotated_before_run: false` with `bytes_before > max_bytes` to be impossible must now allow it. |

### Main reconcile fast-forward (introduced 2026-09-27, #374 section 1)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~Readers of `aethyme broker advanced main reconcile plan --json` and `apply --json`~~ (REMOVED 2026-11-12) | Hard-deleted with its `--detail`, `--resolution-file` and `--write-resolution-template` flags, the `MainReconcile*` types, and the `aethyme broker submit promotion-record plan|apply` recovery commands (with the `broker.promotion-record.restored` event they emitted). The owner decided on 2026-11-11 that pull requests are the only landing path; both commands only repaired state of the local integration landing path. | Both commands now fail as unknown commands. Existing `aethyme/preserve/*` refs created by an earlier reset apply are left untouched. Historical `broker.promotion-record.restored` rows stay readable in the event log; nothing writes new ones. |

### Status summary baseline (introduced 2026-09-27, #374 section 2)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| Readers of `aethyme broker status --json` / `status --summary --json` `summary`, including `packages/aethyme/docs/pilot/export-metrics.sh` | `summary` adds `integration_head`, `baseline_ref`, `baseline_head`, `main_head`, and `integration_ahead_local_main_commits`. `integration_relation` and `integration_ahead_main_commits` keep their names and meaning: they are counted against `baseline_ref` (the remote-tracking default branch, then the local default branch, then `HEAD`). `main_head` and `integration_ahead_local_main_commits` equal `integration status`'s `main_head` and `commits_ahead_main`. `summary.message` now names the baseline (`current with origin/main`, or `checkout` for the `HEAD` fallback) instead of the word `main`, and appends the local checkout's position when it differs from the baseline. | Comparing `integration_ahead_main_commits` with `integration status`'s `commits_ahead_main` compares two different baselines. After an unpushed local fast-forward the first still counts the unpublished commits while the second is 0; compare `integration_ahead_local_main_commits` instead. A consumer that string-matches `current with main` in `message` must match the named baseline or read `integration_relation`. |

### Installed git hooks (`aethyme broker advanced hooks install`, 2026-07-17)

| Source | Invokes | Failure mode |
|---|---|---|
| `<git-common-dir>/hooks/pre-commit` and `post-commit` marker blocks (`# >>> aethyme hooks >>>`), written by `aethyme broker advanced hooks install` into **every repo where an operator opted in** — canonical template: `rust/crates/aethyme-broker/src/hooks.rs:hook_block` | `"<abs-path-to-aethyme-captured-at-install>" broker hooks pre-commit` (blocking, `\|\| exit $?`) and `... broker hooks post-commit` (`\|\| true`) | **Renaming/removing the `broker hooks pre-commit` subcommand blocks ALL commits** in every repo with hooks installed: the shim treats any non-zero exit — including "unknown subcommand" — as a failed gate. `post-commit` degrades silently. A moved/deleted *binary* is handled gracefully by the shims (warn-and-pass / silent skip). Escape hatches: `git commit --no-verify`, `aethyme broker advanced hooks uninstall`, or deleting the marker block by hand. |
| Operators and wrappers approving repository-defined commands (introduced 2026-09-23) | `aethyme broker advanced trust [--repo <path>] [--json]` records SHA-256 digests of the checkout's and integration tip's gate/prepare policy in `<host-state>/gate-trust/<repository-key>.json` after an interactive confirmation; `aethyme broker advanced trust status [--repo <path>] [--json]` is its read-only view. `submit`, `gates run`, `gates pre-push`, `gates doctor --probe`, `prepare` and the installed `broker hooks pre-commit` refuse an untrusted policy with exit 3 and a message naming `aethyme broker advanced trust --repo <path>`. `AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS=1` is a test-only escape, set for `cargo test` by the repository's `.cargo/config.toml`. | `trust` refuses without a TTY on stdin by design, so automation cannot approve a policy; a wrapper that pipes `y` into it gets exit 3. Renaming `broker advanced trust`, the refusal text, or the exit code strands every new clone and every landed gates change behind a refusal nobody can clear. Repositories that already had gate history are grandfathered with their current policy; a clone without history needs `trust` once before its first gated submit, commit through the hook, or prepare. |
| Repository-owned Git hooks run by a brokered `git push` (`aethyme broker advanced git … -- push …`) (introduced 2026-10-06, #264) | The broker sets two environment variables on every brokered `git push`, for the dry run and for the push itself, overriding any caller value: `AETHYME_PUSH_KIND` is `delete-only` (every refspec deletes: `--delete`/`-d` with plain ref names, or `:<dst>` refspecs), `update` (every refspec sends content), or `mixed` (both, or a shape the broker cannot classify with certainty: no refspec, `--all`, `--mirror`, `--tags`, `--prune`, `--follow-tags`, an unknown option, or `:` alone); `AETHYME_PUSH_REFS` is the refspecs exactly as given, newline-separated. For a `delete-only` push the broker runs no dry run of its own even when `[coordination] hooks_outside_lock = true`; the repository's `pre-push` hook still runs once inside the push. The broker never passes `--no-verify` for it. | A hook may short-circuit on `delete-only`, since a deletion sends no content. It must treat `mixed` and an unset variable (a push not made through the broker) like `update` and verify as before. Renaming either variable or a kind string, or classifying an uncertain shape as anything but `mixed`, makes hooks skip verification of content they were never shown. Covered by the `push_kind_*` unit tests in `aethyme-broker/src/operations.rs` and the `a_delete_only_push_…`, `an_update_push_…` and `a_mixed_push_…` cases in `aethyme-broker/tests/operations_e2e.rs`. |
| Repository-owned pre-push hooks following `docs/guides/host-resource-coordination.md` | `aethyme broker advanced gates pre-push "$@"` with Git's ref-update protocol preserved on stdin. Aethyme deliberately does not install this hook. | Renaming/removing `broker advanced gates pre-push`, consuming stdin before delegation, or changing its clean-current-HEAD proof can either block opted-in pushes or validate a tree other than the outgoing tip. Repositories may provide an explicit isolated fallback when the binary is absent; they must not retry after Aethyme starts and partially executes. |
| Agent-surface plugin hooks: `plugins/aethyme/hooks/aethyme-hook.sh`, installed globally by the Claude Code / Codex plugin and wired for `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse` and `Stop` (updated 2026-09-25, recovery plan P4.7; coordination notes 2026-10-01) | `aethyme hook <event> --repo <git-toplevel>` with the event JSON on stdin; stdout is exactly one `hookSpecificOutput` envelope line or nothing, and the exit code is always 0. `SessionStart` now carries **state plus one next command** instead of policy prose: an unregistered checkout gets `Aethyme: this checkout (<path>) is not a broker session; <n> live sessions (...)` then `Next: aethyme broker start --task "<task>" --short-name "<short name>" ...` (or `Next: cd '<worktree>'` when a live session is registered for the host tab named by `AETHYME_SESSION_TAB_NAME`/`AETHYME_CHAU7_TAB_NAME`); a finished session gets `Next: aethyme broker start --reuse --task ... --short-name ...`; an open session gets `Aethyme: session <id>: <task>`, `Worktree: <path>`, `State: <peers>; <blockers>; <advisories>; <commits to integrate>.` and `Next:` — the first own or repository-wide blocker's `clear` command, else `aethyme broker submit --session <id>` when the worktree holds commits the integration branch does not, else `aethyme broker status --json` when advisories are outstanding. At most five lines in the normal case; the installation notice (split router/engine, a running process build differing from the PATH-selected router, a newer release, or — for Claude Code only — a missing or mismatched cached `hooks/hooks.json` / `hooks/aethyme-hook.sh`) is appended after a blank line. The stale-plugin notice names `claude plugin marketplace update aethyme && claude plugin update aethyme@aethyme`, with `claude plugin uninstall aethyme@aethyme && claude plugin install aethyme@aethyme` when Claude says it is already current. It reads the installed hook files against the copies embedded in the CLI; it performs no writes or network calls. Local reads only (broker DB, blocker registry, two `git rev-list`-class reads, plugin files, and direct metadata checks in at most five fixed install roots), no network. On a process/PATH mismatch the notice reports this hook's build and executable path, the PATH-selected router build and path, and the known roots with direct router/engine entries; it does not walk install directories or execute binaries from other roots. Since 2026-10-01 (coordination): `SessionStart` may add one `Overlap: Session(s) <id> (<short name>) … changing <n> files you lease: … You'll get details when you edit them.` line; `PreToolUse` on Claude Code `Edit`/`Write`/`MultiEdit`/`NotebookEdit` or Codex `apply_patch` emits `additionalContext` (never `permissionDecision: deny` any more) of the form `Aethyme: session <id> (<short name>) working on "<task>" is also changing `<path>` (lines <spans> in `<symbol>`). <verdict>` plus a `coordinate: aethyme broker advanced note send --session <me> --to-session <id> --message "…"` line, once per (path, other session, its change), deduplicated in `<git-dir>/aethyme/hook-seen-<session>.json`; `UserPromptSubmit` and `PostToolUse` deliver unread notes as `Note from session <id>: <text>` plus a `reply:` command, acknowledging them on delivery. | The `Next:` line is read by agents and possibly by wrappers that extract it; changing its prefix or the command spellings it names is a text-contract change. Printing anything but one envelope line corrupts the agent session. Covered by `aethyme_broker::agent_hook` unit tests (rendering and next-action precedence), `aethyme-cli/tests/session_start_brief_cli.rs` (built router against a real broker fixture), and `aethyme-cli/tests/hook_install_health_cli.rs` (envelope shape, installation notice, no network). |

### Rust `aethyme` router ↔ Python daemon (REMOVED 2026-07-13, issue #29)

| Source | Invokes / assumes | Failure mode |
|---|---|---|
| ~~`rust/crates/aethyme-engine/src/bin/aethyme.rs` ↔ `src/daemon.py`~~ | Both sides removed in the same commit (the coordinated change this row demanded). The router's repo-daemon subcommand, Python-daemon socket fallback, and socket-path logic are gone. `src/daemon.py` and its `cli.py` registration are deleted. `aethyme explore` is a native-engine-owned surface. Non-explore commands no longer fall through to Python at all — the delegation path went with `src/` in Phase 6 (2026-08-01). Since 2026-07-14 (#31) explore runs **in-process** in the router via `aethyme_engine::explore_cli`, auto-starting the engine daemon (spawning the sibling `aethyme-engine-cli daemon serve`). | Any out-of-repo script calling `aethyme daemon start/stop/status` (repo-socket flavor, `aethyme.sock` / `aethyme-socket.path`) now falls through to the Python CLI, which reports an unknown command. The **engine** daemon (`aethyme-engine-cli daemon ...`, `engine-<hash>.sock`, eval warm phase) is a separate live contract and is unaffected — see the engine-daemon rows above. |

| Workflow | Calls | Failure mode |
|---|---|---|
| `.github/workflows/aethyme-local-tests.yml` | **Manual diagnostic lane since 2026-09-18**, introducing `workflow_dispatch`. Builds `aethyme`, `aethyme-engine-cli` and `aethyme-graph-index` with the lockfile, then runs `cargo test --locked -p aethyme-cli -p aethyme-testkit`. These suites remain included in automatic full-workspace runs; `aethyme_testkit::bins` asserts on failed builds rather than skipping. | Suites fail if a binary target is renamed. No automatic PR/push run is expected here; use the Actions Run workflow control for an isolated diagnostic. Schedule ownership is guarded by `ci_validation.rs`. |
| `.github/workflows/oss-ci.yml` | Runs `cargo test --locked --workspace` on PRs. The independent `product-path-no-python` job remains automatic on both PRs and main/master pushes: it installs the router, engine sibling, and graph indexer, then runs canonical `aethyme deploy` and read-only `deploy verify` behind a sandbox PATH with no interpreter. It rejects source-build and target-checkout paths in generated policy before exercising the remaining product surface. | Catches interpreter regressions, missing embedded deployment assets, non-portable policy, or renamed crate/bin/command surfaces. Main workspace coverage belongs to Aethyme Gates; `ci_validation.rs` guards that division. |
| `.github/workflows/product-performance.yml` (introduced 2026-10-08, #386) | Nightly and manual runs compare Criterion latency distributions for the current router and graph indexer against binaries built from the recorded reference product commit, using the same checked-in Playground fixture. The workflow reads the reference commit and fixture tree SHA from `docs/reports/product-performance-baseline-2026-10-08.md`, runs `scripts/bench-product-latency.sh`, and uploads Criterion results as a non-gating artifact. | Changing the report keys, benchmark command options, fixture identity, or executable pair can make revision comparisons invalid; the workflow intentionally reports estimates and does not enforce a noisy timing threshold. Coverage: `aethyme-cli/benches/product_latency.rs`, `scripts/bench-product-latency.sh`, and the workflow itself. |
| `.github/workflows/cross-process-contract.yml` | Builds the router from the checkout (`cargo run … -p aethyme-cli --bin aethyme`) and runs `aethyme broker check-contract --base ... --pr-body ... --commit-messages`; the checker treats this document as the source of truth for protected entry-point strings. Native since python-retirement Phase 6 (2026-08-01). **Since 2026-10-04 (introduced: `--commit-messages`, `--merged-pr`)** the PR check and the `cross-process-contract` gate read the same decision sources: the gate passes `--commit-messages --merged-pr`, so on a main push it also reads the body of the pull request GitHub associates with HEAD (`gh api repos/{owner}/{repo}/commits/<sha>/pulls`), and a decision accepted on the PR is accepted on its merge commit. A refusal lists every source read; a lookup that cannot run is reported as unavailable with its cause and never passes. | PR contract check becomes stale if this registry misses a consumer or if the PR-template contract language changes without updating `aethyme-broker/src/contract_check.rs`. Breaks if the `broker check-contract` subcommand or the `aethyme` bin target is renamed. |
| ~~`scripts/check-cross-process-contract.py`~~ (REMOVED 2026-08-01, retirement Phase 6) | The Python contract checker was ported to `aethyme-broker/src/contract_check.rs` and is now reached as `aethyme broker check-contract`. Behaviour is unchanged and was verified byte-identical (stdout, stderr, exit codes) against the Python implementation over the clean / undeclared-removal / declared-decision cases before deletion. Entry points at deletion: `.github/workflows/cross-process-contract.yml` and the `cross-process-contract` gate in `.aethyme/gates.toml` (both updated in the same commit); `tests/local/test_cross_process_contract_check.py` (replaced by unit tests inside the Rust module, including the two live-registry assertions). The port had to precede the `src/` deletion — otherwise the migration's largest removal would have landed with its own guard switched off. | Out-of-repo automation invoking `python scripts/check-cross-process-contract.py` gets "No such file". Migrate to `aethyme broker check-contract` (same `--base` / `--pr-body` flags, same exit codes; `--consumers-doc` added to override the registry path). |
| Diff-dependent gate commands | Submit sets `AETHYME_GATE_BASE_COMMIT` to the immutable commit used to select gates for the verified candidate. Standalone gate runs clear it, including an inherited value. Consumers of this commit must set `cache = false` because a gate verdict cache key only covers the candidate tree, gate definition and execution profile. | `fast-guards` uses the supplied commit to select changed tests. A remote-tracking ref could move after the broker selected the verification candidate. |
| Gate-result readers and harnesses nested in gates: `gates run --json` consumers, gate-log readers, and tests or tools a gate command spawns (introduced 2026-09-27, #361; extended 2026-10-17, #827) | Each executed `gates run --json` result gains an additive `broker_database` object -- `path` (the disposable database), `kind` (`"disposable"`), `protected_repositories` (canonical roots whose shared `.aethyme/broker.db` the command is refused), `retention` (`"removed_on_gate_exit"`) -- omitted for cache hits and runs refused before the command. The same object is written to the gate log before any command output as one line, `aethyme gate environment: broker database <json>`. Inside a gate (`AETHYME_GATE_BROKER_DATABASES` set), resolution fails closed: a protected repository resolves to the disposable database even if `AETHYME_BROKER_DB` was removed or blanked, an `AETHYME_BROKER_DB` naming a protected shared database is refused, and an unreadable scope is refused rather than ignored. A protected database whose schema changes while the gate runs invalidates the run (`error`/`environment`, log line `aethyme invalidated this gate: ...`). Gate stdout/stderr logs retain at most 4 MiB; after that cap the broker continues draining both pipes and preserving classifier evidence from the complete drained stream. `output_bytes` counts bytes observed, including omitted bytes. | A harness that relied on removing `AETHYME_BROKER_DB` to reach the checkout's real database inside a gate now gets the disposable one; that path was the #361 hole. A wrapper that exports an unreadable scope now fails before any store opens. Renaming the log prefix or the JSON keys strands readers that audit which database a verdict could have written. Consumers must treat `log_path` as a bounded diagnostic excerpt, not a complete transcript; `output_bytes` is not the retained file length. Incomplete pipe drain after the 2-second grace is reported as `error`/`environment` with `host_fault: true`; host-resource renewal or log-storage failures are also host faults and may use `resource_contention`. Covered by `aethyme-broker/tests/gate_database_isolation.rs` and `gate_database.rs` unit tests. |
| `.github/workflows/aethyme-gates.yml` | Runs on main/master pushes and the new manual `workflow_dispatch` surface (2026-09-18). Sets up Python for the self-provisioning `pytest-aethyme-eval` gate, builds the router with `cargo build --locked --release --manifest-path packages/aethyme/rust/Cargo.toml --bin aethyme`, then runs every configured gate from the repo root with `aethyme broker advanced gates run --all --only <gate>`. **Since 2026-11-05** the `cargo-test` gate is the exception on this workflow: its `cargo-test-*` jobs run the same suites as OSS CI (one nextest archive, twelve partitions, examples and doctests), and the broker keeps running the gate's command for local submissions. `fast-guards`, `workflow-contract` and `gate-policy` are subsets of that suite, so this workflow does not repeat them. Emits only existing `gate.*` events. Since 2026-10-04 the job holds `pull-requests: read` and exports `GH_TOKEN` so the `cross-process-contract` gate can read the merged PR's body. | Breaks if the `broker advanced gates run --all` subcommand, the `aethyme` bin target, or `.aethyme/gates.toml` is renamed/moved. PR workspace coverage stays in OSS CI. Superseded runs are cancelled; release only from an exact tree with completed proof. |
| `packages/aethyme/.github/workflows/*` (REMOVED 2026-07-09) | The package-level workflow set (`ci.yml`, `evals.yml`, `performance.yml`, `cd.yml`, `aethyme-example.yml`) and the `aethyme-scorecard` action were deleted in the Phase 0 truth cleanup. GitHub never executes workflows outside the repo-root `.github/workflows/`, `evals.yml`/`ci.yml` referenced test paths (`tests/evals/`, `tests/integration/`) that no longer exist, and the scorecard action ran `pip install aethyme-cli` — a PyPI name this repo does not publish. | Any external repository referencing `uses: .../packages/aethyme/.github/actions/aethyme-scorecard@main` was already broken by the wrong pip package name; such callers must pin an old commit or migrate to invoking `aethyme ai-ready` directly. |

### Externally deployed runtime files (under `~/Downloads/Repositories/Playground/`)

These are files in *separate repos* (the playground clones), put there
by `deploy_skills` / `aethyme deploy`. For benchmark clones, static skill
deployment remains the compatibility path; for normal repositories,
`aethyme deploy` is the primary path because it also configures and certifies
the broker contract.

| Path (under each `<Playground>/<repo> - Aethyme/`) | Source template | Last verified |
|---|---|---|
| `.codex/skills/aethyme/SKILL.md` | `skills/aethyme/SKILL.md` | 2026-07-27 (short-skill redeploy) |
| `.codex/skills/aethyme/references/*.md` | `skills/aethyme/references/*.md` | 2026-07-27 (short-skill redeploy) |
| `.claude/skills/aethyme/aethyme-explore`, `.codex/skills/aethyme/aethyme-explore` | `skills/aethyme/aethyme-explore` | 2026-10-05 (parity restored; checked by `skill_surface_parity.rs`) |
| `AGENTS.md`, `CLAUDE.md` | `skills/aethyme/AGENTS.md` | 2026-07-27 |

### Phase 4+ graph indexer and redb graph store

These entry points belong to the Phase 1–4 graph rewrite (new
`aethyme-graph-schema`, `aethyme-graph-storage`, `aethyme-graph-indexer`
crates) and the Phase-3 redb graph store. The fragment path is now
required by the engine: the legacy pass pipeline was deleted in 4.7.12.
The durable graph contract is `.aethyme/graph/`; it is the committed
source of truth. The redb graph store at `.aethyme/graph_store.redb`
is only a derived local query artifact written by
`aethyme-engine-cli index`.

| Entry point | Source | Wire shape that becomes a contract |
|---|---|---|
| `aethyme-graph-index` binary | `crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` | `--repo-root`, `--repo-name`, `--engine-version`, `--skip-bootstrap`, `--max-file-size`, `--extra-ignore`, `--json` argv surface; text/JSON summary stdout; nonzero exit on failure. Eval harnesses, CI gates, and downstream scripts depend on this shape. |
| Per-file binary fragments at `<repo>/.aethyme/graph/<source>.bin` | Produced by `aethyme-graph-storage::write_fragment` | Bincode 1 of `Fragment { file_path, schema_version=1, nodes, edges }`. Tagged-enum `EdgeAttributes` uses serde's *externally tagged* form (Phase 2.1 decision); changing it is a forever-format break. |
| Per-module NDJSON shards at `<repo>/.aethyme/graph/_index/<module>.ndjson` | Produced by `aethyme-graph-storage::write_index_shard` | One `SymbolRecord` per line: `{module, symbol, kind, node_id, file}`. Sorted canonically. `merge=union` git attribute relies on the line-based form. |
| `<repo>/.aethyme/engine-version` | Produced by `aethyme-graph-storage::bootstrap_repo` | Plain text, single line, no padding. CI's parser-version-drift check reads this; downgrading or empty-on-trim is an error. |
| `<repo>/.aethyme/graph/.gitattributes` | Constant `aethyme_graph_storage::GITATTRIBUTES_CONTENT` | Two rules: `**/*.bin linguist-generated=true binary` and `_index/**/*.ndjson linguist-generated=true merge=union`. Git itself is the cross-process consumer. |
| `<repo>/.aethyme/graph_store.redb` | Written by `aethyme-engine-cli index --repo <repo>` through `store::redb::graph_store::GraphStore::reset/open`; read by query-only CLI commands through `GraphStore::open_read_only` / redb `ReadOnlyDatabase`. `index --compact` is an opt-in post-index experiment. `index --disposable-fast` writes `<repo>/.aethyme/graph_store.redb.indexing` first, then publishes it over `graph_store.redb` after the final durable metadata commit. | Playground setup and verification assert this file exists. Do not rename or relocate without updating both scripts and docs. redb read-only handles do not replace the single-writer contract; run query-only commands after the index writer has released the store. Keep compaction default-off unless MediaWiki measurements show a real persistent size reduction without meaningful latency cost. Keep disposable-fast default-off until interruption/recovery behavior has more soak time; no external consumer should depend on the `.indexing` staging file. |
| `aethyme-engine-cli --from-fragments` | Compatibility spelling in `src/bin/aethyme-engine-cli.rs`; it now selects the same fragments-only build surface as the default. | Existing diagnostics/tests may keep passing it, but it no longer bypasses or proves a separate fallback. |
| `aethyme-engine-cli --no-fragments` | Removed rollback flag retained as a hard error in `src/bin/aethyme-engine-cli.rs`; `daemon.rs` also rejects non-fragment builds. | No in-repo manifest or workflow passed it in the 4.7.9 audit. Out-of-repo callers must remove the flag and ensure `<repo>/.aethyme/graph/` exists. |

### Redb graph-store contract

`<repo>/.aethyme/graph_store.redb` is a stable local artifact path. It is
not a committed graph format, not the source of truth, and must remain
derived from committed fragments under `<repo>/.aethyme/graph/`.

Supported redb surfaces:

| Surface | Access | Contract |
|---|---|---|
| `index` | Writer | `aethyme-engine-cli index --repo <repo>` rebuilds only the derived redb store from `.aethyme/graph/` fragments. It must not modify fragments. |
| `query-areas` | Read-only | Reads area rows through `GraphStore::open_read_only` / redb `ReadOnlyDatabase`. |
| `query-overview` | Read-only | Reads repo metadata, depth-1 areas, entrypoints, and risks through the redb store. |
| `symbol` | Read-only | Reads bounded V2 function/class symbol matches from redb through exact-name, case-insensitive, prefix, component, path-component, area, and basename signals. It does not build `RepositoryMap`. |
| `symbol-batch` | Read-only | Runs the same redb-backed V2 symbol matcher for multiple queries. It does not build `RepositoryMap`. |
| `graph-node` | Read-only | Renders one node through redb display projections and exact target resolution. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `graph-children` / `graph-parents` | Read-only | Render structural relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-callers` / `graph-callees` | Read-only | Render call relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-docs` / `graph-configs` | Read-only | Render document/config relations through redb relation views. They preserve the existing JSON shape and do not build `RepositoryMap`. |
| `graph-expand` | Read-only | Composes the redb-backed node, relation, and risk views into the existing compact expand JSON shape. It preserves the existing bounds and does not build `RepositoryMap`. |
| `graph-overview` | Read-only | Renders the existing graph overview JSON shape from redb overview/navigation rows. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-expand` | Read-only | Composes redb-backed callers/callees, docs/configs, and risk views into the existing compact task expansion JSON shape. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-anchors` | Read-only | Resolves task anchors from redb overview rows, path indexes, config/doc rows, and bounded symbol candidates. Ranking policy remains in `graph::anchors`; the command preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-scope` | Read-only | Builds task scope from redb-backed anchors, path-prefix lookups, symbol rows, area membership, and risk lookup. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-next` | Read-only | Builds task navigation order from redb-backed anchors, relation views, semantic config/doc path resolution, and bounded overview slices. It preserves the existing JSON shape and does not build `RepositoryMap`. |
| `task-localize` | Read-only | Composes redb-backed `task-anchors`, `task-scope`, and `task-next` outputs. `--profile` reports redb open / task parse / anchors / scope / next / JSON stages instead of `RepositoryMap` build time. |
| `pack` / `task-pack` | Read-only + source snippets | Selects context-pack inputs from redb anchors, scope, relation, docs/config, risk, symbol, and path rows, then reads source files only to supply snippets. It does not build `RepositoryMap` in production. |
| `context` / `task-context` | Read-only + source content | Uses the same redb-selected context-pack inputs as `pack`, then reads source text for bounded content. It does not build `RepositoryMap` in production. |
| `explain` / `task-explain` | Read-only + source snippets | Renders the redb context-pack summary as text. It does not build `RepositoryMap` in production. |
| `activate` / `activate-from` / `impact` | Read-only | Expands activation and impact frontiers through redb anchors, adjacency, relations, docs/configs, area, and risk rows. It does not build `RepositoryMap` in production. |
| `explore` non-usage-boundary intents | Read-only | Native `task_localization_query`, `behavior_localization_query`, and auto-selected explore flows read graph/navigation data from redb and report redb store freshness plus `surface_flow_graph` coverage in observability. Coverage is separate from freshness: a fresh store can still report `source_present_not_indexed` or `partially_indexed` for missing Surface/Flow families such as edge/proxy ingress. Surface/Flow `indexed` means semantic node/edge evidence; path-only fragment evidence is reported separately as `path_indexed`. They do not build `RepositoryMap` in production. |
| `explore --intent usage_boundary_query` | Hybrid redb + source text | Uses the usage-boundary analyzer contract below through the shared explore CLI. It does not build `RepositoryMap` and scans source/docs/config text for evidence. |
| `analyze-usage-boundary` | Hybrid redb + source text | Reads public PHP symbol seeds and candidate source/docs/config files from redb, then scans source/docs/config text for evidence. This is the accepted V2 contract because evidence strings are freshness-sensitive; a fully redb-native variant would need persisted evidence rows plus freshness/invalidation rules. It does not build `RepositoryMap` and fails cleanly when the store is missing. |
| `deps` | Read-only | Reads outgoing file adjacency from the redb store. |
| `importers` | Read-only | Reads incoming file adjacency from the redb store. |
| `callers` | Hybrid grep + graph | Greps for the requested symbol, then uses redb adjacency to expand candidate files. It is not a pure redb symbol-query contract in V1. |

Current storage coverage and limitations:

- Schema version `5` means the `index` writer populates repositories,
  directories, files, areas, functions, classes, docs, configs,
  unresolved/import placeholders, risks, `functions_by_path`, and
  `symbol_by_name` / `symbol_by_component` /
  `symbol_by_path_component`.
- The `index` writer persists the graph edge set without skipping edges for
  missing unresolved/import endpoint rows. Placeholder endpoints are stored as
  typed unresolved rows before adjacency is written.
- Task anchors, task scope, task next, task-localize, task-expand, graph
  overview, context-pack assembly, activation, impact, non-usage-boundary
  explore flows, and usage-boundary seed discovery are served from read-only
  redb rows. Usage-boundary still scans source/docs/config text for evidence
  after redb discovers public symbols and candidate files.

Remaining V2 target contract:

- No ownership change: `.aethyme/graph/` fragments remain the durable source
  of truth, while `.aethyme/graph_store.redb` remains derived, local, and
  rebuildable.
- The V2 writer must persist any remaining graph-navigation node kinds:
  separately represented methods if they stop being represented as functions,
  modules if they are introduced as separate containers, and any future
  container rows required for prefix or parent/child navigation.
- The V2 writer must persist the full graph-navigation edge set for every
  typed node kind it claims to support. It must retain incoming and outgoing
  adjacency for every persisted edge kind.
- The V2 read contract now covers typed node lookup, batch node lookup,
  display projections, area/risk/doc/config lookup, relation views,
  symbol matching with exact/prefix/component/path/area signals,
  path-prefix lookup, incoming/outgoing adjacency, task anchor candidate
  queries, usage-boundary seed discovery, and bounded overview/navigation
  slices.
- A CLI surface is not redb-backed merely because V2 tables exist. It is
  redb-backed only after the implementation reads through
  `GraphStore::open_read_only` / `ReadOnlyGraphStore` and has parity coverage
  against the current `RepositoryMap` output.

File-format policy:

- The engine currently builds against redb 4.x. The redb file format is
  owned by redb, not by Aethyme, so Aethyme does not promise in-place
  migration for old `graph_store.redb` files.
- If redb reports `UpgradeRequired(found)`, Aethyme treats the file as
  disposable and incompatible with the current engine. The operator fix is
  to regenerate it from fragments with `aethyme-engine-cli index --repo
  <repo>`.
- Query-only commands report the incompatibility and stop. They must not
  delete or regenerate the store because they are read-only consumers.

Regeneration behavior:

- `aethyme-engine-cli index --repo <repo>` is the normal writer. It detects
  old redb file formats, prints an operator message, deletes/recreates
  only `.aethyme/graph_store.redb`, and rebuilds it from
  `.aethyme/graph/`. It must not modify `.aethyme/graph/` fragments.
- `index --compact` is an opt-in post-index experiment on the same final
  store path. It is not enabled by default.
- `index --disposable-fast` is an opt-in staged writer. It writes
  `.aethyme/graph_store.redb.indexing` first, uses relaxed durability for
  bulk graph rows, performs a final durable metadata write, then publishes
  the staged file over `graph_store.redb`. The `.indexing` path is private
  and must not become a cross-process dependency.
- Normal `index` also removes stale `.indexing` files before rebuilding the
  public store, so a previously interrupted staged build does not affect
  the default writer.

Reader/writer split:

| Store access | Commands / callers | Required handle |
|---|---|---|
| Write/rebuild final store | `aethyme-engine-cli index --repo <repo>`; indirectly `scripts/eval/setup-playground.sh` after `aethyme-graph-index` writes fragments | Writable `GraphStore::reset/open` |
| Write staged store then publish | `aethyme-engine-cli index --disposable-fast --repo <repo>` | Writable `GraphStore::reset_staging`, then `publish_staging` after close |
| Optional post-write compaction | `aethyme-engine-cli index --compact --repo <repo>` | Writable `GraphStore::compact` after all write transactions commit |
| Read areas / overview | `query-areas`, `query-overview` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read symbols | `symbol`, `symbol-batch` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read task navigation | `task-anchors`, `task-scope`, `task-next`, `task-localize`, `task-expand` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Read adjacency | `importers`, `deps` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` |
| Hybrid usage-boundary | `analyze-usage-boundary`, `explore --intent usage_boundary_query` | `GraphStore::open_read_only` / redb `ReadOnlyDatabase` for seeds, source/docs/config text for fresh evidence |
| Hybrid grep + adjacency | `callers` | Grep first, then `GraphStore::open_read_only` / redb `ReadOnlyDatabase` for candidate expansion |
| Assert artifact exists and is locally ignored in playgrounds | `scripts/eval/setup-playground.sh`, `scripts/eval/verify-playground.sh`, `docs/guides/playground-setup.md` | Filesystem existence check plus `.git/info/exclude` visibility check in playground Aethyme clones |

Read-only open policy:

- Query-only CLI commands must use `GraphStore::open_read_only`. This is
  required, not optional: read-only commands must not create `.aethyme/`,
  initialize empty stores, repair incompatible files, or take a writable
  database handle.
- Writable `GraphStore::open/reset` is only for indexing, staging,
  schema initialization, tests that intentionally mutate the store, and
  future writer-only maintenance commands.
- Read-only opens reduce accidental writer contention, but they do not
  override redb's single-writer/file-lock behavior. Operators should run
  query-only commands after the index writer has released the store.

### Phase 4.7.9 redb/cache deletion audit

Audit command run on 2026-05-28:

```bash
rg -n -- "map_cache|AETHYME_MAP_CACHE_MAX_MB|parse_store\.redb|ParseStore|\.aethyme/cache|graph_store\.redb|--from-fragments|--no-fragments|build_from_fragments|build_with_fragment_preference" packages/aethyme
rg -n -- "aethyme-engine-cli|target/release/aethyme-engine-cli|target/debug/aethyme-engine-cli|AETHYME_ENGINE|ENGINE=|engine_bin|daemon start|daemon serve|build-profile" packages/aethyme
rg -n -- "AETHYME_CACHE_DIR|CACHE_ROOT|clear_repository_cache|_cached_text|_run_binary_command\(" packages/aethyme/src packages/aethyme/tests packages/aethyme/docs
rg -n -- "cache_dir\(|CACHE_SUBDIR|\.aethyme/cache|AETHYME_MAP_CACHE_MAX_MB" packages/aethyme/rust packages/aethyme/src packages/aethyme/scripts packages/aethyme/docs packages/aethyme/tests
```

Findings:

| Surface | Confirmed consumers | 4.7 deletion implication |
|---|---|---|
| Rust `map_cache.rs` and `AETHYME_MAP_CACHE_MAX_MB` | Rust engine internals only: before 4.7.10, `map.rs` loaded/saved the cache and `lib.rs` exported the module. No Python, shell, eval manifest, or skill consumer found. | Deleted in 4.7.10. This does not affect Python's separate `/tmp/aethyme-cache` output cache. |
| `<repo>/.aethyme/cache/map-*.bin` | Deleted Rust `map_cache.rs` only. `aethyme-graph-storage::cache_dir` also reserves `.aethyme/cache` as a layout helper for future local mirrors, but no external script consumes files in that directory. | 4.7.10 may orphan old `map-*.bin` files. Do not delete or rename the generic `cache_dir` layout helper; it belongs to graph-storage layout, not the removed map-cache implementation. |
| `ParseStore` and `<repo>/.aethyme/parse_store.redb` | Rust engine internals only before 4.7.11. No Python, shell, eval manifest, or skill consumer found. | Deleted in 4.7.11; the legacy passes that used it were deleted in 4.7.12. The on-disk `parse_store.redb` is now an orphan local cache file. |
| `<repo>/.aethyme/graph_store.redb` | `scripts/eval/setup-playground.sh`, `scripts/eval/verify-playground.sh`, `docs/guides/playground-setup.md`, `docs/architecture/graph-schema.md`, and `store/redb/graph_store.rs`. | Keep stable. It is the externally asserted redb graph-store artifact and is separate from deleted `ParseStore`. |
| Engine CLI fragment flags | `aethyme-engine-cli.rs` accepts `--from-fragments` as compatibility spelling and rejects `--no-fragments`. No external manifest currently passes either flag. | 4.7.12 removed the rollback path. Keep the hard-error message stable enough for operators to diagnose stale scripts. |
| Engine daemon redb query server | Any external users of `aethyme-engine-cli daemon start/status/serve`; `aethyme explore` reads redb in-process and no longer starts the daemon. Removed eval warm-phase files are historical only. | Daemon start/status names, `engine-daemon.log`, and `listening on` are cross-process contracts. The daemon no longer builds or warms `RepositoryMap`; it opens `.aethyme/graph_store.redb` read-only before listening and serves migrated task/symbol/caller RPCs through redb. |

Outstanding risks after 4.7.12:

- The audit only covers in-repo consumers under `packages/aethyme/`; already-deployed playground repos can still contain stale generated skills or missing local excludes. Run `scripts/eval/verify-playground.sh` against active playground targets after any template or playground-hygiene change.
- Python output caches under `AETHYME_CACHE_DIR` are independent of the removed Rust `map_cache`; deletion of `map_cache.rs` does not clear cached JSON command output.
- Any out-of-repo scripts that read orphaned `.aethyme/cache/map-*.bin` or `.aethyme/parse_store.redb` files directly, or pass `--no-fragments`, are unsupported and were not found by this repo-local audit.
- The Rust engine now builds against redb 4.x. Existing `.aethyme/graph_store.redb` files created by the redb 2.x engine should be treated as disposable local materializations and regenerated from `.aethyme/graph/` fragments with `aethyme-engine-cli index --repo <repo>`; the index command reports old redb file formats before deleting/recreating the graph store. Do not rely on an in-place redb file-format migration for deploys.
- Query-only CLI commands now open `.aethyme/graph_store.redb` with redb `ReadOnlyDatabase`. This prevents inspectors from taking a writable database handle, but redb still rejects read-only opens while a writable `Database` is open on platforms with file locks. Future daemon work needs an explicit ownership model if it keeps the writable graph store open.
- The 2026-05-29 post-index compaction experiment does not justify enabling compaction by default. `index --compact` added 614 ms on Mockup and 283 ms on MediaWiki, produced no persistent Mockup reduction, grew MediaWiki from 33,689,600 to 44,265,472 bytes, and moved MediaWiki `query-overview` median latency from 10.0 ms to 11.3 ms.
- The 2026-05-29 disposable-fast experiment is opt-in. It uses redb `Durability::None` for bulk node/edge/risk commits, keeps the final metadata write at immediate durability, and publishes from `.aethyme/graph_store.redb.indexing` only after the staged store is complete. Initial profile runs improved redb write stages on Mockup (`redb_edge_writes` 925 ms to 528 ms, `redb_commit` 28 ms to 5 ms, plus 143 ms metadata and 124 ms publish) and MediaWiki (`redb_edge_writes` 292 ms to 180 ms, `redb_commit` 30 ms to 12 ms, plus 60 ms metadata and 31 ms publish). Keep it guarded until interrupted-process recovery has more field evidence.

## Migration checklist

When deleting or renaming a CLI entry point:

- [ ] Grep this file for the entry-point name; list affected consumers.
- [ ] For each Skill template (`SKILL.md`, etc.): update the wording.
- [ ] For each shell wrapper (`aethyme-explore`, etc.): update the
      `exec` line.
- [ ] For `verify-playground.sh`: update the grep patterns.
- [ ] For `setup-playground.sh`: update if it calls the entry point.
- [ ] Run `aethyme repo deploy-skills --force` against any active
      benchmark playground clone to flush the stale static copy.
- [ ] Run `aethyme deploy --repo <path>` against any normal
      repository enhancement target to refresh generated onboarding.
- [ ] Add a row to the relevant section above documenting the change.
- [ ] If a consumer was missed (i.e. you discovered it during a
      validation eval rather than this audit): **add it to this file
      in the same commit as the fix.** That's how the registry stays
      complete.

## Failure modes (taxonomy)

When a cross-process consumer references a deleted entry point:

- **Class 1: load-time failure** — script syntax-checks fail or imports
  fail. Loud. Caught immediately.
- **Class 2: invocation failure** — script runs but `aethyme <cmd>`
  errors with `No such command`. Loud once invoked. May go undetected
  if the script's invocation path is rare.
- **Class 3: silent stale guidance** — script reads file content and
  matches/passes/fails based on string presence; updated entry point
  passes the wrong checks. The 2026-05-08 wrapper bug AND the
  `verify-playground.sh:135` grep flip-bug were both Class 3. **These
  are the dangerous ones**: nothing visibly fails, but the system's
  health checks are now lying.

Class-3 failures are why this registry exists. Static analysis can't
help; only manual audit of every consumer can.


### Broker status reporting

`broker status --refresh` explicitly requests fresh lease derivation, dirty
worktree inspection, Git conflict/history checks and cleanup eligibility.
Routine `broker status` and its alias `--compact` infer the current session
from the worktree or accept `--session <id>`. Readers should dispatch on
`schema`. Session selection gives `aethyme.broker-session-view/v2`; elsewhere
`aethyme.broker-status-compact/v1` exposes repository counts and warning
categories. In the repository form, count-bearing lists omit records and
advice categories omit per-observation summaries and commands. In the session
form, other-session and unscoped recovery records are counts, not actions.
Use `--full --json` for the detailed JSON, and `--summary --json` for the
legacy brief. Consumers of the full and brief documents use top-level
`deferred_checks`, `leases_refreshed` and `leases_refreshed_at_ms`. Compact
consumers use `data_freshness` to distinguish recorded and unknown findings.
`--refresh` names in `deferred_checks` any check its inspection budget cut
short, and lists the sessions it did not reach (#460).
`integration_relation: not_checked` and empty ref heads in summary mean unknown,
not synchronized. `cleanup_retention.eligibility_checked` is false on routine
status; its inventory can be incomplete, declared by `inventory_complete` and
`inventory_deferred_sessions`. `phase_timings_ms` contains measured phase costs
and `command_total` including broker opening. See the status section in
`../guides/broker-workflows.md`. Neither reporting mode authorizes deletion.

### Session-local broker commit

`aethyme broker commit --session <id> [--non-interactive] [--timeout <seconds>]
[--json] -- <git-commit-args>` runs a local `git commit` only from that
session's registered worktree and recorded, unprotected branch. It refuses
inherited Git routing/configuration overrides, hook-bypass and history-rewrite
options, incomplete path inventories, and paths outside current session
ownership. Normal Git commit hooks run. The command uses a non-queueing
session-local lane; session worktree removal refuses while that lane is held,
while unrelated worktree starts and coordinated Git operations can proceed.

With `--json`, a report includes `command_ran`, `outcome`, `execution`,
`execution_receipt_id`, `head_before`, nullable `head_after`, nullable
`committed_paths`, nullable `committed_ownership`, nullable
`post_commit_audit_error`, `post_commit_audit_complete`, and `ok`. `outcome` is
`passed`, `command_failed`, `ownership_failed`, or
`post_commit_audit_unknown`. `execution` is the existing guarded-execution
report; `execution_receipt_id` identifies its durable receipt. The committed
path census and its ownership audit are separate from the post-command dirty
path audit in `execution`. A report is returned only after the commit command
ran; a nonzero result can mean the command failed, ownership failed, or the
post-commit audit is unknown. Only the broker's own preflight refusals above
are guaranteed to happen before receipt creation. A supervisor/helper setup or
launch failure after preflight can leave a `starting` receipt and no commit
report; inspect `aethyme broker advanced exec-status` and treat an unresolved
receipt as unknown. `command_ran` is true only when the supervisor confirms
command start; it is null when a timeout leaves dispatch unconfirmed. Null is
unknown, not proof that the command did not run. Inspect the receipt and
reported paths before retrying.

### Lease scope and guarded-execution reporting

Lease records returned by `aethyme broker advanced leases list --json` and by
broker status JSON carry a scope field. Its value exact covers only the named
path; `recursive_directory` covers the subtree of a directory claimed with a
trailing slash. `aethyme broker advanced leases claim --json` reports the same
scope, and adds a warning field when an existing directory was claimed as an
exact path. The warning does not change coverage.

`aethyme broker advanced leases plan --session <id> --json` adds a coverage
object for the proposed paths. Only that session's explicit leases count as
authority; implicit leases and other sessions' leases do not. Paths left
uncovered come with exact and narrow recursive claim suggestions.

`aethyme broker advanced exec --session <id> --json` adds `command_ran`,
`edits_remain` and `changed_paths_remaining`. `command_ran` is true on every
report the command produces, because a command that could not start fails
before any report. `edits_remain` says whether any path the command changed is
still dirty, and `changed_paths_remaining` lists those tracked and untracked
paths in full. Human output says whether the command ran and whether edits
remain, and shows at most eight changed paths. An ownership refusal can come
after the command has already changed files, so a consumer must inspect the
reported paths before retrying.

Successful `broker submit prepare --json` reports `shared_cache_wait_ms`, the
monotonic time spent acquiring the shared cache grant (zero when no shared
cache is used). Its command metric adds a `preparation` object with that wait,
`shared_cache_coordinated`, and the already-reported step results. A current
preparation has no executed steps and a zero wait. When a step uses the
shared cache, the report and the metric's `preparation` object also carry
`shared_cache_mode`: `shared` when the preparation ran alongside others under
`[shared_cache] max_concurrent`, `exclusive` when it held the cache alone; the
field is absent otherwise. A sharing preparation holds a `capacity` allocation
keyed `preparation_cache_users` in pool `aethyme-prepare-cache-users:<repository>`
(60 units divided by `max_concurrent`); every holder that needs the cache alone
holds the `preparation_cache` exclusive key and all 60 units. Command metrics
add the spans `shared_cache_acquire` and `shared_cache_rotation_probe`, and the
counters `shared_cache_held_shared`, `shared_cache_held_exclusive`,
`shared_cache_exclusive_for_recovery`, `shared_cache_exclusive_for_budget_check`
and `shared_cache_exclusive_for_overdue_budget_check`. Step names are
repository-declared; commands and arguments are not logged in this object.
Failed preparation may have no metric breakdown.

Since 2026-11-14 the work a preparation does while it waits for or holds the
shared cache is reported, additively. The report and the metric's
`preparation` object carry `shared_cache_operations`, omitted when empty: one
entry per operation in order, with `operation` (one of `waiting_for_cache`,
`rotation_recovery`, `budget_inventory`, `rotation_drain`,
`preparation_step`), `detail` (the step name, omitted otherwise),
`elapsed_ms`, `items_done` (entries unlinked or measured; 0 otherwise), and for
the budget inventory `bytes_observed` and `outcome` (one of `absent`,
`within_budget`, `over_budget`, `inconclusive_time_limit`,
`inconclusive_entry_limit`). The metric's `preparation` object also carries
`shared_cache_rotation` when a rotation ran. Metrics add the spans
`shared_cache_lease_held`, `shared_cache_rotation_recovery`,
`shared_cache_inventory`, `shared_cache_rotation_drain` and
`preparation_step`, and one counter per inventory outcome
(`shared_cache_inventory_absent`, `_within_budget`, `_over_budget`,
`_inconclusive_time_limit`, `_inconclusive_entry_limit`).

Since 2026-11-23 a sharing preparation (`max_concurrent` above 1) that takes
the cache alone holds it alone only for recovery, the budget check and a
rotation. Before its steps it releases the exclusive grant and takes a share
through the same wait queue and unfinished-rotation probe as any sharing
preparation; if the probe cannot rule out a rotation left unfinished in that
gap, it takes the cache alone again and recovers before any step runs.
`shared_cache_mode` still reports the mode the cache was taken in, and
`shared_cache_wait_ms` includes the wait for the share. Metrics add the spans
`shared_cache_exclusive_section` (from the exclusive grant to the share, or to
release when the preparation never shares), `shared_cache_downgrade` and
`shared_cache_downgrade_wait`, and the counters
`shared_cache_downgraded_to_shared` and
`shared_cache_downgrade_found_rotation`. A due (not overdue) budget check
whose store already has a queued, running or backing-off background
measurement, and no fresh result, no longer takes the cache alone for a
foreground inventory; it shares the cache and counts
`shared_cache_budget_check_deferred_to_background`. The overdue check still
waits for the cache alone.

While it waits for or holds the cache, `submit prepare` keeps one live record
per repository and session under `<host state>/preparation-cache-activity/`
beside the host resource database, rewritten every 15 s and removed when the
preparation ends: `schema` (`aethyme.preparation-cache-activity/v1`),
`repository`, `session_id`, `holder_pid`, `operation`, nullable `detail`,
`items_done`, nullable `items_total`, `items_total_is_lower_bound`, nullable
`bytes_observed`, nullable `lease_acquired_at_ms` (null while waiting),
`operation_started_at_ms` and `updated_at_ms`. Readers ignore unknown fields,
judge freshness before any cap, and skip a record not rewritten for 90 s;
records a killed preparation left behind are swept once stale, and a rerun of
the same session overwrites its record. `submit prepare status --json` adds
that record as `current_operation` for a session that is not current (omitted
otherwise), and, while the session waits, `waiting_on` with the holder's
`session_id`, `holder_pid` and `current_operation`. A waiting prepare's
repeated notice names the holder's session, pid, operation and progress.
Rotation recovery and drain print a stderr line when they start and every
15 s (`preparation cache: <operation> for <s>s: <done> of <total> entries
unlinked; lease held <s>s, ...`); a budget inventory that runs longer than
15 s prints the same. The record is observational, never recovery or
admission authority.

The budget inventory's decision changed the same day. It stops as soon as the
bytes seen pass `max_bytes` and rotates (`budget_exceeded`, with
`bytes_before: null`, `inventory_complete: false` and the bytes seen in
`observed_bytes_lower_bound`). An inventory that stops at its 30 s time limit
or its 2,000,000-entry limit (raised from 100,000) with the bytes seen still
within budget is inconclusive: the store is kept, not rotated. An
unmeasurable store is no longer rotated just for being unmeasurable; runaway
growth is left to the storage-pressure guard, and after three consecutive
inconclusive checks `broker status --json` adds the `Notice` advice
`preparation.shared-cache-budget-inconclusive` with the entries, bytes and
time the last check saw. `inventory_time_limit` and `inventory_entry_limit`
remain valid `rotation_reason` values but are no longer produced. A
repository that does not set `max_concurrent` above 1 now measures its store
at most once an hour, as a sharing one already did, instead of on every
preparation; recovery of an unfinished rotation still runs every time.

Since 2026-11-15 an inconclusive budget check queues a background measurement
of the store, additively. Two records live in the repository's broker metadata
(the `meta` table, never under `.aethyme/`); neither carries a path. The cache
root is named by `cache_root_key` (the 12-hex digest also used in the
budget-check stamp key) and identities are 32-hex digests of the volume-aware
directory identity (device, inode, filesystem identifier).

- `preparation.shared_cache_measurement.request`: JSON or `null`. Schema
  `aethyme.preparation-cache-measurement-request/v1` with `schema`,
  `repository`, `cache_root_key`, nullable `root_identity`, `budget_bytes`,
  `reason` (`inconclusive_time_limit`, `inconclusive_entry_limit`, `refresh`
  or `confirm_over_budget`), `queued_at_ms`, `times_queued`, `attempts`,
  `consecutive_failures`, and, when present, `retry_after_ms`,
  `failure_store_identity`, `running_since_ms` and `running_pid` (set while a
  worker walks; a marker whose pid is gone or older than the 30-minute walk
  deadline plus 10 minutes is stale and taken over), and `last_interruption`
  (`reason`: `memory_pressure`, `deadline`, `root_changed`,
  `rotation_pending`, `root_unavailable` or `scan_refused`; optional
  `detail`, never a path; `at_ms`, `attempt`, `entries`, `bytes`,
  `duration_ms`, `pacer_memory_pause_ms`, `pacer_load_pause_ms`). Written by
  `submit prepare` (queue, coalesced by compare-and-set; a coalesce keeps a
  running marker and attempt) and by the `gc __continue-artifacts` worker
  (running marker, back-off, clear). The worker's final write applies only
  while `attempts` and `running_since_ms` still match its own walk. A
  `root_changed` interruption drops the request; repeated `deadline` or
  `scan_refused` walks of one store back off from 10 minutes, doubling, up to
  24 hours.
- `preparation.shared_cache_measurement.result`: JSON or `null`, the last
  completed measurement. Schema `aethyme.preparation-cache-measurement/v1`
  with `schema`, `repository`, `cache_root_key`, nullable `root_identity`,
  nullable `store_identity` (null when the store was absent), `budget_bytes`,
  `outcome` (`within_budget`: complete and at or under the budget;
  `over_budget`: more than 5% over it; `within_margin`: between the two,
  decides nothing), `complete` (false when the walk stopped once over the
  margin; `bytes` is then a lower bound), `bytes`, `entries`,
  `skipped_entries`, `concurrent_activity`, `over_budget_walks`,
  `over_budget_confirmed` (two consecutive over-budget walks of the same
  store), `started_at_ms`, `completed_at_ms`, `duration_ms`,
  `entries_per_second`, `bytes_per_second`, `pacer_memory_pause_ms`,
  `pacer_load_pause_ms`, `pacer_scan_limiter_sleep_ms`, `attempt`,
  `trusted_for_ms`, `trust_reason` (`growth_rate`, `not_growing`,
  `at_most_half_of_budget`, `at_most_80_percent_of_budget`,
  `above_80_percent_of_budget` or `not_within_budget`), `times_used` and,
  once a budget check decided from it, `used_at_ms`. Interrupted walks never
  replace it, but reset `over_budget_walks`. A completed rotation sets both
  records to `null`.

Readers ignore unknown fields and treat a malformed record as absent. A
budget check uses a result only when its `repository`, `cache_root_key` and
`root_identity` match the current root, it is younger than `trusted_for_ms`
(at least an hour), it decides the current budget (within budget, or
confirmed over budget plus the margin), and, checked under the lease after
rotation recovery, its `store_identity` matches the store. The worker takes
the host-wide advisory lock `<host state>/run/preparation-cache-measurement.lock`
(content-free) for the walk; a busy lock defers the request by a minute, and
a missing host state directory skips the walk for 10 minutes. The worker is
admitted on broker open while a request is due (not running, not backing
off), and `submit prepare` starts it after releasing the cache lease when it
queued one. The walk is paced at 10,000 entries a second; the worker's
`last_unattended_pacing` summary, persisted after the walk, gains
`scan_limiter_sleep_ms` (omitted when zero).

The budget inventory `outcome` gains `within_budget_background` and
`over_budget_background` (the decision came from a measurement; no walk ran
under the lease), with counters `shared_cache_inventory_within_budget_background`
and `shared_cache_inventory_over_budget_background`. The prepare report and
the metric's `preparation` object add `shared_cache_background_measurement`
(omitted otherwise) with `decided_by` (`outcome`, `bytes`, `entries`,
`complete`, `completed_at_ms`, `age_ms`) and `queued` (the queue reason).
`submit prepare status --json` adds `shared_cache_background_measurement`
(omitted when the repository shares no cache or nothing was ever queued):
`request`, `result` (the records above), `result_fresh` and `running`. The
`preparation.shared-cache-budget-inconclusive` advice keeps its threshold; its
`evidence` gains one line with what the background measurement last did, and
it clears when a measurement settles the question (within budget, or a
confirmed over budget). Metrics add, in `phase_counts`,
`preparation_cache_measurement_queued`, `_coalesced`, `_started`,
`_restarted`, `_took_over_stale`, `_already_running`,
`_completed_within_budget`, `_completed_over_budget`,
`_completed_over_budget_unconfirmed`, `_completed_within_margin`,
`_concurrent_activity`, `_host_busy`, `_host_lock_unavailable`,
`_ignored_root_changed`, `_ignored_store_changed` and
`_interrupted_<reason>`, and in `phase_timings_ms` the span
`preparation_cache_measurement_lane`, the durations
`preparation_cache_measurement_walk`,
`preparation_cache_measurement_pacer_memory_pause`,
`preparation_cache_measurement_pacer_load_wait` and
`preparation_cache_measurement_scan_limiter_sleep`, and
`artifact_worker_pacer_scan_limiter_sleep`.

Successful `broker push` command metrics add `push_preflight`, `push_git`,
`push_pr_and_watch`, `push_default_branch`, `push_overlap`, and
`push_finalization` to `phase_timings_ms`. These are wall-clock milliseconds
inside the existing `push_session` phase and must not be added to that total.
The JSON push report itself is unchanged. Since 2026-11-12 push metrics may
also carry the span `default_branch_unmoved_check` (comparing the remote
default branch the push read with the local tracking ref) and the counters
`default_branch_fetch_skipped_unmoved` (the fetch was skipped because the
default branch had not moved) and `default_branch_fetch_reused` (a recent
fetch was reused) in `phase_counts`. Since 2026-11-13 each push attempt also
counts, in `phase_counts`, how it chose to send the session branch, before
Git runs: `push_plain_fast_forward` (the remote branch is absent or an
ancestor of HEAD, so a plain push), `push_lease_recorded` (a rewritten branch
leased against the oid the last `broker push` recorded for the session),
`push_lease_journal_proven` (a rewritten branch leased against the oid a
succeeded journaled push of the same session sent to the same branch on the
same remote, such as a `broker advanced git` push) or
`push_refused_foreign_remote_tip` (the remote branch holds a commit HEAD does
not contain and the session did not push; refused with exit code 3 before
anything is sent). The choice is made against the tip the remote is believed
to hold (the newer of the recorded oid and the remote-tracking ref) and
re-checked under the push lock against the tip the push's own remote read
saw; `push_remote_tip_redecided` counts a push decided a second time because
that read showed the remote elsewhere (the first attempt is journaled
`failed` with `reason` `remote_tip_revalidation_failed` and `observed_sha`,
and Git never ran it), and `push_remote_tip_rejected` counts a push Git
rejected because the remote moved after that read (`stale info`,
`fetch first`, `non-fast-forward`), which is then also decided again against
a fresh read instead of relaying Git's output. A second miss refuses with
exit code 3. A successful `broker advanced git` push of the session's own
branch to the remote `broker push` uses now records the pushed oid the same
way `broker push` does. The coordinated `git push` process span
`push_git_process` is split, from Git's own Trace2 events, into
`push_git_process_before_hook` (connecting and reading the remote's refs),
`push_git_process_hook` (the `pre-push` hook run by that push; `0` when the
push ran none, because none is installed or hooks ran outside the lock) and
`push_git_process_after_hook` (sending the pack and updating the ref). Since
2026-11-12 these are recorded on every push whose trace has a split point;
before that the split was never recorded, because Trace2 carries no
process-relative clock on `child_start`. Two counters say why a split is
missing or hookless: `push_git_trace_without_hook` (the trace showed no
`pre-push` hook) and `push_git_process_split_unavailable` (the trace was
requested but had neither a hook nor an end of the remote ref read, e.g. an
empty trace file). The three spans nest inside `push_git_process`; do not add
them to it. Timed-out pushes carry `push_timeout_timing` in the operation
journal when Trace2 was requested. Its `stage` is `remote_ref_listing`,
`pre_push_hook`, `transfer` or `unknown`; `process_ms` includes the time
observed until the child stopped. When known, `before_hook_ms` gives the
first split boundary. `hook_ms` measures a completed hook (zero with no
hook); `hook_elapsed_ms` reports time observed during an interrupted hook.
`after_hook_ms` is present only when transfer started and both split
boundaries are known. Missing fields represent missing evidence. The
`push_git_timeout_trace` span times the diagnostic read; these fields never
change the write outcome or its lock scope.

Changed 2026-11-21. A configured or granted duration (a budget, a reserve,
a retry delay) is no longer recorded as a timing: it is counted in
`phase_counts` with a zero `phase_timings_ms` entry, and its value goes to the
attachment `phase_values_ms` (an object, the largest value seen per name, in
milliseconds). This applies to `landed_proof_session_budget_doubled`,
`proof_git_stopped_at_deadline`, `artifact_worker_retirement_share_budget`,
`artifact_worker_capacity_removal_retry`,
`artifact_worker_capacity_removal_cache_lane_reserved` and
`checkout_trash_drain_budget_extended`, whose summed "timings" were budgets
(the first summed to 1,617,540 s over 21 worker runs on the busiest
repository). A reader that summed them as time spent must read
`phase_values_ms` instead. Introduced the same day: every metric line a
command writes carries `cpu_ms` (`user`, `system`, `children_user`,
`children_system`: CPU milliseconds of the process and its reaped children,
from `getrusage`; absent off Unix); the removal lane counts
`auto_cleanup_memo_miss_absent` and `auto_cleanup_memo_miss_changed` (why a
closed checkout group is judged in full again); and `checkout_strip_backlog`
nests `checkout_strip_backlog_folders` (the folder and tombstone census) and
`checkout_strip_backlog_regenerable` (the per-folder catalogue `lstat`s).

Changed 2026-11-21 (worker census and activity waits). The checkout backlog census remembers the folders it found to be retired checkouts' tombstones in meta key `checkout_strip.backlog_tombstones` (JSON: path to `[device, inode, modification time in ns]`) and judges a folder whose identity is unchanged from one `lstat` (count `checkout_strip_backlog_tombstone_remembered`); a census that could list every pool replaces the memory, one that could not keeps what it did not see. Counts are unchanged. The changed-file activity probe remembers, for the rest of the process (one command or one worker run), a checkout whose `git status` ran out of its 2 s, and answers the next probe of it as that timeout did without running Git (count `worktree_status_activity_unavailable_remembered`); a failed call is not remembered.

Changed 2026-11-21 (transcripts cadence). The detached worker's transcripts lane (every repository's worker runs it; its work is host-wide) records each pass in `<host state>/transcripts/last-worker-pass.json` (`at_ms`, `idle`: nothing eligible, within budget, first-run gate open). After an idle pass the next worker pass on the host waits 15 minutes (count `transcripts_worker_pass_not_due` on the skipping worker's line); a pass that found work or ran out of budget, an unreadable record, or a clock that went back runs it at once. Manual `transcripts` commands are unaffected.

Changed 2026-11-21 (staged prunes reclaimed promptly). The storage worker deletes every `.aethyme-storage-evicting-*` entry of each of its roots at the end of each pass as well as at its start (pass phase `sweep_end`), whichever process staged it. A gate whose managed cache prune staged units writes `<staging>.bytes` beside the staging directory (its logical size, swept with it) and starts the storage worker when none runs (counts `storage_staged_deletion_worker_started`, or `storage_staged_deletion_worker_running` when one already runs and will sweep at the end of its pass). A staged entry in the host cache root older than 5 minutes makes any command's start check start a worker when none runs, even while the ledger is fresh, and adds `broker status` advice `storage.staged-deletion-pending` (notice: the recorded bytes awaiting deletion, how many staged entries, the oldest's age). Deletion stays in the background worker.

Changed 2026-11-21 (named lifecycle holders). A background lifecycle claim held 5 s or more (`lifecycle_lock_held_background_long`) is also recorded in the attachment `lifecycle_lock_background_long_holders`: an object keyed `"<lane> <source file>:<line>"` (the claim's lane and the line in Aethyme's source that asked for it; never a user path or id) with `count`, `total_ms` and `max_ms`; `unknown` when the claim did not record one. The merged-branch slice stops starting deletions after 4 s of its claim (was 5 s), so the one in progress ends under 5 s.

Changed 2026-11-21 (abandoned-close claim). The automatic close of an abandoned or landed session (`abandoned_close`) no longer holds the repository lifecycle claim through its evidence. It runs every check (activity, gates, operations, merge queue, preparation, artifact use, process use, leases, landed proof, pull requests, uncommitted content, the work class and early-close rule) under the per-session operation claim, as before, then takes the lifecycle claim only for the close: it re-checks the start reservation and the spawned process, compares a stamp of the checkout's commit state taken before the evidence (HEAD commit, the session branch's tip, the index file's identity) with the same stamp now, and runs the existing compare-and-set (session row, holder binding, leases). Under the lifecycle claim it first keeps the session when another live session is recorded at the same path (`start_reserved`, count `abandoned_close_path_reused_before_close`). The stamp is read from files only (`HEAD`, the branch's loose ref or its `packed-refs` line through the worktree's `gitdir` and `commondir`, the index's metadata), never by running Git; a part that cannot be read keeps the session (`worktree_activity_unavailable`, count `abandoned_close_checkout_stamp_unreadable`). Anything that moved keeps the session for this pass (`changed`, count `abandoned_close_checkout_moved_before_close`); re-checks that took 4 s or more keep it with the new keep reason `close_claim_budget` (counter `session_abandonment_kept_close_claim_budget`, count `abandoned_close_claim_budget`). New spans `abandoned_close_claimed` and `abandoned_close_checkout_stamp`. On the busiest repository the claim was held 9 to 14 s per close before.

Changed 2026-11-21 (GC service recovery). `broker gc service` recognises an installation record that is provably dead. A record is this repository's `gc-sweep-services/<uuid>/plan.json` (with its receipts) and the unit `~/Library/LaunchAgents/com.aethyme.gc-sweep.<uuid>.plist`. It is dead when launchd answers that the label is not loaded (`launchctl print gui/<uid>/<label>` exiting 113; any other answer proves nothing) and either the unit file is missing or the recorded runtime binary is missing. `service status` reports a record without a unit as `installation_state: stale_record` with `record` (`recorded_plan_digest`, `recorded_runtime_version`, `recorded_runtime_present`, `unit_file`, `scheduler_state`: `loaded`/`not_loaded`/`unknown`, `replaceable`, `replaceable_because`) instead of failing. `service plan` adds `existing_installation` (the same object, or `{"same_plan": true}`) to its JSON and two lines to its text. `service install --confirm <new digest>` archives a dead, different record and installs the new plan; it still refuses a loaded one (deactivate first) or one launchd cannot vouch for. `service uninstall --confirm <recorded digest>` finishes removing a dead record without a unit. Archiving renames the record's `plan.json`, `control.json`, `last-pass.json` and, when present, the unit into `gc-sweep-services/<uuid>/archive/<ms>/` (the unit as `unit.plist`, so it never stays where launchd loads units), keeping the newest five archives. Span `gc_service_record_archive`, count `gc_service_record_archived`.

Changed 2026-11-21 (GC service record hardening). `install` and `uninstall` take the service directory's installer claim (`.install.lock`) before reading the record or asking launchd, and decide on the record read under it, so two installs cannot both judge a record dead. Archiving moves the unit first (copy, sync, then remove when the rename fails with `EXDEV` across volumes), so a crash leaves a plan without a unit, never the reverse. A unit with no plan is a stale record too (`replaceable_because: plan_missing_and_label_not_loaded`); `status` reports it with `plan_file: missing` and `recorded_unit_digest`, and `uninstall` takes that unit digest as confirmation when no plan is recorded. Launchd's answer now covers both per-user domains: `launchctl print` of `gui/<uid>/<label>` and `user/<uid>/<label>`; loaded in either is `loaded`, `not_loaded` needs exit 113 from both, anything else is `unknown`. Span `launchd_label_state` wraps both probes.

Changed 2026-11-21 (GC service survives self-update). The service stays pinned to one exact runtime pair, and self-update now rebinds it before that pair can be deleted. After the selector switch and before `cleanup_old_bundles`, `update execute`/`apply` looks at each `gc-sweep-services/<uuid>/plan.json` whose recorded router lies in a bundle of the installation being updated that neither `current` nor `previous` selects (or that is already gone). Under the service's installer claim it loads and verifies the recorded plan (namespace, label `com.aethyme.gc-sweep.<uuid>`, generation, digest, and the unit file's bytes equal to the plan's rendered unit), asks launchd, and then: `unknown` (or a probe that could not run) changes nothing and keeps the old bundle this time; `not_loaded` archives the plan, receipts and unit and publishes a plan and unit built by `Plan::from_runtime` for the new pair from the recorded repository, uuid, home and uid; `loaded` does the same, then `bootout gui/<uid>/<label>` and `bootstrap gui/<uid> <unit>`, with `aethyme.gc-sweep-service-control/v1` receipts (`action: refresh`) before and after. The old bundle is kept unless launchd said not loaded or the bootout was acknowledged, so a failure, a refused bootout or a tick holding the claim never strands a loaded job. Nothing here can fail the update. Each outcome is an `aethyme.gc-sweep-service-refresh/v1` object (`repository_uuid`, `outcome`: `not_installed`/`not_verifiable`/`already_current`/`label_state_unknown`/`refresh_failed`/`refreshed_not_loaded`/`refreshed_and_reloaded`/`refreshed_reload_unconfirmed`/`refreshed_bootstrap_unconfirmed`, `scheduler_state`, `previous_plan_digest`, `previous_runtime_version`, `plan_digest`, `runtime_version`, `archived_to`, `keep_previous_runtime`, `error` (at most 256 characters), `observed_at_unix_ms`), written to `gc-sweep-services/<uuid>/last-refresh.json` and listed in the update's JSON as `gc_service_refresh` (omitted when empty; text output prints one line per service). `broker gc service refresh [--json]` runs the same steps for this repository against the running runtime. `gc service status` runs it by itself only when the recorded runtime is gone, the uuid read from the repository's marker is the service's, the recorded plan and unit verify, and the plan names this repository; it then adds `automatic_refresh`. Status JSON also adds `last_error`, `last_refresh` and `next_action` (the refresh command when the recorded runtime is gone). A managed tick that fails preflight writes `aethyme.gc-sweep-service-last-error/v1` (`repository_uuid`, `generation` when it is a valid digest, `observed_at_unix_ms`, `error` at most 256 characters) to `last-error.json` beside `last-pass.json`, only into an existing private service directory named by a valid uuid; it is archived with the record. `broker status` adds the warning `storage.gc-service-runtime-missing` with `aethyme broker gc service refresh` when this repository's recorded runtime is gone. Spans `gc_service_refresh_for_update`, `gc_service_refresh` and `status_gc_service_runtime`; counts `gc_service_refreshed`, `gc_service_refresh_reload_unconfirmed`, `gc_service_refresh_label_unknown`, `gc_service_refresh_failed` and `gc_service_tick_refused`. The pr-watch service binds its broker binary and digest the same way and is not refreshed yet.

Changed 2026-11-21 (GC service refresh never leaves the service silently gone). If publishing the refreshed plan or unit fails, the archived record is moved back exactly (anything matching the failed new record is dropped first, then the plan, then the unit, then the receipts; the plan and unit refuse a conflict, while a receipt already in the directory, such as a `last-error.json` a tick wrote without the installer claim, is treated as newer and kept, leaving the archived copy in the archive): outcome `refresh_failed_restored`. If that restore fails too, the outcome is `refresh_failed_unrestored`, `last-refresh.json` names the archive, `gc service status` reports `installation_state: refresh_interrupted` with `archived_record`, `last_refresh` and `next_action`, and `gc service refresh` restores the record from that archive (only a numeric child of the service's own `archive/`) before refreshing it. A record whose last control receipt was an `activate` or `refresh` that was not acknowledged (refused, unknown or interrupted) is retried by `gc service status` under the automatic-refresh proofs plus a present runtime: launchd not loaded gets one `bootstrap gui/<uid> <unit>` (receipt `action: activate`), loaded records `operation_result: observed_loaded`, unknown does nothing; the result is `load_retry` in the status JSON. `broker status` reads files only (no launchctl) and warns with one of `storage.gc-service-runtime-missing`, `storage.gc-service-record-interrupted` (command `aethyme broker gc service refresh`), `storage.gc-service-unit-missing` (a plan without its unit file and a present runtime; commands `aethyme broker gc service status --json` and `aethyme broker gc service plan`) or `storage.gc-service-load-unconfirmed` (commands `aethyme broker gc service status --json` and `aethyme broker gc service activate --confirm <plan digest>`). The refresh walk inside one self-update has an overall budget of 30 seconds, checked before each service; a service not reached gets `label_state_unknown` with an error saying the budget was spent, a `last-refresh.json`, and keeps its bundle. Each kept bundle is named in the outcome's `kept_bundle`, and the update's text output prints one line per kept bundle and why. Spans `gc_service_record_restore` and `gc_service_load_retry`; counts `gc_service_record_restored`, `gc_service_load_retried` and `gc_service_refresh_budget_spent`.

Changed 2026-11-22 (GC service passes say why they reclaimed nothing, and confirm stuck trash). `broker gc sweep` (an operator's, and the scheduled service's tick) first runs the confirmation `gc apply` already applies whatever plan it is given: a checkout trash row still pending because its mover's start time was unreadable (a sandboxed command records `creator_started_ms: 0`, and no process table can then prove the mover gone) is confirmed when it is over a day old, its entry still has the recorded device and inode, and no open session uses its session's path. It deletes nothing: the paced host deleter removes confirmed entries with its own identity and open-file checks, and a confirmation requests a checkout retirement run. The confirmed count is the rows the compare-and-set actually flipped. The same pass drops a stale row whose entry does not exist, but only when its trash is present, private, carries the broker's marker and is on the device the row recorded; the look never creates the trash, so an unmounted volume or a cleared temporary directory keeps its rows. The row is written before the rename, so such a row is a rename that never happened (the checkout is still in the pool) or an entry already gone. `gc apply` runs the same confirmation and drop. `gc sweep --read-only-snapshot` skips this step and writes nothing. The sweep JSON adds `artifact_lane` (`ran`, `gc_lock_held` when another GC, normally the detached worker, owns the repository's GC lock, `disabled`, `paused_by_strict_cleanup`, `spaced`) and `checkout_trash` (`recorded`, `deletable`, `pending`, `stale_pending`, `confirmed_this_pass`, `oldest_deletable_created_ms`, `dropped_missing_this_pass`, and the unconfirmable rows kept by cause: `kept_too_young`, `kept_identity_changed`, `kept_session_in_use`, `kept_trash_unavailable`); the text output prints a line for each when relevant. `aethyme.gc-sweep-service-last-pass/v1` keeps its schema and adds optional fields older readers ignore: `artifact_lane`, `entries_reclaimed`, `bytes_reclaimed` (observed logical file bytes plus preparation-cache bytes), `checkouts_removed`, `checkouts_kept`, `trash` (the `checkout_trash` object; a failed pass keeps the last observation), `trash_entries_drained` (entries gone from the deletable backlog since the previous pass, net of this pass's confirmations; a floor), `reclaimed_nothing_because` (when the pass reclaimed and confirmed nothing and the deleter made no progress, the first of `trash_not_draining` (deletable entries, and neither fewer of them nor a later oldest one than at the previous pass), `artifact_lane_yielded_to_worker`, `artifact_lane_disabled`, `artifact_lane_paused_by_strict_cleanup`, `artifact_lane_budget_spent`, `trash_entries_kept_by_proofs`, `trash_entries_awaiting_mover`), `passes_without_reclaim` (consecutive empty passes, kept across a failed pass and reset by one that reclaims, confirms, or finds the paced deleter draining the trash) and `unreclaimed_since_unix_ms`. After eight consecutive empty passes (two hours at the fixed 900 s interval), `broker status` warns with `storage.gc-service-reclaiming-nothing` when the recorded cause is one a command can act on: `trash_not_draining` (`aethyme broker gc trash drain --json`) or `artifact_lane_budget_spent` (`aethyme broker gc sweep --json`); rows the proofs keep are never advised, each with `aethyme broker gc service status --json`; it reads the service's files only, after the repair advices, inside span `status_gc_service_runtime`. Span `gc_sweep_confirm_stale_trash`; count `artifact_sweep_gc_lock_held`; attachment `gc_sweep_pass` (`artifact_lane`, `artifact_complete`, `directories_reclaimed`, `trash_confirmed`, `trash_deletable`, `trash_pending`, `trash_stale_pending`, `trash_dropped_missing`); count `checkout_trash_stale_missing_rows_dropped`.

Changed 2026-11-22 (trash movers record a provable start, and entries whose identity changed stop counting as backlog). A trash mover now reads its own start for itself alone (`KERN_PROC_PID` on macOS; `/proc/self/stat` field 22 plus `btime` on Linux), falling back to a process-table snapshot; it no longer parses the table's start text, which a sandboxed command cannot list and which parses only in C-locale words. Counts `trash_creator_start_direct`, `trash_creator_start_from_table`, `trash_creator_start_unknown` (one per move). The deleter proves a creator gone only when the table lists no process with its pid or one with another start; a listed start it cannot read no longer counts as gone, and a listed start within 2 s of the recorded one, or exactly one hour away (a `ps` text read back in the repeated daylight-saving hour), is the same process (the abandoned-operation archive sweep uses the same proof). A confirmed trash row may carry `identity_mismatch` (`device_changed`: the recorded inode on another device number, as after a remount; `inode_changed`; `not_a_directory`) in `checkout_retirement.trash_rows`: set while the entry exists with another identity, cleared when it matches again; such an entry is never deleted, is left out of the deletable backlog, of the drain's `entries_pending` and of the catch-up decision. The deleter's pre-pass and `gc sweep`'s reconcile set and clear it; the reconcile also drops a confirmed row whose entry is missing from a present, private, marked trash on the recorded device, and concludes nothing for a row whose directory is not named as the trash. The sweep JSON's `checkout_trash` adds `confirmed_dropped_missing_this_pass`, `identity_changed` with `identity_changed_device`, `identity_changed_inode`, `identity_changed_not_a_directory`, and the other confirmed entries held out of `deletable`: `failing` (failure cap) and `in_use`. The last-pass receipt adds `trash_entries_held_back` (entries that left the deletable backlog without being deleted since the previous pass), and `trash_entries_drained` and `trash_not_draining` no longer count those as drained; `reclaimed_nothing_because` may be `trash_entries_identity_changed`, which is never advised. `gc trash drain --json` adds `entries_deletable_remaining`, `identity_changed`, `identity_changed_by_cause` and `identity_changed_entries` (at most 20 `{entry, cause}`). `broker status` adds the Notice `cleanup.trash-identity-changed` (entries and causes, at most 20; command `aethyme broker gc sweep --json`); it asks people to leave `device_changed` entries alone and to look at `inode_changed` and `not_a_directory` ones by hand. Attachment `gc_sweep_pass` adds `trash_confirmed_dropped_missing`, `trash_identity_changed`, `trash_failing`, `trash_in_use`; counts `checkout_trash_identity_changed` (per held-back entry per deleter run), `checkout_trash_confirmed_missing_rows_dropped`, `checkout_trash_identity_marks_changed`.

Changed 2026-11-22 (trash rows are rebound after a remount). A remount gives every directory on the volume a new device number and keeps inode numbers, so trash rows in `checkout_retirement.trash_rows` recorded before it no longer match. `gc sweep`'s reconcile and the deleter's pre-pass now rebind such a row, confirmed or pending, to the device its trash is on now, each row on its own proof: its directory is named as the trash and the trash, opened under its pool without following links, is a real directory owned by this user, closed to group and others, holding the broker's marker (a regular file, opened without blocking) with the broker's content; the trash's current device differs from the row's (another inode on the same device is another directory, never rebound); the entry is a real directory on that device with the recorded inode; and the entry name is a broker entry name whose session field is the row's session. No other row's state is needed (the name is generated once per move and never reused), so rows written before several remounts are each rebound and a row refused for good never blocks the others. The trash's ownership, mode and marker are judged only once some row records another device than the trash's; a directory not named as the trash, an absent pool or trash, or a trash reached through a link concludes nothing and counts nothing. The rebind sets `device` and clears `identity_mismatch`, nothing else, inside the compare-and-set and only onto a stored row unchanged since the proof; a pending row stays pending and still needs its own confirmation, and the run that rebinds a row neither confirms nor deletes it. Deletion keeps its pinned-identity checks unchanged. The pre-pass no longer drops a pending row whose creator is gone and whose entry has the recorded inode on another device: one that fails its rebind proof is kept with `identity_mismatch: device_changed` (never deleted, left out of `entries_pending`), so its entry no longer turns unrecognized; the sweep JSON's `checkout_trash` counts such a row under `identity_changed` (with `identity_changed_device`), not `pending`, as `broker status` and the drain report do. `cleanup.trash-identity-changed` lists held pending rows too and gives the remedy for a `device_changed` entry still listed after a sweep (`trash_unverified`: restore the trash's mode, owner or marker; `name_not_for_session`: handle like `inode_changed`). The sweep JSON's `checkout_trash` and the GC service receipt add `rebound_this_pass`, `rebind_refused_this_pass` (rows) and `rebind_proven_old_devices_mixed_this_pass` (trash directories whose rebound rows spanned more than one old device: an observation); `gc trash drain --json` adds `rebound` and `rebind_refused` (rows by cause); attachment `gc_sweep_pass` adds `trash_rebound`, `trash_rebind_refused`, `trash_rebind_proven_old_devices_mixed`; attachment `checkout_trash_drain` adds `rebound` and `rebind_refused` (rows by cause). Span `checkout_trash_rebind` (each trash with a row on another device, in the sweep and the drain); counts `checkout_trash_rebound`, `checkout_trash_rebind_refused_<cause>` (one per refused row per look) with cause `trash_unverified`, `name_not_for_session`, `entry_missing`, `entry_unreadable`, `not_a_directory`, `entry_on_other_device` or `inode_changed`, `checkout_trash_rebind_proven_old_devices_mixed`, and `checkout_trash_pending_identity_held`. A catch-up slice that only rebound rows counts as progress.

Changed 2026-11-22 (GC service rebinds itself after an update). A managed tick verifies its recorded router as the installer receipt's router, which resolves through the installation's `current` selector, so a service stays usable only while it is bound to the bundle `current` selects; the bundle an update keeps as `previous` still exists but every tick refuses it ("scheduled GC requires an installer-managed immutable runtime pair"). The update refresh (`update apply`/`execute` and the installer's `update bootstrap`, which share one activation path) now rebinds every service of the installation whose recorded router lies in any bundle other than the one it just selected, or is gone, instead of only bundles it removes; the steps and proofs are unchanged. A service's runtime binding is `current`, `stale` (present but not what `managed_root/current/aethyme` resolves to), `gone` or `unknown` (the selector cannot be read; proves nothing). `gc service status` runs its automatic refresh for `stale` as well as `gone`, and only when the command itself runs from the installation's current selection (the runtime the refresh binds); the status JSON adds `runtime_binding`, and `next_action` is `aethyme broker gc service refresh` whenever the runtime does not verify, the binding is `stale` or `gone`, or the last tick error is newer than the last finished pass. `broker status` reads files and selector links only: for a `stale` or `gone` binding, when it runs from the current selection, it writes `gc-sweep-services/<uuid>/refresh-scheduled.json` (`aethyme.gc-sweep-service-refresh-scheduled/v1`: `repository_uuid`, `observed_at_unix_ms`) and then starts one detached `<current router> broker gc service status --json` in the main checkout (own process group, no stdio, the service's tick variables removed), at most once per 900 s counting any refresh attempt recorded in `last-refresh.json`; the child repeats every proof under the installer claim and launchd decides as before (an unknown answer changes nothing). `broker status` warns with `storage.gc-service-runtime-stale` (a present runtime that is no longer current) or `storage.gc-service-ticks-refusing` (`last-error.json`'s `observed_at_unix_ms` newer than `last-pass.json`'s `finished_at_unix_ms`, or no pass), each with `aethyme broker gc service refresh` then `aethyme broker gc service status --json`, and adds an evidence line when it started or held back a background refresh. Span `gc_service_refresh_schedule`; count `gc_service_refresh_scheduled`. A managed tick now writes `gc-sweep-services/<uuid>/tick-started.json` (`aethyme.gc-sweep-service-tick-started/v1`: `repository_uuid`, `generation` when it is a valid digest, `pid`, `started_at_unix_ms`) before its first access to the repository or a pool volume; it is archived and restored with the other receipts. The tick releases the service's installer claim (`.install.lock`) around the writable broker open, which validates the worktree root on the pool volume and can block there, and takes it back afterwards, re-loading the plan (same bytes) and re-verifying the runtime; a mismatch refuses the tick into `last-error.json`. A tick that started at least one service interval (900 s) ago with no `last-error.json` or finished `last-pass.json` since is unanswered: `gc service status` adds `unanswered_tick` (`state` `tick_blocked` while its pid exists, else `tick_ended_without_receipt`; `started_at_unix_ms`, `minutes_since_start`, `pid`, `process_alive`, `cause_hint`, `person_action`) and `tick_started`; `broker status` warns with `storage.gc-service-tick-blocked` (the pid still exists, checked with signal 0) naming the likely macOS cause, a privacy consent for the new runtime binary on a removable or protected volume, and the person's action. launchd does not start a second instance of a running job when its `StartInterval` fires, so a blocked tick stops the service until it ends. Review follow-up: the tick retakes the claim waiting up to 2 s; when its broker open fails it records the failure as this generation's last pass only after retaking the claim and re-proving the plan, otherwise generation-free into `last-error.json`. A `last-pass.json` of another generation now reads as no pass for this one (count `gc_service_foreign_generation_receipt`) and the next pass overwrites it, instead of failing `status` and every later receipt write. `tick-started.json` adds `process_started_at_unix_ms` (this process's own start); an unanswered tick counts as blocked only while signal 0 finds its pid and a process-table snapshot does not prove that pid now belongs to a process with another start, and a service whose last control is an acknowledged `deactivate` is never advised as blocked. `broker status` reads the service record once and asks for its own executable only when the binding is stale or gone; it decides under the service's installer claim taken without waiting (a contended claim starts nothing), and `refresh-scheduled.json` adds `current_bundle` and `attempts`: the wait after an attempt is 900 s doubling per attempt that left the binding stale for the same current bundle, capped at 6 h, and a new current bundle resets it (a later success alone does not, since the binding is then current and nothing is scheduled). It prints nothing: counts `gc_service_refresh_receipt_unwritable` (no spawn, and none for the rest of that process), `gc_service_refresh_start_failed` and `gc_service_refresh_reap_failed`. It starts nothing from an App Sandbox process (`APP_SANDBOX_CONTAINER_ID` set); other sandboxes are bounded by the backoff. Second review: the tick's process is `alive`, `gone` or `unverified`. Signal 0 answering `ESRCH` is gone, and `EPERM` proves nothing; the native process listing alone (never `ps`, never a timeout; a failure is latched for the process) then decides. When neither answers, `unanswered_tick.state` is `tick_unverified` (`process: unverified`) and `broker status` warns with `storage.gc-service-tick-unverified`, which says the tick has no receipt and its process could not be checked from here; it is never called blocked or ended. The scheduler releases the claim right after writing its receipt, before starting the child. A lock file that cannot be opened at all (not contention) is `gc_service_refresh_lock_unavailable`, latched per process and shown nowhere. A scheduling or refresh receipt dated in the future counts as expired.

Changed 2026-11-22 (every `broker status` form notices a stale GC service binding). In 0.8.160 the service check ran only inside the full view (`--full`, `--refresh`), so the default compact `broker status` and `broker status --json`, the session view and `--summary` never looked: after an update the binding stayed stale, no `refresh-scheduled.json` was written and no `gc_service_*` count appeared in the metrics line. The check now runs in the full, compact, summary and session forms; `--read-only-snapshot` still writes nothing and skips it. The compact form lists the advice by category (`storage.gc-service-runtime-stale` and the other `storage.gc-service-*` ids); the session view adds `gc_service_advice` (an array of status advice rows, present only when there is one) and its text form prints `GC service: <summary>`. It stays a few file and selector-link reads, prints nothing on stderr, starts no child to decide and at most the one detached refresh. Each look counts exactly one outcome: `gc_service_status_identity_unavailable` (no home, git common directory or repository UUID marker), `gc_service_status_record_unreadable`, `gc_service_status_not_installed`, `gc_service_status_record_incomplete` (a plan without its unit, a unit without its plan, or a plan naming no router or installation root), `gc_service_status_binding_current`, `gc_service_status_binding_unknown` (the `current` selector cannot be read), and, for a `stale` or `gone` binding, `gc_service_refresh_latched` (an earlier lock or receipt failure in this process), `gc_service_refresh_sandboxed`, `gc_service_refresh_running_unknown` (no executable path), `gc_service_refresh_not_current_router` (this process's executable and `managed_root/current/aethyme` resolve to different files; both sides are canonicalized, so a PATH link, the unresolved `current` link, a relative link or a directory alias all compare by the file they name), `gc_service_refresh_scheduled`, `gc_service_refresh_backing_off`, `gc_service_refresh_contended`, `gc_service_refresh_lock_unavailable`, `gc_service_refresh_receipt_unwritable` or `gc_service_refresh_start_failed`. Span `status_gc_service_runtime` wraps the look and `gc_service_refresh_schedule` the decision. Which path repairs what: an update run by a binary that already has the update refresh rebinds the services itself (`gc_service_refresh` in the update's JSON). An update run by an older binary runs the older code, which cannot know the new rule, so it leaves the service bound to the previous bundle; the first `broker status` in that repository run from the installation's current router (the managed `aethyme` on PATH) repairs it in the background (`gc_service_refresh_scheduled`, then `last-refresh.json`), and `aethyme broker gc service status --json` or `aethyme broker gc service refresh` repairs it in the foreground. A `broker status` run from another binary (for example a separately built `aethyme` earlier on PATH) counts `gc_service_refresh_not_current_router` and only advises. Review follow-up: one look reads the record once, classifies its binding once and loads the plan at most once, and reads the repository identity from the Git common directory the broker open already bound plus the UUID marker file, so it runs no `git` process. The session view's `gc_service_advice` is a revisioned section (added to the hashed sections, so tokens issued before it reset once): an advice that appears, changes or clears produces a new revision and appears in the delta. To keep that revision stable, the blocked- and unverified-tick evidence names the tick's start time and pid without a minutes-ago count, and the line saying what the look did (started, backing off, contended) is added only to the full and summary forms, never to the session view; the compact form shows advice ids and counts only.

Changed 2026-11-23 (the default `broker status` form runs the GC service check). Measured on 0.8.161: the default compact `broker status [--json]` still recorded no `gc_service` count. Routine status (`status` with only `--json`, `--compact`, `--summary`, `--session <id>` or `--since-revision`) opens a read-only snapshot by design, so it does not touch broker storage or mark advisories delivered, and the 0.8.161 check ran only on the writable compact route, which no routine status takes. The check now runs in the CLI after every form's view is built: compact, summary, session view, full and the full read-only snapshot. Decision: routine status may still start the one background rebind. The read-only rule for routine status protects broker storage and advisory delivery; the rebind writes only the service's own `refresh-scheduled.json`, is bounded by the installer claim and backoff, and lets its child repeat every proof. Without it nothing an agent runs would repair the binding. A read-only snapshot that is not merely routine (a compatibility read-only open, when the broker database is newer than the binary) stays strictly read-only: it classifies the binding, counts the outcome and shows the advice, and schedules nothing (outcome `gc_service_refresh_read_only` for a `stale` or `gone` binding). On every platform each check adds span `status_gc_service_runtime` and one mode count, `status_gc_service_check_may_repair` or `status_gc_service_check_read_only`; off macOS nothing else is read or counted. Command metric lines are recorded only in normal mode, and a compatibility read-only open is the only status that may not repair, so `status_gc_service_check_read_only` and `gc_service_refresh_read_only` are never written to `command-metrics.jsonl` today: they are counted in-process and exercised by tests, and every metrics line that exists names `status_gc_service_check_may_repair`. Review follow-up (what a routine poll can cause): the status process itself writes nothing in broker storage and marks nothing delivered. On macOS, for a `stale` or `gone` binding and only from the installation's current router, it may start one detached `<router> broker gc service status --json` with the main checkout as its working directory and its output discarded. That child goes through the generic dispatch, where `gc service` is now service administration: none of its actions (`status`, `refresh`, `plan`, `install`, `activate`, `deactivate`, `uninstall`) surfaces session advisories (so it neither opens the broker store writable for them nor records an `AdvisoryDeliverySurface::Command` delivery) and none renews or downgrades a session's task lease. The child writes the service's own receipts (`refresh-scheduled.json`, `last-refresh.json`) and may ask launchd to rebind; it appends its own `broker.command.succeeded` or `broker.command.failed` event through an open that never creates or migrates broker state, a line in `command-metrics.jsonl`, and whatever the per-command storage check does for every command; and it runs one `git` probe, as every `gc` command does.

Every command metric line may add span totals to `phase_timings_ms`, and a
`phase_counts` object with how many times each span ran. Spans are recorded
where the work happens rather than by each command: `subprocess_git` and
`subprocess_other` (child processes), `broker_open`, `lease_refresh`,
`implicit_lease_scan`, `overlap_classification`, `landing_search`,
`remote_durability`, `size_walk`, `status_build`, `blockers`, `agents_view`,
`cleanup_plan`, `cleanup_eligibility`, `auto_cleanup_plan`,
`auto_cleanup_apply`, `finish_session`, `sync_session`, `prepare_session`,
`preparation_status`, `gates_run`, `git_fetch`, `default_branch_fetch` and
`worktree_remove`. A phase a command names itself takes precedence over a
span of the same name. Spans nest and are summed across worker threads, so
one can exceed `duration_ms` and they must not be added together. The set
grows; consumers must ignore names they do not know. Spans never cause a
read-only command to write a metric line.

Submit command metrics add `phase_timings_ms` for broker opening, contract
preflight, ownership auditing, planning, merge simulation, verification slot
wait, verification checkout setup, graph and gate selection, gate execution,
and verdict recording as reached. `submission_total` includes the phases inside
submission and must not be added to them. Durations use a monotonic clock and
repeated phases accumulate. Early failures may have only a partial breakdown;
phase keys are allowlisted and never contain session input.

Writable broker openings split the enclosing `broker_open_recovery` span into
`open_contract_backfill`, `open_operation_outcome_replay`, `open_prepared_operation_reap`,
`open_repeatable_fetch_recovery`, `open_integration_reconciliation`, and `open_gc_maintenance`.
Within GC maintenance, `open_gc_rotation_debt` probes cache rotation witnesses,
`open_gc_retention_policy` reads the retention rules, `open_gc_confirmed_journal`
checks or resumes a confirmed journal, `open_gc_inline_sweep` measures the
foreground sweep, and `open_gc_worker_admission` measures admission for
the detached worker. The zero-duration `open_gc_inline_cadence_skip` event
marks an open that uses recent sweep progress to skip headroom sampling.
Confirmed-journal-only opens omit the sweep, worker, and rotation probes.
Subphases are nested in their enclosing durations. Since 2026-11-19 worker
admission nests `worker_admission_cleanup_outbox` (queueing a due post-close
checkout cleanup), `worker_admission_compact_backfill`,
`worker_admission_due_checks` (the recorded reads that decide whether any
lane is due; absent, like the backfill, when the checkout is ephemeral or
the worker is suppressed) and
`worker_admission_launch` (runtime checks, the worker lock and the spawn;
only when a lane is due).

Guarded-exec reports include `phase_timings_ms`: `precheck` measures session
lookup, the initial worktree snapshot, receipt and admission;
`command_and_supervision` measures dispatch, the child and its supervisor;
`postcheck` measures outcome recording and the ownership audit. The CLI
command metrics include these timings plus `broker_open`, without child
arguments. Timings are wall-clock milliseconds and do not sum to the full
duration: startup, rendering and final bookkeeping are outside these phases.
Failures before a report is returned have no phase breakdown.

Introduced 2026-11-19. Guarded exec also records `precheck`,
`command_and_supervision` and `postcheck` as spans, so `broker commit` and
preparation, which wrap guarded exec without naming phases, carry them on
their metric lines; `broker exec` keeps the values it names itself. Nested
spans split the two checks: `exec_precheck_session` (session row and checkout
discovery), `exec_precheck_census` (the dirty and untracked path snapshot and
path identities), `exec_precheck_receipt` (receipt and helper observation
rows), `exec_precheck_artifact_use` (artifact-use admission);
`exec_postcheck_receipt` (outcome rows), `exec_postcheck_census` (the second
snapshot and identity comparison), `exec_postcheck_audit` (the ownership
audit and its receipt row) and `exec_postcheck_overlap_changes` (overlap
content attached to warned leases). `broker commit` adds `commit_preflight`
(session verification, path census and ownership audit before dispatch) and
`commit_post_audit` (history shape, committed-path census and ownership audit
after the command); the Git command and its hooks are inside
`command_and_supervision`.

Introduced 2026-11-19. The session view of `broker status` (`--session`, or
compact status from a session checkout) adds spans `status_session_lookup`
(finding the session for the current checkout), `status_session_coordination`,
`status_session_block`, `status_session_scratch_dir`,
`status_session_lease_requests`, `status_session_peers`,
`status_main_checkout` (sync state and its advice), `status_session_stack` and
`status_session_render` (revisioned rendering and output).

Introduced 2026-11-19. `finish` measures the checkout it is about to remove
before it takes the repository lifecycle claim, recorded as
`finish_size_walk_unlocked` (the walk inside it is still `size_walk`). The
figure it reports as `cleanup.reclaimed_bytes` is that measurement, as before.
An already-closed session that `finish` only judges for cleanup eligibility is
no longer walked at all.

Introduced 2026-11-19 (second round). The session peer view nests
`peer_view_own_leases`, `peer_view_lease_overlaps` (the indexed overlap
lookups; their rows and order are unchanged), `peer_view_scopes` and
`peer_view_pre_edit`. Session coordination nests
`coordination_action_notice`, `coordination_collaboration_fence`,
`coordination_stale_count` and `coordination_waiters`. `blockers` nests one
span per source: `blockers_operations`, `blockers_host_operations`,
`blockers_resource_leases` (with `blockers_resource_lease_owners`, mapping
lease worktree fingerprints to sessions, and the zero-duration count
`blockers_resource_lease_owner_canonicalized`, one per recorded checkout
that still exists and is resolved), `blockers_gate_cache`,
`blockers_pidfiles` and `blockers_action_required`. `finish` splits its time
under the lifecycle claim into `finish_claim_cleanup_plan` (the eligibility
plan), `finish_claim_recovery_proof` (the recovery proof, ownership re-proof
and the durable cleanup intent) and `finish_claim_removal` (the removal
itself). Every Git worktree removal adds `worktree_remove_environments`
(destroying the checkout's environments) and `git_worktree_remove` (the
`git worktree remove` child, whose time is also in `subprocess_git`).

CLI exec also commits an opaque receipt before command dispatch and adds its id
to the final text/JSON report. `broker advanced exec-status --session <id>`
reads one receipt by `--receipt` or a bounded latest list (`--limit`, default
20, maximum 100). The record contains only phase, command outcome, exit code,
ownership audit, process-group cleanup, timestamps and whether process status
was revalidated; it stores no command, argv, environment, output, or PID. This
is an observational status surface: it never signals or cleans a process and
does not make an unresolved receipt authoritative for session cleanup. A client
disconnect can leave `starting` or `running` as the last persisted phase;
legacy terminal mode records direct-child exit while descendant cleanup stays
unverified. Keep the newest 100 terminal receipts per session. A known
direct-child completion can be pruned even when descendant cleanup is
unverified; pruning does not prove descendants stopped. Starting/running rows
and unknown outcomes without verified cleanup remain until the owning session
is deleted, which cascades to its receipts.

`aethyme broker advanced exec --non-interactive` opts that invocation into
the supervised process-group runner even when the caller has a controlling
terminal. It is a caller assertion that the command does not need terminal
interaction, not TTY isolation: stdin/stdout/stderr routing stays the same as
the default invocation (JSON mode still routes child stdout to stderr), and a
child may still open the controlling terminal; terminal reads or job-control
operations may stop or fail. The flag belongs before `--`; after the separator
it is a child argument. If the supervised helper cannot start, the user command
is not dispatched and there is no unsupervised fallback. This option does not
by itself change the JSON report schema.

`aethyme broker advanced exec` accepts `--timeout <seconds>` from 1 through 86400.
Supervised execution defaults to 1800 seconds. An explicit timeout is refused
when automatic mode selects the legacy terminal runner, which remains
unbounded. A verified timeout returns a nonzero CLI status after the ownership
audit; JSON keeps `command_success: false`, `exit_code: null`, and the always
present `command_timed_out: true`, with `outcome` set to `timed_out` or
`timed_out_and_ownership_failed`. If process-group cleanup is unverified, the
CLI reports an unknown outcome and skips the ownership audit rather than
claiming that the command stopped.

### Push size accounting

`broker push` JSON `pr_size.files` and `changed_lines` remain review-policy
counts. `pr_size.excluded` lists omitted paths and reasons; optional
`pr_size.diff_totals` contains unfiltered file and changed-line totals from
the same complete local Git diff. Binary files contribute a file and zero
lines. Human output labels the policy counts and reports full totals when
exclusions apply. Advisory thresholds continue to use the policy counts.

### Managed README skill-learning guidance (#814)

Canonical deployment and canonical scaffolding add or refresh a README section
owned by `AETHYME:BEGIN skill-learning` / `AETHYME:END skill-learning` comments.
The scaffold report includes `scaffold.readme-skills` (`created`, `pass`, or
`fail` using the existing check-status envelope); deploy actions include the
selected README path and the existing `created` / `updated` / `unchanged` action.
Consumers must allow this additional check/action. Text outside the section is
preserved. Local-only and supporting-artifact-only deployment do not write the
shared README. See the repository deployment guide for supported formats and
refusals. This adds guidance only, with no agent dispatch or session-log export.

### Complete committed entry checks (2026-10-27)

The serialized internal `ContentVerdict` diagnostic adds `tree_entry_mismatch`
with `paths`, the compared path scope, when object IDs agree but committed file
modes or types differ. The list is a comparison scope, not a claim that every
listed path differs. Consumers must treat this and any unknown verdict as
unavailable for cleanup authorization, never as `present`. Exact-content
landing evidence requires matching complete committed entries; executable bits
and symlink types are part of the proof. Existing ancestry and patch proofs
retain their separate requirements.


## New-session startup phase timings (2026-10-29)

New `broker start --json` and detached `broker start --cmd ... --json` reports
add `phase_timings_ms`, a map of nonnegative integer wall-clock milliseconds.
Existing report fields and public Rust report types remain unchanged. Timings
are CLI metadata carried internally alongside the library reports. Allocation records `upstream_fetch`,
`base_selection`, `lifecycle_lock_wait`, `checkout_count`, `worktree_root`,
`storage_admission` and `git_worktree_add`. Ordinary starts add
`session_registration` (including ownership witness and repository-contract
capture) and `preparation_status`; detached starts add `agent_setup`.
CLI reports also record `broker_open`, `session_allocation`,
`response_preparation` and `total_before_output`, captured before the success
response is serialized or printed. Earlier warnings may already have been
emitted. Human output additionally records `human_render_and_advisories` for
rendering, declared-scope capture and advisories after that boundary, then
prints the map.
The same map is included in the existing command-metric details. Failed
allocation records only the observed coarse open/allocation phases; missing
internal phases are unknown, not zero. Sub-millisecond observed phases round
down to zero.

`session_allocation` and `total_before_output` overlap the narrower phases;
consumers must not sum every map value. `total_before_output` excludes router
and compatibility dispatch, serialization/output, and metric persistence.
The outer command elapsed metric includes output and, for commands that still
perform it, primary-checkout maintenance; external wall-clock measurements can
also include router/process launch and teardown. Do not label whole-command
latency as Git worktree-creation time.

New-session creation no longer scans or fast-forwards the primary checkout.
When a remote default is configured, an implicit base still requires a
successful bounded upstream fetch. Repositories without a remote default
retain their existing local-base behavior. Startup still reads committed
policy and checks root identity, storage and path leases.
Adopt/reuse behavior and explicit session sync remain unchanged. Regression
coverage lives in `aethyme-broker/tests/start_base_cli.rs`.


### Issue-set manifests (draft workflow)

`broker advanced issues apply <manifest.toml> --repo owner/name --session ID
--reason AUTHORIZATION` reads a manifest and creates or updates its GitHub issue
set. The target default branch must opt in with `[issue_workflows]` and
`issue_sets = "allow"` in `.aethyme/config.toml`; missing or invalid policy
fails closed. The current implementation requires the target to match the
session checkout's origin. If a native relationship endpoint returns 404 after
both issues are verified readable, the managed bodies receive a task list or
`Depends on` line; other API failures stop the operation.

A manifest has a stable `set_id`, one `[parent]`, and zero or more `[[issues]]`.
Each entry supplies a unique `key`, a `title`, a relative `body_file`, and
optional `labels` and `depends_on` arrays. Body references such as `{{epic}}`
resolve to issue numbers. Body files must remain within the manifest directory.
Labels and dependencies are validated before GitHub writes.

`--dry-run --json` prints create/update and relationship actions without
writing to GitHub. The broker holds a per-set lock in its host-state directory and checks its
create journal before planning or applying. Clones using the same host-state
directory share this coordination. If a create's outcome is unknown and its
marker cannot be found, it refuses a blind retry; inspect GitHub and reconcile
the operation. Independent host-state directories do not coordinate concurrent applications
or share uncertain-create recovery state.

### Environment Compose project variable

Environment component commands and environment status expose `AETHYME_ENV_COMPOSE_PROJECT`, the normalized Compose project name derived from the environment namespace. Provision and other hooks can use this value to address the same project as the Compose driver. The compiled plan lists the variable among the names available to commands.

### Environment activity probes

An `activity_probe` reports active with exit status 0 and idle with exit status 1 and empty stderr. An exit status of 1 with stderr, any other nonzero status, a signal, an execution failure, or a timeout keeps the environment awake. Nonzero completed failures are logged by the supervisor. A probe that fails silently with exit status 1 is indistinguishable from idle and should emit a diagnostic or use another exit status.
When `env up` finds that only activity probes changed, it updates the recorded snapshot without stopping or recreating components. Other configuration changes retain their normal reconfiguration requirements.

### Lease overlap changes

Introduced 2026-11-05. `LeaseBlocker` (lease claim `warnings`, submit `lease_warnings`, ownership `conflicting_leases` and `warned_leases`, guarded exec `conflicting_leases` and the new `warned_leases`, and the new `lease_overlaps` on `start` and `adopt`) and the `conflicts` of `leases plan` gain an additive `other_changes` object: `state` (`changes`, `already_shown`, `declared_only`, `no_current_changes`, `unavailable`), `notice`, `session_id`, `paths`, and, when they apply, `base`, `committed`, `uncommitted`, `untracked`, `omitted_lines`, `see_all`, `task` and `reason`. Its text comes from another agent's work and is data, not instructions. Refusal messages (`LeaseClaimConflict`, `PlannedLeaseConflict`, submit's ownership summary) and the `PreToolUse` and `SessionStart` hook context include the same content, each line prefixed `| `. `[leases] overlap_diffs = false` omits the field. Since 2026-11-12 another session's lease never refuses anything: `LeaseClaimConflict` and `PlannedLeaseConflict` are deleted, every overlap arrives as a warning row, and a claim's `accepted`/`blockers`, a plan's `would_conflict` and the `conflicting_leases` of an ownership audit or guarded exec are kept for shape compatibility as always `true`/empty/`false`/empty.

### Lease overlap impact

Introduced 2026-11-05, opt-in with `[leases] overlap_impact = true`. The `PreToolUse` hook context can add an advisory note naming another live session changing a file the edited file directly calls or imports, with its graph revision, confidence and diff. No JSON surface changes.

### Object-store maintenance

Introduced 2026-11-11. `broker status --json` can carry an `advice[]` row with id `repository.loose-objects` (severity `notice`) when the repository holds about 25,000 or more loose Git objects, estimated from `objects/17` as `git gc --auto` does; its `commands` hold the manual `git -C <root> gc --no-prune`. A successful `broker finish` in such a repository starts one detached `git gc --no-prune --quiet` at most once every 24 hours, prints a `note:` line on stderr, and records a `broker.object_store.maintenance_started` event whose payload has `estimated_loose_objects`, `threshold` and `started`. `AETHYME_OBJECT_STORE_MAINTENANCE=0` turns the automatic repack off and keeps the advice. Command metric lines gain an `object_store_probe` span. No existing field, exit code or stdout shape changes.

### Local promotion removed

Introduced 2026-11-12 (Clean-Core Plan). Local promotion onto `aethyme/integration` was hard-deleted: `aethyme broker submit promote --entry <id>` (and the `broker promote` internal spelling) now fails as an unknown command; `submit` never promotes, so a verified entry stays `verified`, `submit --json` always reports `promoted: false`, and `promotion_suppressed` on a verified entry reads `verified; the work lands through the session's pull request, so nothing was moved locally`. `submit --verify-only` is still accepted and changes nothing. `[promote] mode = "auto"` or `"manual"`, and `promote_on_submit = true`, still parse; they are ignored, and `broker status --json` carries one `advice[]` row with id `promote.mode-removed` (severity `warning`, `evidence` naming the setting) saying local promotion was removed, the setting is ignored, and to delete it. `mode = "verify-only"` stays valid and silent; `promote.mode-invalid` still reports an unknown value. `status` no longer produces `integration.may-move`, `integration.upstream-main-ahead`, `integration.fast-forward-available`, `integration.stale-promotions`, `main.external-writes` or `integration.behind-upstream`; `summary.may_move_integration` is always `false`; `doctor`'s `integration_movement` is always absent; `start_base.bypassed_integration.reason` is always `verify_only` with no `recovery_command`, and a session is never cut from `aethyme/integration`. Broker open no longer completes an interrupted promotion or backfills exposures for pre-v17 promotions; the `merge.promoted` event and `promoted` queue status are no longer produced, but old rows still load. The `CaptureRequiredForPromotion` and `NotVerified` errors are gone. A repository with no fetched default branch still verifies against its local integration ref, which follows the main checkout's HEAD.

Consumers of `advice[].id` must tolerate `promote.mode-removed`. A wrapper that waited for `promoted: true`, polled for a `promoted` queue status, or ran `submit promote` must land through `aethyme broker push --session <id> --pr` instead. Covered by `aethyme-broker/tests/promote_mode.rs` and `aethyme-broker` `merge::verdict_tests`.

### Formerly promoting repository without a remote

Introduced 2026-11-12. In a repository with no fetched default branch whose `aethyme/integration` holds commits the local default branch lacks (what local promotion left behind), `submit` now verifies against the local default branch that sessions are cut from, not the integration ref: `submit --json` reports `verified_against.source` `local_default` (a new value beside `upstream` and `integration`), `verified_against.reference` the branch name (e.g. `main`), and a `fallback_reason` naming the leftover commit count and the resolving command. `submission_plan.integration_head` and the queue entry's `base_commit` are that branch's tip. While integration holds nothing the local default branch lacks, the base stays `integration` as before. `broker status --json` and `doctor` carry the `integration.leftover-work` row (severity `warning`) in that state, which they previously omitted when nothing was published; its `commands` hold `aethyme broker advanced integration reconcile --upstream <branch> --dry-run`. The row's summary no longer says a pull-request merge will clean integration up; nothing moves integration any more. The text `submit` prints after a verified entry names `push --session <id> --pr` only where that command can work (a fetched default branch and `push_session_branches` authorized); elsewhere it names `aethyme broker advanced representation scan --session <id>`. The library function `Broker::auto_cleanup_landed_integration` was removed (nothing called it since local promotion was removed); `AutomaticIntegrationCleanupReport` stays in the `PostMergeCleanupReport.cleanup` shape, which is always absent.

Consumers of `verified_against.source` must tolerate `local_default`. Covered by `aethyme-broker/tests/merge_e2e.rs` (`a_formerly_promoting_repository_without_a_remote_verifies_against_local_main`).

### Break-glass publication setting removed

Introduced 2026-11-13. `[publication] allow_break_glass` authorized `aethyme broker advanced ship execute --break-glass`, which was deleted with the ship commands; nothing reads the value any more. A configuration that still sets it (`true` or `false`) keeps loading, the value is ignored, and `broker status --json` carries one `advice[]` row with id `config.allow-break-glass-removed` (severity `warning`, `evidence` `.aethyme/config.toml [publication] allow_break_glass = <value>`) saying the setting is ignored and to delete it. The parsed publication policy no longer serializes the key. Consumers of `advice[].id` must tolerate `config.allow-break-glass-removed`. Covered by `aethyme-broker/tests/promote_mode.rs` (`a_retired_break_glass_setting_loads_and_warns_once`), `ship::tests` and `merge::verdict_tests`.

### Automatic maintenance lane shares and keep codes

Introduced 2026-11-13. One automatic maintenance pass (`gc sweep`, a broker open's sweep, the background checkout-cleanup worker, `broker start` at the checkout limit) used to run session retirement first with the whole pass deadline, so a heavy landed-session lap left closed-checkout removal no time: its report read `removed: []`, `kept: []` while closed checkouts accumulated. Retirement now gets half of the pass, a quarter once registered linked checkouts reach three quarters of `[worktrees] max_checkouts`; removal gets the rest, including whatever retirement leaves unused. Under capacity pressure a bounded removal lane continues to the next page of closed checkouts while time remains, up to the end of its lap; otherwise each pass keeps its one-page tranche.

The `auto_cleanup` report (`gc sweep --json`, `gc plan --json`) gains an optional `lanes` object: `capacity_pressure` (bool), `retirement_budget_ms`, `retirement_elapsed_ms`, `removal_budget_ms`, `removal_elapsed_ms` (selection time), `removal_examined`, `removal_memo_skips` (integers; budgets are 0 for an unbounded pass). It is absent from reports the retirement-only worker writes. Every `kept[]` entry gains `code`, a stable machine-readable form of `reason` (`ignored_content`, `uncommitted_changes`, `in_grace`, `session_open`, `not_contained`, `ownership_unproven`, `lease_held`, `pinned`, ..., `other` for an unrecognised reason). A pass whose removal lane judged no checkout no longer erases the previous pass's `kept[]`; it carries it forward. Phase telemetry adds the spans `auto_cleanup_retirement_lane` and `auto_cleanup_removal_lane` and the counters `auto_cleanup_removal_examined`, `auto_cleanup_removal_memo_skips`, `auto_cleanup_removed`, `auto_cleanup_kept` and `auto_cleanup_capacity_pressure`. Consumers must tolerate the new fields; `reason` keeps its wording. Covered by `auto_cleanup.rs` tests `removal_keeps_its_share_when_retirement_exhausts_its_own`, `lane_shares_give_removal_the_larger_part_under_capacity_pressure` and `keep_reasons_have_stable_codes`.

### Cache-tagged and witnessed test-tool directories are disposable

Introduced 2026-11-13. Whole-checkout cleanup (automatic removal, `finish` cleanup and their recovery archives, which share `classify_ignored_for_recovery`) now treats as disposable an ignored directory that holds a regular `CACHEDIR.TAG` whose first bytes are `Signature: 8a477f597d28d172789f06886806bc55`, provided a bounded walk (20,000 entries, symlinks not followed, an unreadable entry keeps it) finds no `.env*`, `*.key`, `*.pem`, `*.p12`, `*.pfx`, `*.sqlite`, `*.sqlite3`, `*.db`, `*.kdbx`, `id_*` without `.pub`, `.netrc`, `.npmrc`, `.pypirc`, `credentials.json` or `.aethyme` beneath it. `BUILT_IN_REGENERABLE` gains `.hypothesis` (only Hypothesis's entries), `playwright-report` (only `index.html`, `data/`, `trace/`) and `test-results` (Playwright's `.last-run.json` with a string `status`, at most 1 MiB), each also subject to the same whole-subtree walk. A checkout that was kept only for such a directory may now be removed by the next pass. Covered by `auto_cleanup.rs` tests `a_cache_directory_tag_declares_an_ignored_directory_regenerable`, `witnessed_test_tool_outputs_are_regenerable_and_lookalikes_are_not` and `a_closed_landed_checkout_holding_only_a_tagged_cache_is_removed`.

### Ignored blockers, keep memo and capacity advice

Introduced 2026-11-13. A `kept[]` entry with `code` `ignored_content` gains `ignored_blockers` (up to 16 ignored roots relative to the checkout; a directory ends in `/`). The `auto_cleanup` report gains `blocked_by_ignored`, absent when empty: up to ten `{name, checkouts, config_line?}` rows aggregated over every closed checkout the removal lane remembers keeping for ignored leftovers (meta key `auto_cleanup.keep_memo`, at most 4,096 entries, re-proved at least weekly), most common first; `config_line` is `[cleanup] regenerable = ["<dir>"]` for a directory and absent for a file. A remembered checkout is skipped (counted in `lanes.removal_memo_skips` and the `auto_cleanup_removal_memo_skips` counter) until its HEAD, the cleanup policy, its session set or the modification time of the checkout root, a blocker or a blocker's parent changes; skipping only ever keeps. `broker status` `cleanup.checkout-capacity` adds one evidence line per top blocker (at most five) and its `commands` change from `["aethyme broker gc sweep --json", "aethyme broker finish cleanup --all-cleaned --json"]` to `["aethyme broker gc sweep --json", "aethyme broker gc plan --json"]`; the removed command is a read-only inventory that freed nothing in this state. Covered by `auto_cleanup.rs` `an_unknown_ignored_directory_is_named_remembered_and_released_by_configuration` and `tests/broker.rs` `status_warns_before_the_linked_checkout_limit_refuses_start`.

Standalone-file rows (changed 2026-11-13; the JSON change is additive, the policy change is not). `[cleanup] regenerable` globs now also release a standalone ignored regular file whose whole relative path they match with `*`, `?` and `[...]` confined to one path component (a glob containing `**` never names a file); a protected name in any component, compared without regard to case, still keeps the checkout with a reason containing `has a protected name`. Directory matching itself is unchanged, but an existing glob now also releases the files it matches under the file rule: `<dir>/*` the ignored files directly in `<dir>`, `*-cache` a root-level file of that name, `*.ext` every root-level ignored `*.ext` file. The guard is the load-time probe (a glob matching `.env`, `.env.local`, `secrets.key`, `cert.pem`, `data.sqlite`, `app.db`, `notes.txt`, `README.md` or `.aethyme` refuses the policy). The protected-name set, shared with the tagged-cache and test-tool walks, now matches its documentation and grows: any `.env` prefix, anything containing `.sqlite`, `*.db` with `-wal`/`-shm`/`-journal` siblings, `*.tfstate`, `*.tfstate.*`, `*.jks`, `*.keystore`, `.pgpass` and `.git-credentials` join `*.key`, `*.pem`, `*.p12`, `*.pfx`, `*.kdbx`, `id_*` without `.pub`, `.netrc`, `.npmrc`, `.pypirc`, `credentials.json` and `.aethyme`; a tagged cache or witnessed test-tool directory holding one of the added names now keeps its checkout, while directories recognised by name or configured are not walked. A `blocked_by_ignored` row for a file (a `name` without a trailing `/`) now carries `config_line` `[cleanup] regenerable = ["<path, glob metacharacters escaped>"]` when such a line would release exactly that file, and otherwise a new optional string `config_refusal` saying why no line can (a protected name, or a path whose literal pattern would match a secret or ordinary root-file probe such as `notes.txt`, or a path containing a backslash or a control character, which no pattern can name); at most one of the two is present, and directory rows never carry `config_refusal`. The row appears identically in `auto_cleanup.blocked_by_ignored`, `auto_cleanup.capacity_removal.blocked_by_ignored`, `capacity_removal.blocked_by_ignored` in `status --json`, and the `cleanup.checkout-capacity` evidence text. Readers that took an absent `config_line` to mean "a file" must check for a trailing `/` instead. New phase counters: `cleanup_checkout_released_by_file_pattern` (an ignored-content verdict passed with at least one file released by a file pattern; planning and the pre-removal re-check each count) and `cleanup_regenerable_protected_refused` (one per protected entry a configured glob named). Covered by `auto_cleanup.rs` `a_standalone_ignored_file_is_named_with_its_line_and_released_by_a_file_pattern`, `a_standalone_file_blocker_names_the_line_that_releases_exactly_it` and `file_pattern_releases_and_protected_refusals_are_counted`.

### `broker start` at the linked-checkout limit

Introduced 2026-11-13. When registered linked checkouts reach `[worktrees] max_checkouts`, `broker start` now runs the closed-checkout removal lane alone (no session retirement) with a 20-second budget, after waiting up to 5 seconds for a GC lock another maintenance pass holds; before, it ran a 5-second combined pass that retirement could consume and skipped it entirely when the lock was held. The refusal reason keeps its prefix `linked checkout limit reached (<count>/<max>); no checkout was allocated` and now continues `; the removal pass removed <n> closed checkout(s), kept <k> (<code> <count>, ...), deferred <d>` and, when ignored content blocks checkouts, `; ignored <name> keeps <n> closed checkout(s): if it holds only regenerable output, adding `[cleanup] regenerable = ["<dir>"]` to .aethyme/config.toml releases them` (or that the lock stayed busy), before the existing reuse and configuration guidance. The phase `checkout_capacity_cleanup` in `phase_timings_ms` covers the wait and the pass; the counter `checkout_capacity_cleanup_lock_busy` records a lock that stayed held. Covered by `tests/worktree_root_cli.rs` `checkout_limit_refusal_names_what_keeps_closed_checkouts_and_its_release` and the existing `checkout_limit_reclaims_an_eligible_closed_checkout_before_refusing`.

### Landed-session lane order and remembered declines

Introduced 2026-11-13. Each retirement pass first offers the landed lane up to 64 open sessions idle past `session_landed_after_hours` that already show landing evidence: a pull request the cached provider data records as merged, or a branch tip with commits of its own that is an ancestor of the fetched default branch (one `git for-each-ref --merged` listing for all sessions). This queue may use half of the retirement lane's share and rotates (meta key `auto_cleanup.likely_landed_after`) so a session whose proof keeps exhausting the budget cannot hold its front. The oldest-first cursor then continues as before, skipping sessions the queue judged in the same pass. Declines `no_session_commits` and `no_landing_evidence` are remembered per session (meta key `auto_cleanup.landed_decline_memo`, at most 4,096) under the session head, the default-branch tip and whether a merged pull request or a representation exists; while all of them are unchanged the landed lane is skipped for that session (counted under the same decline code; counter `landed_decline_memo_skips`) and the abandonment window still applies. Every proof and re-check of `try_close_landed_session` is unchanged. Counters `landed_likely_examined`, `landed_likely_closed` and the span `landed_likely_queue` are added. Paging the removal lane under capacity pressure stops once 32 checkouts are selected in one pass, the most one page could select before. Covered by `auto_cleanup.rs` `a_likely_landed_session_behind_never_landing_older_ones_closes_first`.

### Worker checkout removal, bounded cache passes and interrupted removals

Introduced 2026-11-13. Four mechanisms kept closed landed checkouts from being removed on a busy repository near `[worktrees] max_checkouts`, and each changes:

- The detached artifact worker (`broker gc __continue-artifacts`) held the GC lock for its whole cache lane (minutes once unlinks were paced), so every other GC-lock user was refused. Each cache pass now ends within 30 seconds, and the worker sleeps 500 ms with the lock released before the next, longer than the 250 ms poll of `broker start` waiting at the limit. A post-close wake that still finds the lock held is harmless: the close transaction already queued the outbox row and the worker re-reads it.
- Under checkout capacity pressure the worker now removes closed checkouts itself: a slice (the same proofs as `broker start` at the limit) before its retirement and cache work and between cache passes, at background priority under the host single-deleter slot. A slice removes one checkout per pass and after each removal waits out what Git deleted on the unattended pacer (`[cleanup] unattended_unlinks_per_second`, using the checkout's measured inode count or 10,000 when unmeasured) with no GC lock or lifecycle claim held. As first introduced a slice lasted 20 seconds and removed at most four checkouts; since 0.8.96 it lasts up to 120 seconds and removes at most twelve (see "Capacity removal slice: several paced removals per slice and yield to foreground commands" below, which also holds the current retry intervals). Capacity pressure alone wakes the worker; a slice that removes nothing backs off (meta key `gc.artifact_sweep.capacity_removal_retry_after_ms`, namespaced like the other worker keys). The worker status (`gc.artifact_sweep.worker_status`) reads `capacity_removal removed=<n> kept=<k> deferred=<d> examined=<e> lock_busy=<bool> paced_ms=<t> elapsed_ms=<t>` after a slice, or `capacity_removal_failed: <error>`.
- A pass too short to list checkouts (a broker open's inline sweep, under one second) always reported `capacity_pressure: false`. `broker start` and every worker removal slice now record the count (meta key `auto_cleanup.checkout_usage`, `{"count", "maximum", "at_ms"}`), and short passes and worker admission read it while it is under 15 minutes old.
- Selection could run to the removal lane's deadline, leaving the apply step's filesystem path verification an expired deadline: every selected checkout was deferred and none removed. Selection now stops at three quarters of the removal budget. When path verification does run out of time, the `<session checkout inventory>` kept entry says `registered checkout paths could not be verified before the deadline; selected checkouts wait for the next pass` instead of claiming that paths changed.

Automatic removal now writes a removal-attempt record (meta key `auto_cleanup.removal_attempt:<canonical checkout path>`, JSON `{owner_session_id, head, at_ms}`) and commits it before `git worktree remove` starts; a successful removal clears it (empty value). A closed checkout whose only changes are worktree deletions of tracked files (porcelain ` D`, index unchanged, at least a quarter of the tracked paths, no submodule) passes proof 3 only when that record exists for the same path, owner session and HEAD: a removal that stopped part-way is then finished. Every other proof, including HEAD containment, still applies and is re-checked under the lifecycle claim. Without the record the checkout is kept as before, now under kept code `unmarked_tracked_deletions` (reason `<n> tracked path(s) deleted with no recorded removal attempt, first: <path>`); this includes every checkout half-deleted before this release, which has no record. With `[cleanup] strict = true` a recorded one is kept too, code `strict_tracked_deletions`. Any modification, staged change, rename or untracked path other than `.DS_Store` keeps it as `uncommitted_changes`. `finish` and the landed-session lane are unchanged.

Report fields: `auto_cleanup.lanes` gains `removal_selected` (checkouts handed to revalidation and removal) and `removal_apply_reserve_ms` (budget withheld from selection; 0 when unbounded). An `auto_cleanup.removed[]` entry (and the `broker.cleanup.auto_removed` event payload) gains `resumed_interrupted_removal`, the tracked paths already missing, absent when 0. Phase telemetry adds the counters `auto_cleanup_removal_selected`, `auto_cleanup_interrupted_removals_resumed`, `auto_cleanup_capacity_recorded`, `auto_cleanup_capacity_unknown`, `auto_cleanup_alias_verification_deadline`, `artifact_worker_cache_passes`, `artifact_worker_capacity_removed`, `artifact_worker_capacity_removal_lock_busy`, `artifact_worker_capacity_removal_failed`, the timed phase `artifact_worker_capacity_removal_paced` (total pacer wait of a slice) and the span `artifact_worker_capacity_removal`. Consumers must tolerate the new fields. Covered by `auto_cleanup.rs` tests `a_selection_that_uses_its_whole_share_still_removes`, `an_interrupted_removal_of_a_closed_landed_checkout_is_resumed`, `strict_cleanup_keeps_a_recorded_interrupted_removal`, `a_removal_record_counts_only_for_its_owner_and_head`, `only_a_pure_tracked_deletion_reads_as_an_interrupted_removal`, `a_tracked_submodule_is_never_an_interrupted_removal`, `a_worker_capacity_slice_caps_its_removals`, `a_worker_capacity_slice_backs_off_when_the_gc_lock_is_held`, `a_short_pass_sees_capacity_pressure_from_the_recorded_count` and `the_worker_capacity_slice_removes_and_then_backs_off`, `gc.rs` `a_worker_cache_pass_holds_the_gc_lock_for_at_most_one_slice` and `gc/unattended_pacing.rs` `a_bulk_removal_is_charged_to_the_installed_pacer_only`.

### Capacity removal slice record, index lap completion and lap-aware back-off

Introduced 2026-11-13. On the busiest repository the worker's capacity removal slice judged no closed checkout for hours: whenever another lane had rewound the shared checkout path index, one refresh call advanced at most 128 pages of 16 sessions, so with thousands of registered sessions selection found no complete inventory, examined nothing and backed off for 10 minutes; once it did judge checkouts, a slice that ran out of time mid-lap also backed off for 10 minutes; and its only record, the shared worker status, was overwritten by the worker's later lanes. Changes:

- A bounded selection pass whose path index is mid-lap keeps resuming that lap (never rewinding it) while its deadline allows and each call makes progress. An unbounded pass keeps the single bounded call.
- After a slice that removed nothing, the retry (meta key `gc.artifact_sweep.capacity_removal_retry_after_ms`, unchanged) was 1 minute when the slice moved the path index or the planner cursor but the lap did not end (index incomplete, inventory unavailable, or closed-checkout groups left after the cursor), and 10 minutes when the lap ended or nothing moved; since 0.8.96 the first case is 20 seconds (see the 0.8.96 slice section below). After a removal it is cleared and after a held GC lock it is 1 minute, as before.
- Each slice writes meta key `auto_cleanup.capacity_removal` (JSON): `evaluated_at_ms`, `outcome` (`removed`, `lock_busy`, `inventory_unavailable`, `lap_unfinished`, `nothing_removable`), `removed`, `examined`, `selected`, `kept`, `deferred`, `kept_by_code` (object, keep code to count), `blocked_by_ignored` (as in the report, absent when empty), `index_complete`, `lap_remaining`, `cursor_after_session_id`, `cursor_high_water_session_id`, `passes`, `budget_ms`, `scan_ms`, `selection_ms`, `apply_ms`, `paced_ms`, `elapsed_ms`, `retry_after_ms` (absent after a removal). `gc plan --json` shows it as `auto_cleanup.capacity_removal`, and `status --json` as top-level `capacity_removal`; both are absent until a slice has run. It is never stored inside `auto_cleanup.last_report`.
- `auto_cleanup.lanes` gains `removal_scan_ms` (selection's inventory time: index completion, group page and open-session path resolution) and `removal_inventory_unavailable` (bool: selection judged nothing because no complete inventory was available before its deadline); a pass that found no inventory now reports `lanes` with these set instead of omitting them.
- The worker status after a slice appends ` outcome=<outcome>` to its existing `capacity_removal removed=… elapsed_ms=…` text.
- Phase telemetry adds the span `auto_cleanup_cohort_index_refresh`, the counters `auto_cleanup_cohort_index_refresh_calls`, `auto_cleanup_removal_inventory_unavailable` and `artifact_worker_capacity_removal_outcome_<outcome>`, and the timed phases `auto_cleanup_removal_scan`, `artifact_worker_capacity_removal_scan`, `artifact_worker_capacity_removal_selection` and `artifact_worker_capacity_removal_apply`.

No safety proof changes: uncommitted or untracked content, unpushed commits and ignored content not known to be regenerable still keep a checkout, and apply still requires a complete, revision-current index and a fresh filesystem alias audit. Consumers must tolerate the new fields. Covered by `auto_cleanup.rs` tests `a_capacity_slice_finishes_a_long_cohort_index_lap_before_selecting`, `a_capacity_slice_that_removes_nothing_says_why_in_its_own_record` and `a_worker_capacity_slice_backs_off_when_the_gc_lock_is_held`, and `gc.rs` `an_unfinished_capacity_lap_retries_soon_and_a_finished_one_backs_off`.

### Device-renumber ownership re-proof and ownership keep codes

Introduced 2026-11-13. A schema-2 spawn witness (saved before volume UUIDs) records mount-scoped device numbers, which macOS renumbers on restart or remount, so automatic removal kept those closed checkouts forever as "closed checkout cohort or ownership witness changed" (code `other`). Automatic whole-checkout removal now re-proves such a witness when its paths, all four inodes and the spawn root generation are unchanged and the four objects keep their device-sharing pattern, and in addition the session row is `spawned`, closed and names the path, the path is under the broker worktree root with this repository's common directory, the live `.git` pointer names this repository, this repository registers it (its admin directory is `<common>/worktrees/<name>` and that registration's `gitdir` names the checkout's existing `.git`, the record `git worktree list` reads; a prunable registration does not count), and the registration's `HEAD` is the session branch, which resolves to a commit. The registration evidence is read from those files, fresh on every check, never from a listing of every worktree; the broker-root check's Git call is bounded by the pass deadline, and evidence that cannot be gathered in time defers the checkout instead of keeping it. Artifact-use marker review keeps its own comparison. Since 0.8.96 GC reclamation, checkout compaction, `finish` and finish-cleanup recovery use the same re-proof ("One ownership re-proof for automatic removal, GC and finish" below), so this section's rule is the device-renumber half of that one.

- An `auto_cleanup.removed[]` entry (and the `broker.cleanup.auto_removed` event payload) gains `ownership_reproof`, `"device_renumber"` when the removal used the re-proof, absent otherwise.
- An `auto_cleanup.kept[]` entry gains `ownership_reproof`, `"device_renumber"` when the re-proof was tried and refused; its `code` is `ownership_reproof_refused` and its `reason` names the failed condition.
- New `kept[].code` values (also counted in `auto_cleanup.capacity_removal.kept_by_code`): `ownership_reproof_refused`, `ownership_witness_missing` (a spawned row that never persisted a witness), `ownership_witness_legacy` (a schema-1 witness without a spawn generation), `ownership_adopted_only` (only adopted sessions use the checkout), `checkout_missing` (the directory no longer exists; judged by one `stat`, no Git process) and `cohort_unavailable` (the reason carries the cohort code, for example `index_changed`). The three ownership codes that cannot be proven name the manual command `aethyme broker finish cleanup <session-id>` in `reason`. Each `reason` keeps its old leading phrase, so substring matches on it keep working; consumers that counted `other` or `ownership_unproven` see these codes instead.
- Phase telemetry adds the counters `auto_cleanup_ownership_reproved`, `auto_cleanup_ownership_reproof_refused`, `auto_cleanup_ownership_reproof_unavailable` (each evaluation) and `auto_cleanup_checkout_missing`, and the timed phase `auto_cleanup_ownership_reproof_registration` (registration-file reads).

Consumers must tolerate the new fields and codes. Covered by `events.rs` `device_renumber_accepts_a_schema_two_witness_whose_objects_are_unchanged`, `device_renumber_refuses_each_changed_object_path_and_generation`, `device_renumber_refuses_a_changed_device_sharing_pattern` and `device_renumber_applies_only_to_uuid_less_schema_two_witnesses`, and `auto_cleanup.rs` `renumber_reproof_requires_*`, `renumber_reproof_accepts_only_unchanged_objects`, `ownership_keep_reasons_have_their_own_codes`, `checkout_removal_reproves_a_schema_two_witness_after_a_device_renumber` and `checkout_removal_refuses_a_renumbered_witness_whose_root_was_replaced`.

### Removal judges likely-removable checkouts before its lap

Introduced 2026-11-13. The removal lane walked closed checkouts only in session-id order, so on the busiest repository a checkout whose pull request had merged waited behind a lap over hundreds of long-kept or long-gone groups. Under checkout capacity pressure (bounded passes: the worker's removal slice and `broker start` at the limit), each pass now first judges up to eight indexed closed-checkout groups with a session whose pull request is recorded merged (`pull_request_milestones.merged_at`, through `pull_request_session_links` or the milestone's own `session_id`), most recently merged first, then groups with a due, uncompleted `closed_checkout_cleanup_requests` row (`not_before_ms` passed), most recently closed first. They go through every removal proof unchanged; only the order changes. They do not move the lap cursor, and the lap skips them within the same pass.

- Meta key `auto_cleanup.priority_memo` (JSON `{"entries": {"<canonical checkout path>": {"judged_at_ms", "merged", "retry_at_ms", "outcome", "apply_code"?}}}`, at most 4,096 entries, pruned once `retry_at_ms` passes) remembers every judged priority group: `outcome` is `selected`, `kept` or `deferred`; a selected or kept group is not judged ahead of the lap again for 30 minutes, a deferred one for 5 minutes, or sooner when its merged state changes; `apply_code` is the keep code apply gave a selected group it then kept (for example `artifact_use`). A worker slice whose pass selected a checkout that apply kept goes on to its next pass instead of ending, and counts the selection as progress.
- `auto_cleanup.lanes` gains `removal_priority_examined` (integer, groups judged ahead of the lap, included in `removal_examined`).
- Phase telemetry adds the span `auto_cleanup_priority_selection` and the counter `auto_cleanup_priority_examined`.

Consumers must tolerate the new field and key. Covered by `auto_cleanup.rs` `priority_groups_order_merged_pull_requests_then_due_requests` and `under_capacity_pressure_a_merged_checkout_is_judged_before_the_lap`.

### Capacity removal slice: out of time before selection

Introduced 2026-11-13. A capacity removal slice whose budget ran out before its first removal pass (on the busiest repository, counting about 900 registered checkouts took the whole 20-second budget) recorded `lap_unfinished` with `examined: 0, passes: 0` and backed off ten minutes as if nothing were removable. Such a slice now records outcome `out_of_time_before_selection` and retries after one minute. The `auto_cleanup.capacity_removal` record gains `count_ms` (time counting registered checkouts), `pre_selection_ms` (slice start to its first removal pass, or to giving up) and `budget_consumed_by` (`checkout_count` or `pre_selection`, only with that outcome). Consecutive slices with that outcome wait 1, 2, 4, 8 and then 10 minutes (meta key `gc.artifact_sweep.capacity_removal_out_of_time_streak`, reset by any slice that reaches selection), and the worker now counts registered checkouts from `<common>/worktrees/` with one directory read, falling back to `git worktree list` when that cannot be read. Phase telemetry adds the timed phase `artifact_worker_capacity_removal_count` and the counter `artifact_worker_capacity_removal_outcome_out_of_time_before_selection`. Consumers must tolerate the new outcome and fields. Covered by `gc.rs` `a_slice_that_ran_out_of_time_before_selection_retries_soon` and `an_unfinished_capacity_lap_retries_soon_and_a_finished_one_backs_off`.

### Removal inventory progress and cheap checkout counts

Introduced 2026-11-13. On the busiest repository, capacity removal slices recorded `inventory_unavailable` with `examined: 0`, `scan_ms: 5` and `selection_ms: 28374` against a 20-second budget, then waited ten minutes. The plan's capacity-pressure probe ran a full `git worktree list` (about 20 s over 884 checkouts on an external volume) before the inventory scan, which then found its deadline gone. Changes:

- The linked-checkout count used by `start` admission, `status` (`cleanup.checkout-capacity`), the worker and the pressure probe is one read of `<common>/worktrees/` (the registrations `git worktree list` enumerates), falling back to the listing only when that directory cannot be read. A long pass's probe first reuses a count recorded at most 60 seconds earlier against the same configured limit (meta key `auto_cleanup.checkout_usage`, unchanged).
- Removing a checkout reads its lock reason from its own registration's `locked` file instead of listing every worktree; the listing remains the fallback.
- `auto_cleanup.lanes` gains `removal_probe_ms` (integer) and `removal_inventory_wait` (absent unless `removal_inventory_unavailable`): `capacity_probe` (the deadline passed before the scan began), `cohort_index` (the checkout path index was mid-lap; its progress is kept and resumed by the next pass), `group_page` or `live_paths`.
- The `auto_cleanup.capacity_removal` record gains `probe_ms`, `waiting_on` (the same values) and `index_after_session_id` / `index_high_water_session_id` (how far the path index lap got, done of total by session id). A slice whose inventory stayed unavailable without the index or cursor moving now backs off like `out_of_time_before_selection` (1, 2, 4, 8, then 10 minutes, same streak key); one that progressed retried in one minute and, since 0.8.96, retries in 20 seconds.
- `gc __continue-artifacts` (the detached worker) now writes one `command-metrics.jsonl` line per run with its phase timings (label `gc`, phases `artifact_worker_*`, `auto_cleanup_*`), and no broker outcome event. Phase telemetry adds the timed phase `auto_cleanup_capacity_probe`.

The `[cleanup] regenerable` setting only feeds the ignored-content keep memo, which is re-checked lazily per judged checkout through its policy fingerprint; nothing is reclassified up front when it changes. Consumers must tolerate the new fields and values. Covered by `auto_cleanup.rs` `a_long_pass_pressure_probe_reuses_a_fresh_recorded_count_for_the_same_limit`, `the_checkout_count_reads_this_repositorys_registrations`, `an_unavailable_inventory_says_what_it_waited_on_and_resumes_its_progress` and `the_lock_reason_comes_from_the_checkouts_own_registration`, `gc.rs` `an_unavailable_inventory_backs_off_only_without_progress`, and `cli/telemetry.rs` `the_artifact_worker_records_phase_metrics_without_an_outcome_event`.

### Capacity removal slice: several paced removals per slice and yield to foreground commands

Introduced 2026-11-14. On the busiest repository the worker removed about 13 checkouts an hour under capacity pressure while hundreds waited: each removal's pacer wait (about 22 s for a 22,000-entry checkout at 1,000 unlinks/s) used up the 20-second slice, so every slice removed exactly one, and slices ran only at the start of a worker run and between cache passes. Changes:

- A slice may use up to 120 seconds (pacer waits included) and remove up to twelve checkouts, within a per-run capacity allowance: when the cache lane is due, capacity slices together leave it at least 90 seconds of the run (three cache passes); otherwise they may use the run. Each removal is still its own pass, with every proof and the interrupted-removal record unchanged. A slice ends when a pass selects nothing, after four passes that removed nothing (a selected checkout kept or deferred at the last step; such a checkout is remembered and not selected again), or when a pacer wait would run past the slice's end. A checkout judged and kept costs none of the removal allowance.
- A capacity pass bounds selection by its 20-second budget, and revalidation checks it between steps: a selected checkout that cannot be revalidated in time is deferred, not removed. Not bounded: a revalidation step already running (lease and gate checks, the cleanliness walk) and Git's removal of the one checkout the pass started. Between passes the lock stays free at least as long as the last pass held it (and at least 500 ms), so a slice holds it at most about half the time.
- Before every pass the slice waits out memory or load pressure on the unattended pacer; pressure that outlasts the pacer's pause budget or the slice's deadline stops the slice before it removes anything (`pacer_backoff`). After each removal the pacer charge credits only the time Git spent removing the checkout, at the configured rate, never more than the entries charged, and sleeps the rest at `[cleanup] unattended_unlinks_per_second`; disk-pressure mode no longer lifts the rate for these charges (it still does for cache tree removal). A charge the slice deadline cuts short is owed: the next slice waits it out first. A checkout Git started deleting but failed to remove (for example `ENOTEMPTY`) is charged like a removal, and a slice stops after two such failures (`removal_failures`, 10-minute retry).
- The operator commands `gc apply`, `gc abandon` and `gc forget-abandoned`, and `broker start` at the checkout limit (which waited 5 seconds), now wait up to 30 seconds for a GC lock holder instead of refusing at once, keeping an empty file named after its PID under `.aethyme/run/gc-lock-waiters/` while they wait. The worker checks it before each capacity pass (and stops the slice) and before each cache pass (and waits, up to 30 seconds, for the waiter to take the lock); retirement passes do not check it. A broker open resuming a confirmed GC journal still fails fast on a held lock and announces nothing. A notice older than 90 seconds, or whose process is gone, is stale and removed; nothing else in that directory is touched. Still failing fast or deferring on a held GC lock: the post-close cleanup wake in `finish` (reports `reclaim_deferred`; the worker does the reclaim), broker opens' inline maintenance, and the library calls `auto_cleanup_now`, `auto_cleanup_plan_now` and `auto_cleanup_apply_now` (return nothing).
- Retry after a slice: pressure that outlasted the pacer, 10 minutes whatever was removed (no further capacity slice that worker run); two Git removal failures, 10 minutes; a failure mid-slice, 1 minute (the slice record is still written); a slice that removed nothing after keeping a checkout at the last step, at least 1 minute; a lap that progressed without ending, 20 seconds instead of 1 minute; an unpaid pacer wait, at least that wait. A slice that yielded to a foreground command without removing anything records outcome `lock_busy` and retries after 1 minute.
- The `auto_cleanup.capacity_removal` record gains `stop_reason` (`cap`, `budget`, `pacer_backoff`, `slice_deadline`, `yielded_to_foreground`, `nothing_selected`, `kept_only_limit`, `lock_busy`, `failed`, `removal_failures`; absent in records written before), `removal_cap`, `kept_only_passes`, `removal_failures`, `pass_budget_ms`, `lock_held_ms` (pass time under the GC lock), `lock_wait_ms` (time on a pass that found it held), `lock_yield_ms` (free-lock time between passes beyond the pacer wait), `git_removal_ms`, `pressure_wait_ms`, `pacer_credited_entries`, `pacer_charged_entries`, `pacer_unpaid_ms`, `cache_lane_reserved_ms` and `retry_in_ms` (the chosen wait, absent when the next slice may run at once). The worker status after a slice appends ` stop=<stop_reason>`.
- Phase telemetry (the worker's `command-metrics.jsonl` row) adds the timed phases `artifact_worker_capacity_removal_lock_held`, `_lock_wait`, `_lock_yield`, `_pressure_wait`, `_git_removal`, `_retry` (the chosen wait, 0 for none) and `_cache_lane_reserved` (all prefixed `artifact_worker_capacity_removal`), and the counters `artifact_worker_capacity_removal_stop_<stop_reason>`, plus the timed phase `artifact_worker_cache_yielded_to_foreground`.
- `.aethyme/gc.lock` is now created with the holder's PID already in it (a staged file hard-linked into place, falling back to create-then-write where hard links are unavailable), and a PID-less lock file younger than 5 seconds is treated as held rather than stale. `artifact_worker_capacity_removal_paced` is unchanged.

Consumers must tolerate the new fields, counters, status suffix and notice directory. Covered by `auto_cleanup.rs` `a_worker_capacity_slice_caps_its_removals`, `the_worker_slice_removes_more_than_four_while_the_backlog_lasts`, `a_kept_checkout_does_not_consume_the_removal_allowance`, `a_worker_capacity_slice_yields_to_a_waiting_foreground_command`, `a_worker_capacity_slice_removes_nothing_under_pressure`, `pressure_after_a_removal_backs_off_instead_of_retrying_at_once`, `kept_only_passes_are_bounded_and_never_repeat_a_checkout`, `a_capacity_pass_is_bounded_by_the_pass_budget`, `start_announces_itself_while_it_waits_for_the_gc_lock`, `pressure_that_outlasts_a_short_slice_still_backs_off_ten_minutes` and `a_pacer_wait_cut_by_the_slice_end_is_owed_by_the_next_slice`; `gc.rs` `a_capacity_slice_stops_for_budget_or_a_foreground_waiter`, `the_worker_slice_limits_bound_the_lock_and_allow_a_backlog`, `the_gc_lock_stays_free_at_least_as_long_as_a_pass_held_it`, `capacity_slices_leave_the_cache_lane_its_minimum_share`, `a_slices_stop_sets_a_real_back_off`, `a_gc_lock_waiter_notice_counts_only_live_other_processes`, `a_stale_gc_lock_waiter_notice_expires`, `waiting_for_the_gc_lock_announces_the_waiter`, `a_journal_resume_fails_fast_on_a_held_gc_lock_without_a_notice`, `the_gc_lock_file_carries_its_pid_and_a_young_empty_one_is_held` and `an_unfinished_capacity_lap_retries_soon_and_a_finished_one_backs_off`; and `gc/unattended_pacing.rs` `a_bulk_removal_is_charged_to_the_installed_pacer_only`, `a_bulk_charge_credits_only_the_removals_own_duration`, `a_bulk_charge_ignores_the_disk_pressure_lift` and `a_bulk_charge_and_the_pre_removal_gate_back_off_under_pressure`.

### Load throttles unattended removal instead of stopping it

Introduced 2026-11-15. On the busiest repository (10 cores, memory fine, a one-minute load average around 50 for more than a day from many short-lived processes and I/O waits) no background checkout removal ran from 0.8.96 on: the pre-pass check waited out "load far above the core count" for the whole slice and stopped it (`stop_reason` `pacer_backoff`, `passes` 0, `retry_in_ms` 600000), for both triggers, and every such slice also grew the steady idle streak. The checkout count kept climbing at the limit. Memory pressure is the hazard the pacer exists for; load on its own is not. Changes:

- Memory pressure (macOS `kern.memorystatus_vm_pressure_level` above normal, Linux PSI memory) is unchanged: no removal starts while it lasts, it is waited out in one-second steps at most 120 s per worker run and never past the deadline, and when it outlasts that the slice stops with the new `stop_reason` `memory_backoff` (ten-minute retry, a hard steady retry as before). `pacer_backoff` is no longer written; records written by older binaries keep it.
- Load (one-minute load average above twice the core count, or Linux PSI I/O `some avg10 >= 40`) never stops unattended work. The check before a capacity removal pass waits at most 5 seconds for it to clear; when it has not, the pass runs anyway and the slice makes at most one removal, then stops with the new `stop_reason` `load_throttled` and keeps the normal cadence (one minute after a steady slice; under capacity pressure at least 30 seconds since the follow-up below); it is not a hard retry. Tree removal (cache lane, artifact reclaim, closed-session reclaim) and the bulk charge after a checkout removal wait at most 1 second per 256-entry batch while load lasts, on top of `[cleanup] unattended_unlinks_per_second`, and go on; load waits never spend the 120-second memory pause budget. At the default rate that is about 200 entries a second under sustained load instead of 1,000.
- A slice that ran no pass because of pressure records the new outcome `pressure_before_selection` (not `out_of_time_before_selection`), does not grow `gc.artifact_sweep.steady_removal_idle_streak` and neither grows nor resets `gc.artifact_sweep.capacity_removal_out_of_time_streak`; it retries after 10 minutes when memory stopped it (`stop_reason` `memory_backoff`), and after 1 minute when load did (`stop_reason` `budget`, the slice reached its pressure check with at least the 5-second minimum slice left and the bounded load wait then left too little for a pass). Since the follow-up below, a slice that only saw load but spent its budget before the check, or whose pass failed, keeps `out_of_time_before_selection`.
- The `auto_cleanup.capacity_removal` record gains `memory_wait_ms` and `load_wait_ms` (pacer waits during the slice, pre-pass checks and removal charges together), `pressure_seen` (`memory`, `load` or `memory_and_load`; omitted when none) and `throttled`. `pressure_wait_ms` keeps its meaning (time in the pre-pass checks). The worker status after a slice appends ` pressure=<memory|load|memory_and_load|none> throttled=<bool>`.
- The unattended pacing summary (`last_artifact_sweep.unattended_pacing`) gains `load_throttled` (checks that proceeded throttled under load); `pressure_yields` and `memory_pause_ms` now count memory pressure only.
- Phase telemetry (the worker's `command-metrics.jsonl` row) adds the timed phases `artifact_worker_capacity_removal_memory_wait` and `_load_wait`, the counters `artifact_worker_capacity_removal_throttled`, `_stop_memory_backoff`, `_stop_load_throttled` and `_outcome_pressure_before_selection` (prefixed `artifact_worker_capacity_removal`), and, for every worker run, the timed phases `artifact_worker_host_deleter_wait`, `artifact_worker_pacer_memory_pause`, `artifact_worker_pacer_load_wait` and `artifact_worker_pacer_limiter_sleep` and the counters `artifact_worker_pacer_memory_backoffs` and `artifact_worker_pacer_load_throttled`. A worker run writes its row when it exits, after up to 15 minutes waiting for the host deleter slot plus its five-minute budget, so a row can arrive more than 20 minutes after the run's first slice record; the host deleter wait is now visible in it.

Consumers must tolerate the new stop reasons, outcome, fields, counters and status suffix, and the absence of `pacer_backoff` in new records. Every removal proof and safety gate is unchanged. Covered by `unattended_pacing.rs` `sustained_load_throttles_tree_removal_and_never_stops_it`, `sustained_load_lets_a_removal_start_after_a_bounded_wait`, `load_that_clears_mid_wait_is_clear` and `persistent_memory_pressure_yields_within_the_deadline_and_the_run_budget`; `tree_removal.rs` `sustained_load_throttles_unattended_removal_without_stopping_it`; `auto_cleanup.rs` `sustained_load_still_removes_a_landed_checkout_per_steady_slice`, `sustained_memory_pressure_backs_off_without_growing_the_idle_streak`, `load_that_clears_mid_wait_leaves_a_normal_steady_slice`, `capacity_pressure_makes_progress_under_sustained_load` and `a_worker_capacity_slice_removes_nothing_under_pressure`; and `gc.rs` `a_slices_stop_sets_a_real_back_off`.

### Load-throttled removal keeps a gap, and pressure-blocked slices are only those load blocked

Introduced 2026-11-15, follow-up to the section above. Three corrections from its review:

- Classification. A slice is `pressure_before_selection` for load only when load is what prevented its pass: it stopped for budget (`stop_reason` `budget`), it reached its pre-pass pressure check with at least the 5-second minimum slice left, and it saw load. On a machine whose load is permanently above the threshold every slice sees load, so seeing it proved nothing: a slice whose pass failed before removing anything was recorded `pressure_before_selection` with a one-minute retry. Such a slice, and one whose budget went on counting or the other checks before the pressure check, now keeps the classification it had before load became a throttle: `out_of_time_before_selection` with `budget_consumed_by` and the escalating 1, 2, 4, 8, then 10-minute retry (`gc.artifact_sweep.capacity_removal_out_of_time_streak` grows), and `stop_reason` `failed` for a failed pass. Memory pressure is classified as before.
- Throttle gap. Under capacity pressure a slice that removed a checkout clears its retry, a worker run holds up to eight slices (one before its cache work and one after each further cache pass) and broker opens start the next run as soon as the last exits, so under sustained load removal could run with almost no gap for as long as the repository stayed at its limit. A slice that stops `load_throttled` now sets a retry of at least 30 seconds (one cache pass, half the steady minute), within the run and across runs. Without load nothing changes; a steady slice keeps its minute. The slice record gains `throttle_gap_ms` (what the throttle added to the wait: 30000 under `pressure`, 0 otherwise).
- Visibility. The slice record gains, for the worker run it belongs to so far, `run_slices` (this slice's number in the run), `run_removed`, `run_elapsed_ms` (from taking the host deleter slot), `run_deleting_ms` (Git removal time, summed), `run_waiting_ms` (pacer charges, pressure checks and GC lock gaps and waits, summed), and `run_deleting_pct` / `run_waiting_pct` (whole percent of `run_elapsed_ms`). The worker status after a slice appends ` throttle_gap_ms=<n> run_slices=<n> run_removed=<n> run_deleting_pct=<n> run_waiting_pct=<n>`. The worker's `command-metrics.jsonl` row gains the counter `artifact_worker_capacity_removal_slice` (one per slice: slices per run) and the timed phases `artifact_worker_capacity_removal_waiting` and `artifact_worker_capacity_removal_throttle_gap` (count: slices that applied the gap); removals per run are the existing `artifact_worker_capacity_removed` count and deleting time the existing `artifact_worker_capacity_removal_git_removal`.

Memory pressure stays a hard stop everywhere; every removal proof and safety gate is unchanged. Consumers must tolerate the new fields, counters and status suffix. Covered by `gc.rs` `a_slices_stop_sets_a_real_back_off` and `a_worker_run_tally_counts_its_own_slices`; `gc/unattended_pacing.rs` `memory_and_load_in_the_same_sample_is_a_memory_back_off`; and `auto_cleanup.rs` `capacity_pressure_makes_progress_under_sustained_load`, `without_load_a_pressure_slice_that_removed_applies_no_throttle_gap`, `memory_and_load_in_one_sample_stop_a_slice_as_memory_pressure`, `a_throttled_removal_resets_a_stale_idle_streak`, `under_sustained_load_a_slice_out_of_time_before_selection_keeps_its_streak`, `under_sustained_load_a_failed_pass_is_not_pressure_blocked` and `a_slice_the_load_wait_left_without_a_pass_is_pressure_blocked`.

### Steady checkout removal finishes its lap before backing off

Introduced 2026-11-15. On the busiest repository steady removal ran but removed nothing: each slice ended at its first pass that selected nothing (`stop_reason` `nothing_selected`, `passes` 1) after judging about 9 lap groups, and counted toward the idle back-off although over 100 lap groups were never judged; the priority lane re-judged the same kept checkouts every 30 minutes. Changes (the `pressure` trigger is unchanged; no proof or gate changed):

- A steady pass that selects nothing but moved the lap cursor or judged a checkout no longer ends the slice: passes go on, under the same budget, GC lock gap, foreground yield, memory and load gate and pacer, until the lap reaches its end (new `stop_reason` `lap_complete`, telemetry `artifact_worker_capacity_removal_stop_lap_complete`) or a pass moves nothing (`nothing_selected`).
- `gc.artifact_sweep.steady_removal_idle_streak` now grows only after a steady slice that reached the end of the lap, or moved nothing, with nothing removed. A slice that removed nothing with the lap unfinished and moving leaves it unchanged and sets `gc.artifact_sweep.steady_removal_retry_after_ms` to 1 minute. The ladder (5, 10, 20, 30 minutes), the hard retries and the early wake on a newly due cleanup request are unchanged.
- The priority memo (`auto_cleanup.priority_memo`) entry gains an optional `stable` object (`code`, `sessions`, `blockers`, `fingerprint`): a priority group kept by selection with code `ownership_adopted_only`, `ownership_witness_legacy`, `ownership_witness_missing`, `ownership_unproven`, `checkout_missing` or `ignored_content` (with its blockers) is remembered for the keep memo's re-proof age (7 days) instead of 30 minutes and is skipped by the priority lane while its merged state, its sessions (id, origin, status, cleanup state) and its file modification times are unchanged. The lap still judges it. Every other keep keeps the 30-minute re-judge. Entries written before have no `stable` and behave as before.
- The `auto_cleanup.capacity_removal` record gains `lap_completed`, `priority_memo_skips` and `memo_skips`; `auto_cleanup.lanes` gains `removal_priority_memo_skips` (omitted when 0). The worker status after a slice appends ` passes=<n> lap_completed=<bool>`.
- Phase telemetry adds the counters `artifact_worker_capacity_removal_pass`, `_priority_memo_skips`, `_memo_skips`, `_lap_completed` (prefixed `artifact_worker_capacity_removal`) and `auto_cleanup_priority_memo_skip`.

Consumers must tolerate the new stop reason, fields, memo object, counters and status suffix. Covered by `auto_cleanup.rs` `an_unfinished_steady_lap_reaches_a_landed_checkout_in_the_same_slice`, `steady_removal_backs_off_only_after_a_full_empty_lap`, `stable_kept_priority_groups_are_skipped_until_their_state_changes` and `transient_priority_keeps_stay_retryable`; `gc.rs` `a_steady_pass_that_moved_the_lap_is_progress_not_an_empty_slice` and `steady_removal_backs_off_on_a_ladder_and_keeps_its_smaller_limits`.

### Worker checkout removal follows merges, not capacity pressure

Introduced 2026-11-14. The worker's checkout removal slice ran only under capacity pressure (three quarters of `[worktrees] max_checkouts`). On the busiest repository, with the limit raised, the count climbed about 13 an hour and nothing was removed until it would reach the pressure threshold. Changes:

- The slice now also runs without pressure as the steady removal of landed checkouts (`trigger` `steady`): at most four removals in up to 60 seconds, ending when a pass selects nothing or after two passes whose selected checkout was kept at the last step; each pass judges the priority lane (merged pull request, due cleanup request) and then at most 16 groups of the oldest-first lap from its stored cursor, with no capacity probe. Under pressure (`trigger` `pressure`) the limits, retries and records are unchanged. Every proof, keep code, artifact-use marker, the GC lock and its gap, the yield to a waiting foreground command, the memory and load gate and the unattended pacer apply to both; only when removal runs changed. `[cleanup] auto_remove = false` turns both off.
- Steady retry, under the new meta key `gc.artifact_sweep.steady_removal_retry_after_ms` (namespaced like the other worker keys): 1 minute after a slice that removed something; 5, 10, 20, then 30 minutes after consecutive slices that removed nothing (`gc.artifact_sweep.steady_removal_idle_streak`); 1 minute when the GC lock was contended; the pressure stop floors still apply (pacer back-off and two failed removals 10 minutes) and are kept under `gc.artifact_sweep.steady_removal_hard_retry_after_ms`. Steady removal is due only while a spawned session is closed with its checkout not yet removed, and no hard retry is pending; then when the steady retry passed, or at once when a closed checkout's cleanup request became due after the last steady slice (`gc.artifact_sweep.steady_removal_last_run_ms`). A broker open wakes the worker when it is due; a run due for it that cannot reach its slice (busy host deleter slot, unverified runtime, launch failure, no time left) sets the hard retry for 1 to 5 minutes. The trigger compares the recorded count with the limit configured now, so a count recorded against another limit (or with the limit since disabled) never makes a slice `pressure`.
- The slice counts checkouts from the registrations only; without pressure it never falls back to `git worktree list`.
- The `auto_cleanup.capacity_removal` record gains `trigger` (`steady` or `pressure`; absent in records written before), `priority_examined`, `priority_removed`, `lap_examined`, `lap_removed` and `idle_streak`. `auto_cleanup.lanes` gains `removal_priority_selected` (omitted when 0), and its `capacity_pressure` is false for a steady slice's passes. The worker status after a slice appends ` trigger=<trigger>`.
- Phase telemetry adds the counters `artifact_worker_capacity_removal_trigger_steady`, `_trigger_pressure`, `_priority_examined`, `_priority_removed`, `_lap_examined` and `_lap_removed` (prefixed `artifact_worker_capacity_removal`), the timed phase `session_retirement_yielded_to_foreground`, and the counter `artifact_worker_steady_removal_deferred`.
- The retirement worker now checks the GC lock waiter notice before each pass: it waits for the waiter (up to 30 seconds, within its tranche) and, when it is still waiting, defers the retirement request (`session_retirement_yielded_to_foreground`) instead of taking the lock.

Consumers must tolerate the new fields, counters, meta keys and status suffix. Covered by `auto_cleanup.rs` `without_pressure_the_worker_removes_a_merged_checkout`, `without_pressure_a_checkout_whose_work_did_not_land_is_kept`, `steady_removal_backs_off_while_nothing_is_removable`, `steady_removal_honors_auto_remove_off`, `a_slice_stops_after_two_failed_removals`, `steady_removal_leaves_an_open_sessions_checkout`, `steady_removal_waits_for_the_closed_worktree_grace`, `a_steady_selection_judges_at_most_sixteen_lap_groups`, `steady_removal_follows_a_pressure_episode`, `a_disabled_limit_runs_steady_removal_despite_a_stale_count`, `a_steady_slice_stops_after_two_failed_removals`, `steady_removal_is_not_due_without_a_closed_checkout`, `a_newly_due_cleanup_request_ends_the_idle_back_off` and `the_worker_capacity_slice_removes_and_then_backs_off`; `gc.rs` `steady_removal_backs_off_on_a_ladder_and_keeps_its_smaller_limits` and `session_retirement_yields_to_a_waiting_foreground_command`.

### Volume identity for artifact-use markers and the preparation rotation journal

Introduced 2026-11-13. Device numbers are assigned at mount time, and macOS gives an external volume a new one when it returns in another disk slot, so durable records that saved `st_dev` and compared it exactly failed forever. Two live failures are fixed here; the shared rule is that a volume is identified by its stable volume UUID when both sides have one, and by the exact device number otherwise (never weaker than before). The UUID comes from `getattrlist` `ATTR_VOL_UUID` on macOS and is read fresh on every lookup (no cache: a different disk in the same slot and mount point would be indistinguishable to one); Linux and other targets report it unknown and keep the exact device comparison.

- Artifact-use marker inventory (collectors: `finish cleanup`, automatic cleanup, GC, gates, session retirement): a schema-1 marker whose paths, inodes, checkout key and nonce match but whose saved device numbers differ is listed as an unresolved marker (rule `legacy_marker_device_renumber`) instead of failing the inventory with "artifact-use marker primary root device does not match". The checkout is still kept: a marker never authorizes removal, and when a marker counts as resolved is unchanged. Live producers keep the exact device comparison. Phase telemetry adds the counter `artifact_use_marker_legacy_device_renumber` (one per such marker).
  - The `finish cleanup` refusal for such markers reads `artifact-use markers are unresolved; preserved the worktree; <n> of them were saved before a device renumber (rule legacy_marker_device_renumber): ...` and says what can be done: `reconcile plan` removes one only when a spawn or registry record saw its old device numbers; one it retains for lack of that record stays until the user confirms its producer is gone and deletes the marker file under `.aethyme/run/artifact-use/` by hand. Refusals without such markers keep their old text.
  - Session retirement: `retirement_deferred_reasons[]` gains `{"kind": "legacy_artifact_markers", "session_id", "markers"}`, reported as actionable in `broker status` advice (it previously surfaced as `operation_evidence_unavailable`; without this kind it would have read as `operation_active`). The landed lane still counts it as `operation_unknown`.
- Preparation rotation journal (`.aethyme-preparation-rotation-*.sqlite3`, beside the host resource database): a new table `rotation_root_volume(root_path, root_identity, volume_uuid, rule, recorded_at_ms)` is created on first use without changing `user_version` (still 3); binaries from 0.8.2 on check only that `rotation_root` and `rotation_debt` exist and ignore it. A row binds a volume UUID to the exact saved `root_identity` bytes; it no longer applies once the root row's identity changes. `rule` is `observed` (recorded while the identity matched), `volume_uuid` or `legacy_root_marker_nonce`.
- Before recovery reads the journal:
  - A root row whose saved identity equals the live root exactly is refused when a volume UUID is recorded for it, the live UUID is known, and they differ (a different disk in the same slot can show the same device, filesystem ID and inode number). Without a recorded UUID it is accepted as before and the UUID is recorded.
  - An unsettled root row whose saved identity differs from the live root is rewritten to the live identity, keeping its generations, cursor and lap state, only when the live volume UUID is known, the inode is unchanged, no debt rows are pending, and either (a) the recorded volume UUID equals the live one (`volume_uuid`), or (b) the cache root's parent marker, whose random nonce names this root, names this host's resource database, was saved under the same device number and filesystem ID as the journal row, and the parent shares the root's filesystem now (`legacy_root_marker_nonce`).
  - On macOS a device number and filesystem ID name a disk slot and filesystem type, not a mount epoch, so rule (b) only shows the marker and the row were written while the drive had the same slot; it fails closed when the drive had another slot between creating the cache root and writing the row. Known limit: rule (b) cannot tell apart a different volume carrying a byte copy of the marker (same nonce and inode numbers); only reproducible cache contents are at stake.
  - Otherwise recovery stops with the stop reason `preparation cache root identity differs from its durable journal (root_volume_reproof_refused: <condition>)`; no command edits this journal. The stop reason ends with a `next:` hint (shown by `submit prepare` and in `preparation_rotation_recovery` of `broker advanced resources pressure --json`): mount the original volume at the same path and rerun `submit prepare`; if the refusal persists, the last resort is to remove that preparation cache root, which holds only reproducible package caches and which the next `prepare` recreates; never edit the host journal by hand. The same hint accompanies a `debt_volume_reproof_refused` stop that begins with the same `preparation cache root identity differs from its durable journal` text. Phase telemetry adds the counters `preparation_rotation_root_reproved` and `preparation_rotation_root_reproof_refused`.

Consumers must tolerate the new table, rule values, deferred-reason kind and refusal text. Migrated by later sections: rotation debt payloads and in-cache witnesses, fetch journals and the job service context (the next two sections), and schema-2 spawn witnesses and finish-cleanup recovery proofs in every deletion lane ("One ownership re-proof for automatic removal, GC and finish"). Archive-cleanup intents need no change: no production code writes one and the only reader refuses cleanup whenever one exists without comparing its devices. Still comparing device numbers exactly (not migrated): the candidates of the GC plan's environment-cache lane (`cache_root_device` and `entry_device` in `retention.rs` and `environments/cache.rs`), the git maintenance plan binding (`git_maintenance_lock.rs`: repository directory and objects directory identity), the coordination lock and intent keys `git-common:<device>:<inode>` that name the per-repository operation locks (`operations/git_scopes.rs`, `common_lock_key`), and the job repository key, which hashes device numbers. Live artifact-use marker producers also keep the exact device comparison by design. Other in-operation identity checks that compare a device and inode within one command (for example the GC state directory and the repository UUID marker directory) were not part of any inventory and are unchanged. Covered by `volume_identity.rs` tests; `artifact_use.rs` `inventory_lists_a_renumbered_legacy_marker_as_an_unresolved_blocker`, `inventory_counts_only_markers_whose_devices_actually_differ`, `inventory_rule_still_requires_paths_inodes_key_and_nonce`, `producers_still_compare_legacy_devices_exactly` and `inventory_refuses_a_marker_from_a_different_volume_with_equal_inodes`; `broker/leases.rs` `a_renumbered_legacy_artifact_marker_defers_close_distinctly`; `preparation_rotation_journal.rs` `root_volume_tests`; and `preparation_rotation_debt.rs` `root_volume_reproof_tests`.

### Volume identity for preparation rotation debt payloads and sidecars

Introduced 2026-11-14. Follows the root-row change above: a rotation interrupted mid-way left every record of its debt naming the old device number (the journal `rotation_debt` payload and `fresh_store_identity`, the in-cache witness `.aethyme-preparation-rotation-<repository>.json` and its scratch, the fresh-stage intent and proof, and the in-tree witness inside the original store), so after a remount in another disk slot the debt could neither finish nor be cleared, by `prepare` or by the gc cache sweep. Record formats are unchanged; older binaries keep reading every record.

- Preparation rotation journal, two new tables created on first use without changing `user_version` (still 3):
  - `rotation_debt_volume(root_path, repository, payload_sha256, root_identity, volume_uuid, rule, recorded_at_ms)`: one row per cache root and repository binds the volume UUID to the debt with that payload digest and to the exact cache-root identity bytes the debt saved. Written when a debt is journaled (rule `observed`), backfilled for a journaled debt whose identity still matches exactly (`observed`, only while the root row is bound to the same live UUID), and written after a re-proof under the rule that proved it. Removed when that debt's recovery completes; until then it outlives the debt row, because stage proofs of a completed debt can remain in the cache. At 512 rows the oldest bindings no journaled debt row holds any more (leaked when a completed recovery never retired them) are evicted to make room, each counted as `preparation_rotation_debt_volume_binding_evicted` once its write commits (since 0.8.98). A binding of a journaled debt is never evicted, and since the journal holds at most 128 debt rows a full table always has a leaked binding to evict; a stage proof that still needed an evicted binding is then refused, never accepted. `preparation_rotation_debt_volume_binding_cap_reached` remains as a defensive refusal that the current row bounds cannot reach.
  - `rotation_volume_key(root_path, filesystem_key, volume_uuid, recorded_at_ms)`: the filesystem keys (`<device>:<filesystem ID>`) a cache root was seen under while a re-proven recovery wrote records, with the UUID it had then. A key is protected while the root has any pending debt row, or while a debt binding of that root names its UUID (stage proofs can outlive the debt row); recovery records keys only under that protection. Since 0.8.98 unprotected keys are trimmed to the 16 newest per root and UUID whenever protection can lift (a debt row completes, a binding is forgotten) and before every new key is checked against the cap, so unprotected keys never cause a refusal. Protected keys stop at 256 per root and UUID: past that the key is refused (counter `preparation_rotation_volume_key_cap_reached`) and the recovery that needed it refuses (`proof_unrecorded`) until its debt is cleared. Re-recording a key already held under the same UUID writes nothing. These protections hold only while every writer of the journal runs 0.8.98 or later: an older binary prunes the 16 newest per root across all UUIDs, which can drop a protected key, and the record that needed it is then refused (`record_key_unproven`), never accepted.
- Recovery (`prepare` and the gc cache sweep): when the journal root row names an older identity of the same cache-root directory and debt rows are pending under it, the root row is no longer refused for that reason. The sweep now walks the journal under the saved identity and hands each debt to its repository's recovery; recovery addresses the journal under the saved identity until each debt completes. The debt's records are accepted only when the live volume UUID is known, the cache-root inode is unchanged, and either the UUID recorded for the debt equals the live one (`volume_uuid`), or the UUID recorded for the root row's saved identity does (`root_volume_binding`), or, with neither recorded, the parent root marker rule of the root re-proof holds (`legacy_root_marker_nonce`, same limits). A recorded UUID that differs refuses, as does an unreadable binding. Before anything is mutated, the root row's saved identity is bound to the live UUID and the live key is recorded in `rotation_volume_key`; if either write fails, recovery refuses. Once proven, the recovery call treats the saved key, the live key and every key recorded in `rotation_volume_key` under the live UUID as one volume; inode numbers and every other recorded field are still compared, and any other key still differs. This also covers a second renumber before the debt completes. When the last debt completes, the root row moves on to the live identity in the same call.
- A journaled debt whose saved identity matches the live root exactly is checked against its recorded UUID only where both exist: with a UUID recorded for the debt and today's UUID readable (macOS), a different UUID is refused, since another disk mounted in the same slot can show the same device number and cache-root inode. Where no UUID was recorded for the debt (journaled before 0.8.96, or its binding was never written, for example when the binding write failed) or today's UUID cannot be read (Linux and other targets), the exact identity is accepted as it always was: such a disk cannot be told apart from the original. A stage proof left behind after its journal row completed is also accepted on an exact identity match without any UUID comparison, whether or not a binding exists.
- After the journal row completes, a stage proof left behind is re-proven through the debt binding only while an in-cache record still names the binding's saved identity; otherwise no proof is attempted and nothing is counted.
- Refusals stop recovery with `preparation cache root identity differs from its durable journal (debt_volume_reproof_refused: <condition>)`, or, for a record that needs a renumber no proof covers, `<record> names this cache root under another device number (debt_volume_reproof_refused: <condition>)`, where the condition is the one the attempted proof failed on, or `no recorded volume UUID or root marker proves that its saved device names this volume`. A record written under a key that no recorded UUID binds (only if its `rotation_volume_key` row was pruned or lost) reads `a record names a device number that no recorded volume UUID binds to this volume`; its contents are reproducible cache data, so the manual route is to remove that preparation cache root, which the next `prepare` recreates. Linux and other targets have no stable volume identifier, so nothing is re-proven there and the exact comparison stays; the stop reason then reads `debt_volume_reproof_refused: the live volume UUID is unknown: ...` where it previously read `root_volume_reproof_refused`.
- Diagnostic reads (the recovery-state generation label and the remaining-entries count of an incomplete recovery) no longer compare the device number, so a debt that recovery will re-prove is not reported as refused there.
- Phase telemetry adds the timed phases `preparation_rotation_debt_volume_proof` and `preparation_rotation_debt_volume_record`, and the counters `preparation_rotation_debt_volume_recorded`, `preparation_rotation_debt_volume_binding_cap_reached`, `preparation_rotation_debt_volume_reproved` (with `_volume_uuid`, `_root_volume_binding` or `_legacy_root_marker_nonce`: the rule that first proved the volume), and, once per refused recovery call, `preparation_rotation_debt_volume_reproof_refused` plus one `preparation_rotation_debt_volume_refused_<condition>` counter (`live_volume_unknown`, `inode_differs`, `debt_volume_differs`, `root_volume_differs`, `no_root_marker`, `marker_other_coordinator`, `marker_other_slot`, `marker_split_filesystem`, `unproven_record`, `binding_unreadable`, `proof_unrecorded`, `saved_identity_malformed`, `record_key_unproven`).

Consumers must tolerate the new tables, rule values and refusal text. Covered by `preparation_rotation_journal.rs` `debt_volume_tests`, `preparation_rotation_debt.rs` `debt_volume_reproof_tests`, `storage.rs` `recovery_sweep_resumes_a_journal_only_debt_saved_before_a_device_renumber` and `preparation.rs` `a_journaled_rotation_debt_records_its_volume_uuid_at_once`.

### Volume identity for fetch journals and the job service context

Introduced 2026-11-14. The same rule as above (a volume is identified by its stable volume UUID when both sides have one, by the exact device number otherwise) now covers two more durable records. Linux and other targets have no stable volume identifier here and keep the exact device comparison.

- Fetch journal (`details_json.fetch_reconciliation` of a coordinated Git fetch): gains the optional field `common_volume_uuid` (16 bytes as a JSON array), omitted when the UUID is unknown, so journals on Linux keep their exact bytes. `schema_version` stays 1. A journal whose saved common-directory device differs from the live one is used to reconcile only when its path and inode match and the recorded UUID equals the live one (rule `volume_uuid`). Otherwise the journal is not used and the operation falls back to the repeatable-fetch resolution exactly as before ("fetch again"). Equal devices with a recorded UUID naming another volume are refused too. When the live volume UUID is known and the volume evidence refuses (a recorded UUID naming another volume, or a journal without one), stderr reads `[coordination] fetch evidence of operation <id> was not used (fetch_common_volume_refused: <condition>)`; a changed path or inode, and a changed device without a live UUID (the historical exact refusal), stay silent. An older binary cannot parse a journal carrying the field (the journal refuses unknown fields) and takes the same fallback. Phase telemetry adds the counters `fetch_journal_common_volume_reproved`, `fetch_journal_common_volume_refused` (device or volume conditions) and `fetch_journal_common_identity_refused` (path or inode).
- Job service context (`broker-service-jobs-context.json` in the service directory): the volume UUIDs of the runtime directory (which also holds the singleton lock and job database), the repository root and the common Git directory are written to a new sidecar `broker-service-jobs-context-volumes.json` (`{schema_version: 1, context_sha256, runtime, root, common}`), bound to the exact context bytes, and only when at least one UUID is known. The context format is unchanged; older binaries ignore the sidecar, and a context they rewrite no longer matches its digest. A context whose saved devices differ is accepted only when every path, the repository UUID and every inode match and, for each object whose device differs, the UUID recorded with that context equals the live one (and the lock and database share the directory's device). Otherwise startup fails with `service job context identity changed (service_context_volume_refused: <object>: <condition>)` for volume conditions, or `(service_context_identity_refused: <condition>)` when a path, the repository UUID or an inode differs or the context's schema or worker generation is malformed. Refusals that only removing the context can clear (no volume UUIDs recorded for this context, or a recorded UUID naming another volume with equal devices) end with a hint naming the context and job database files to review. An accepted re-proof prints `[coordination] service job context re-proved by volume UUID (rule volume_uuid): <service directory>` on stderr; the service runs outside the per-command metric recorder, so it records no phase counters. The job repository key still hashes device numbers, so jobs queued before a remount are not found under the new key.
- Archive-cleanup intents keep their device fields unchanged: no production code writes an intent yet, and the only reader refuses cleanup whenever an intent exists without comparing them.

Consumers must tolerate the new optional journal field, the sidecar file, and the refusal texts. Covered by `operations/git_scopes.rs` `a_renumbered_common_directory_is_proved_by_its_recorded_volume_uuid`, `every_condition_of_the_common_directory_reproof_is_required`, `a_prepared_journal_survives_a_device_renumber_only_through_its_volume_uuid` and `fetch_journal_identity_decisions_are_counted_by_class`, and `service/job_runtime.rs` `a_renumbered_context_is_proved_by_the_volume_uuids_recorded_with_it`, `every_condition_of_the_context_reproof_is_required`, `recorded_volumes_bind_to_their_schema_and_exact_context_bytes` and `a_service_context_survives_a_device_renumber_only_through_recorded_volume_uuids`.

### One ownership re-proof for automatic removal, GC and finish

Introduced 2026-11-14. Only automatic whole-checkout removal accepted a saved checkout identity whose device numbers macOS had renumbered; GC reclamation and checkout compaction, `finish` / `finish cleanup` and the finish-cleanup recovery proof compared device numbers exactly, so after a remount they refused (GC: cohort code `spawned_witness_mismatch`; finish: no recovery proof recorded, or an interrupted cleanup that could never resume). All of them now use one re-proof (`Broker::checkout_ownership_reproof`). Its filesystem half requires unchanged paths, all four inodes and the spawn root generation, and either every recorded volume UUID equal to today's UUID of that object (`volume_uuid`) or, for a record saved without UUIDs, an unchanged device-sharing pattern (`device_renumber`, the #1665 rule). In this comparison a recorded UUID that today's platform cannot observe is refused, never compared by device, whether or not the device numbers changed. Its Git half is the #1665 evidence: a closed spawned session naming the checkout under the broker worktree root, and this repository's registration of it with `HEAD` on the session branch. `HEAD` is normally a symbolic ref to that branch, so "at the branch tip" only proves the branch resolves to a commit; it binds `HEAD` to a commit only where the broker recorded one, which today is the finish recovery proof's `head` (refusal `head_moved`). Witness lanes (automatic removal, GC, finish's recovery-proof capture) record no commit and bind `HEAD` to the branch only. Schema-3 spawn witnesses keep their existing UUID comparison, which needs no re-proof. Linux and other platforms record no UUIDs: they keep the exact device comparison, and the #1665 device-renumber rule for records saved without UUIDs now applies on every platform in GC reclamation and compaction and in registered finish recovery, as it has in automatic removal since 0.8.93. The one exception is the registered finish-cleanup recovery proof: when its four saved device numbers equal today's and a recorded UUID cannot be read today, it keeps the exact acceptance recovery always made (a UUID that reads differently is still refused); under other device numbers it is refused.

- `FinishCleanupRecoveryProof` (the `recovery_proof` of `session.finish_cleanup_started` events) gains `volume_uuids` (`{"worktree","git_pointer","git_admin","git_common"}`, 16-byte arrays), written only where the platform reports them; `schema_version` stays 3. Binaries before 0.8.96 refuse such a proof (its type denies unknown fields). On macOS every new proof carries the field, so a downgraded binary cannot resume any interrupted cleanup the new binary started, remount or not; the checkout is kept, never removed.
- Recovery after a remount: a registered interrupted cleanup resumes through the re-proof; an orphan (Git already deregistered it) resumes only by volume UUID, since the registration half cannot be made, and the removal's descriptor checks are rebound to today's device numbers of the same re-proven objects. An orphan proof saved without UUIDs is refused.
- `finish` records its recovery proof when the spawn witness matches exactly or passes the re-proof; the re-proof judges the session as the cleanup start leaves it (closed, cleanup pending), because that same transition, under the same lifecycle claim, writes the proof.
- `finish` reports gain `cleanup.ownership_reproof` (`volume_uuid` or `device_renumber`, omitted for an exact match) when the interrupted cleanup it resumed needed the re-proof, and since 0.8.98 also when `finish` itself needed it to record its recovery proof (a clean `finish` after a remount), whether or not the removal then completes; the removal itself reports it, so finishing adds no extra check. The GC worker status line reads `compact_checkout_removed ownership_reproof=<kind>; ...` for such a compaction.
- The closed-checkout cohort gains the unavailable code `ownership_reproof_refused` (the objects are the same but the Git half refused), apart from `spawned_witness_mismatch` (a different or replaced checkout). Automatic removal keeps its `kept[]` codes and reasons.
- Phase telemetry adds `gc_ownership_reproved`, `gc_ownership_reproof_refused`, `gc_ownership_reproof_unavailable`, `finish_ownership_reproved`, `finish_ownership_reproof_refused` and `finish_ownership_reproof_unavailable` and the timed phases `gc_ownership_reproof_registration` and `finish_ownership_reproof_registration` (the automatic lane keeps its `auto_cleanup_ownership_reproof_*` names; since 0.8.98 the retirement plan and archive cleanup a checkout compaction runs count under `retirement_ownership_reproved`, `retirement_ownership_reproof_refused`, `retirement_ownership_reproof_unavailable` and the timed phase `retirement_ownership_reproof_registration` instead of `gc_*`, and the orphan device rebind just before an orphan removal counts nothing), plus one counter per refusal reason, `ownership_reproof_refused_<reason>`: `saved_unsupported`, `current_unsupported`, `path_changed`, `generation_changed`, `inode_changed`, `volume_uuid_differs`, `volume_uuid_unavailable`, `device_sharing_changed`, `nothing_to_reprove`, `not_spawned`, `not_closed`, `session_path`, `outside_broker_root`, `other_repository`, `not_registered`, `not_on_session_branch`, `head_not_branch_tip`, `head_moved` and `orphan_legacy_proof`. Read-only plans, reports, the interrupted-cleanup checks other than the one that acts, and the orphan device rebind make the same proof without counting it. Two rechecks do count, in their own lane: the retirement plan and archive cleanup a checkout compaction runs count under `retirement_*` (since 0.8.98), so one compaction can show a `gc_*` decision and `retirement_*` rechecks; and automatic removal's recheck just before the removal counts again under `auto_cleanup_*`, as it has since 0.8.93.

Consumers must tolerate the new field, values and code. Covered by `events.rs` `identity_*` and `a_recovery_proof_*` tests, `auto_cleanup.rs` `ownership_reproof_names_the_reproof_that_held`, `every_ownership_reproof_refusal_has_its_own_counter` and `gc_reclaim_reproves_a_renumbered_witness_and_reports_each_refusal`, and `broker/cleanup.rs` `finish_recovery_*`, `orphan_recovery_*` and `finish_records_*`.

### Gate records name their checkout; automatic removal keeps only checkouts a gate could be using

Introduced 2026-11-15. Automatic removal of a closed checkout kept it (`gate_running`) whenever any gate of the repository was running, including other sessions' gates; on the busiest repository gates run almost constantly, so 7 of 16 judged checkouts were kept that way and background removal removed nothing. Changes:

- The gate pidfile `.aethyme/run/gates/<session>-<gate>.pid` gains a sixth field: the canonical checkout the gate command runs in, as lowercase hex of the path bytes (`-` when it could not be resolved). Readers that take the first two, four or five fields are unaffected. An older binary's strict per-session inventory (guarded task retirement) accepts at most five fields, so it reports a session whose gate a newer binary is running as unknown and declines to retire it while that gate runs; that is the fail-closed direction.
- A gate owner lock's holder line (`.aethyme/run/gates/owners/*.lock`) becomes `<worker> since=<ms> pid=<pid> start=<start|-> checkout=<hex> end`. Readers that use the first token and `since=` (the "held by" text and `broker status` waiters) are unaffected. The holder empties the line before it releases the lock. A reader trusts the checkout only when the line ends in `end`, names `pid=`, `start=` and `checkout=` exactly once each, and its `pid` is still the process with that `start` time; anything else (an older binary's line, a line cut short, a crashed holder's line) is "owner unknown".
- Automatic removal (selection and the revalidation immediately before removal) keeps a closed checkout for a running gate only when one of the checkout's own sessions runs it (its pidfile name, or the strict per-session inventory guarded retirement uses), when the gate's recorded checkout overlaps the candidate by path (component-wise, raw and canonical) or Git directory, or when nothing identifies the gate's checkout. A gate recorded in another checkout is set aside; a gate in the main checkout keeps only checkouts nested inside it. Selection reads the gate records and the per-session inventory once per pass; revalidation reads both fresh for each checkout under the lifecycle claim. What stops a gate from starting in the checkout after that read is the artifact-use producer marker gate admission registers for the gate's checkout, which the removal's artifact-use reclaimer excludes, together with the pidfile recording that checkout. Admission alone does not refuse every closed session's checkout: a gate run for an open session may use another linked worktree of the same repository.
- Accepted residuals: a gate is attributed only to the checkout it runs in. A gate command that works outside that checkout (it changes into a sibling worktree, or is given another checkout's path as an argument) is not attributed to that other checkout. A checkout moved or remounted after its gate started is attributed by the path recorded at start. In both cases the remaining guards are the artifact-use marker and the open-file and working-directory check made immediately before removal.
- `auto_cleanup.kept[]` gains `gate_keep` (`own_session`, `path` or `unknown_owner`, only with code `gate_running`); the code itself is unchanged, and the reason now starts `a gate may be running (own session)`, `(this checkout's path)` or `(owner unknown)`. `auto_cleanup.lanes` gains `removal_gate_checks` and `auto_cleanup.capacity_removal` gains `gate_checks` (both absent when no check ran): `checks`, `scans`, `gates_seen`, `attributed_here`, `attributed_elsewhere`, `unknown_owner`, `inventory_unknown` (checks whose own-session inventory came back unknown, for example because the run directory changed during the read; each keeps the checkout), `elapsed_ms`. Phase telemetry adds the timed phase `auto_cleanup_gate_check` and the counters `auto_cleanup_gate_own_session`, `auto_cleanup_gate_path`, `auto_cleanup_gate_elsewhere`, `auto_cleanup_gate_unknown_owner` and `auto_cleanup_gate_inventory_unknown`.
- Gate cache GC (`gc` cache inventory and removal) keeps the repository-wide check: a managed cache is shared by every checkout's gates.

Consumers must tolerate the new pidfile field, holder-line fields and report fields. A consumer that parsed the holder line as exactly two tokens must take the first token and `since=` instead. Covered by `gates.rs` `a_pidfile_records_its_checkout_and_older_records_name_none`, `an_owner_lock_holder_line_names_its_checkout_only_while_complete_and_current`, `gate_witnesses_name_the_checkout_of_a_held_owner_lock_and_clear_it_on_release` and `gate_witnesses_name_the_session_and_checkout_of_a_live_pidfile`, `a_partial_owner_lock_acquisition_clears_the_lines_it_wrote` and `the_session_inventory_reads_every_session_once_and_matches_the_single_session_read`, `auto_cleanup.rs` `the_own_session_inventory_decides_when_no_witness_does`, `a_held_owner_lock_keeps_only_the_checkout_its_line_names`, `a_recorded_checkout_that_cannot_be_resolved_is_owner_unknown`, `a_gate_keeps_a_checkout_only_when_it_could_be_using_it` and `the_gate_check_reads_once_per_snapshot_and_keeps_on_unknown_owners`, and `tests/auto_cleanup.rs` `a_gate_recorded_in_another_sessions_checkout_does_not_keep_a_landed_closed_checkout`, `removal_rereads_the_gate_records_after_selection` and their neighbours.
### Judging closed checkouts without wasted disk work

Introduced 2026-11-15. On the busiest repository background checkout removal judged about 17 closed checkouts per 60-second steady slice (about 2.1 s each), one slice per worker run, and removed nothing: groups the database already disqualified went through Git first, stable keeps were judged again every pass, the priority lane could take a whole pass, and a pass that only judged was followed by a GC lock gap as long as itself. Changes (no removal proof, keep code or safety gate changed; apply re-proves every selected checkout under its locks as before):

- Database keeps. Before any Git process or worktree walk, selection keeps a group an open session uses (`session_open`, or the overlap-unknown reason) or one with a session inside `closed_worktree_grace_hours` (`in_grace`), with the reasons the full judgment gives. They cost none of a pass's judged shares (8 priority groups, 16 steady lap groups). Priority candidates kept this way are left out of the priority lane (the lap reports them; the cleanup request becomes due when the grace ends), and the priority query orders candidates past their grace first.
- Keep memo (`auto_cleanup.keep_memo`). Entries gain an optional `code` (absent: `ignored_content`, as before) and, with code `not_contained`, `upstream`. Besides ignored leftovers the memo now remembers `uncommitted_changes` and `unmarked_tracked_deletions` (keyed on HEAD read from the checkout's `HEAD` file and branch ref, the index's modification time and size, the checkout root, its `.gitignore`, the common `info/exclude`, the first changed path its reason names, that path's directory and the directory's `.gitignore`; an unmarked-deletions entry is never used while any removal attempt is recorded for the path) and `not_contained` (HEAD and the remote default branch tip). A keep is remembered only when those files read the same before and after its judgment and the changed path was not modified within two seconds of it. Entries are re-proved at least weekly and whenever the sessions or the cleanup policy change. A hit is checked before the group inspection and only ever keeps; it counts in `removal_examined` and `removal_memo_skips` as before.
- Selection runs one deep verdict (ancestry, then the landing search while time remains) instead of a shallow verdict followed by the whole verdict again, and reuses the pass's open-session path index; apply still reads open sessions afresh.
- Full judgments alternate between the priority lane and the lap, priority first, so the lap gets every other one in every pass. The lap leaves the groups queued for the priority lane in that pass to it, as before.
- The GC lock gap after a pass in which Git deleted nothing is the fixed 500 ms (a waiting command announces itself and the slice yields to it before each pass); after a pass that deleted it stays at least as long as the pass held the lock. The record's `lock_yield_ms` keeps its meaning.
- A worker run takes steady slices back to back while each stops for `budget`, `cap` or `kept_only_limit` with passes run, no pressure block, the lap unfinished and the inventory available, and at least 10 s of the run's capacity allowance remains (500 ms of free GC lock between slices). Such a slice does not wait out the one-minute steady retry the previous slice set; a hard retry still holds it. The `pressure` trigger is unchanged.
- `auto_cleanup.lanes` gains `removal_db_in_grace`, `removal_db_session_open`, `removal_priority_db_skips` (omitted when 0), `removal_memo_hits` (`ignored_content`, `uncommitted_changes`, `unmarked_tracked_deletions`, `not_contained`, each omitted when 0) and `removal_check_timings` (`database`, `memo`, `inspect`, `upstream`, `sessions`, `cohort`, `ownership`, `live_paths`, `leases`, `gates`, `status`, `ignored`, `containment`, `blocker_listing`, each `{count, ms}` and omitted when no check of that kind ran; selection and revalidation summed).
- The `auto_cleanup.capacity_removal` record gains `empty_laps` (the stored ladder value, `gc.artifact_sweep.steady_removal_idle_streak`), `db_in_grace`, `db_session_open`, `priority_db_skips`, `memo_hits`, `check_timings`, `groups_judged_per_min` (examined plus database keeps per minute of `selection_ms`) and `continued`. `idle_streak` now reports the streak that set the slice's retry: 0 for a steady slice that left the lap unfinished and moving (its retry is a minute whatever the stored streak), otherwise `empty_laps`.
- Phase telemetry adds the timed phases `auto_cleanup_check_<kind>` (the kinds above) and `artifact_worker_host_slot_held` (one per worker run that took the host deleter slot; its Git deleting time stays `artifact_worker_capacity_removal_git_removal`), and the counters `auto_cleanup_db_kept_in_grace`, `auto_cleanup_db_kept_session_open`, `auto_cleanup_priority_db_skip`, `auto_cleanup_removal_db_kept`, `auto_cleanup_memo_hit_<code>` and `artifact_worker_capacity_removal_continued`. Slices per run remain the `artifact_worker_capacity_removal_slice` count.

The host-wide single-deleter slot was unchanged by this change (a worker run held it from before its first slice to its exit, judging included); see the next section for how it is held now. Memory pressure stays a hard stop and load a throttle. Consumers must tolerate the new fields and counters, and an `idle_streak` of 0 while `empty_laps` is not. Covered by `auto_cleanup.rs` `the_database_alone_keeps_groups_in_grace_or_in_use`, `groups_the_database_keeps_cost_no_judgment_or_priority_share`, `priority_candidates_past_their_grace_come_first`, `the_lap_judges_in_every_pass_however_many_priority_groups_wait`, `an_uncommitted_keep_is_remembered_until_the_checkout_changes`, `a_new_commit_or_a_newly_landed_tip_ends_a_remembered_keep`, `a_remembered_keep_never_selects_a_checkout`, `apply_still_refuses_a_checkout_changed_after_selection` and `a_worker_run_keeps_taking_steady_slices_while_the_lap_is_unfinished`, and `gc.rs` `the_gc_lock_gap_is_long_only_after_a_pass_that_deleted` and `only_an_unfinished_unblocked_steady_slice_continues`.

### The host deleter slot is held only while deleting

Introduced 2026-11-15. A worker run waited up to 15 minutes for the host-wide single-deleter slot (`<host state dir>/run/gc-unattended-deleter.lock`) and then held it from before its first slice to its exit. Since a run that only judges lasts up to five minutes, with several busy repositories on one machine a worker could wait out its 15 minutes and do nothing (the busiest repository measured waits of 84, 104 and 110 s and one of 900 s). Changes:

- The unattended pacer takes the slot only around a deletion, waiting at most 2 s (polling every 100 ms): a whole-checkout removal takes it right before Git deletes, with every proof re-read, and gives it back after the removal's pacer charge; a tree removal (cache lane, artifact reclaim) takes it at its first unlink, keeps it batch after batch, hands it over after 20 s (leaving it free for 250 ms) and gives it back when it returns, after paying its partial batch. No unattended unlink happens without the slot: an unlink about to happen with no slot held starts a fresh batch through the full gate (pressure check, then the slot), whatever the batch counter says. A charge that has held the slot for 20 s hands it over the same way and pays the rest without it if another worker takes it (the deletion it pays for is over). A gate whose pacer is already borrowed refuses instead of proceeding. Judging, counting, lock gaps and memory or load waits never hold it: an unpinned slot is given back before the pacer sleeps for pressure. Lanes that delete through paths with no per-deletion gate (session retirement, compact checkout archives, the requested checkout cleanup) pin it for their length, waiting at most 10 s, and keep it through pressure waits as before.
- Memory pressure is still checked immediately before each deletion: the pressure check runs first, and when taking the slot meant waiting (or for a whole-checkout removal, always, with or without a host directory) memory pressure is sampled again once the slot is held. Elevated, the slot goes back; a tree removal then waits the pressure out without it, and a whole-checkout removal (which holds its GC lock) is put off and its slice stops `memory_backoff`. Load stays a throttle and the unlink rate limit is unchanged; the limiter's sleep is paid while the slot is held, so the machine's one deleter stays at the configured rate.
- When another repository's worker keeps the slot through the wait, the deletion is put off resumably: a tree removal stops as at a deadline, and the rest of that cache pass refuses at once instead of waiting again for each directory (the pass ends; the next one, after the worker's short gap, tries again); a whole-checkout removal is counted as deferred and the next pass judges its group again (its priority memo entry is dropped, and the lap cursor is moved back before it), and the slice stops with the new `stop_reason` `host_deleter_busy` (outcome `lock_busy`, a one-minute retry, hard for steady removal, idle streak unchanged). A pinned lane that cannot get the slot is deferred for one minute with its request durable (worker status `session_retirement_host_deleter_busy`, `compact_checkout_host_deleter_busy` or `checkout_cleanup_host_deleter_busy`). The worker status `host_deleter_busy waited_ms=<n>; request retained` is no longer written.
- Lock order: judging permit, then the repository's GC lock (and lifecycle claim), then the deleter slot; every wait for a permit or the slot is bounded. While a worker holds the slot it waits for nothing but its own repository's locks (a pinned lane takes its GC lock and lifecycle claim inside its pin); it never waits for a permit, the slot or another repository's locks. Foreground commands never take the slot or a permit, and each repository has one worker, so the only process that can wait for a holder's repository locks holds no slot or permit: no wait cycle can form, and two workers can never delete at once. The slot and the permits are `flock` locks: a crashed holder releases them when its process exits.
- Judging is bounded across repositories by a separate host-wide semaphore of two permits, `<host state dir>/run/gc-unattended-judge-0.lock` and `-1.lock`. A capacity removal slice takes one before it counts and judges (waiting at most 5 s) and holds it to the slice's end; with both taken it ends at once with a one-minute retry (worker status `capacity_removal judge_permit_busy waited_ms=<n>; retry in 60000 ms`) and records no slice.
- Post-close cleanup wake: `finish`, `close` and abandoned-session closes try the GC lock without waiting; a held lock (usually the worker's) is now the expected case and prints no `wake post-close checkout cleanup` warning. The request is already in the close outbox. The running worker reconciles the outbox before its requested-cleanup lane, so a close made before that point is served in the same run; a close made after the worker passed that lane stays in the outbox until the next broker open or worker start reconciles it (every broker open does, and a due request then starts a worker). `finish --json` still reports `reclaim_deferred` and `reclaim_worker_pid`.
- `cleanup_retention.last_artifact_sweep.unattended_pacing` gains `host_slot_held_ms`, `host_slot_deleting_ms` (held time not spent in the limiter's sleep), `host_slot_acquisitions`, `host_slot_busy`, `judge_permit_wait_ms` and `judge_permit_busy`; `host_lock_wait_ms` is now the sum of the bounded waits. The `auto_cleanup.capacity_removal` record gains `host_slot_wait_ms`, `host_slot_held_ms`, `host_slot_deleting_ms`, `host_slot_busy` (per repository, so a worker starved of the slot shows) and `judge_permit_wait_ms` for the slice. `host_slot_busy` counts bounded waits that ended with the slot still taken. `run_elapsed_ms` now counts from the run's start rather than from taking the slot.
- Phase telemetry: `artifact_worker_host_slot_held` is now the pacer's held time (it was the run's whole length); new timed phases `artifact_worker_host_slot_deleting`, `artifact_worker_judge_permit_wait`, `artifact_worker_capacity_removal_host_slot_wait`, `_host_slot_held` and `_judge_permit_wait`, and counters `artifact_worker_host_slot_busy`, `artifact_worker_judge_permit_busy`, `artifact_worker_host_slot_pin_busy`, `artifact_worker_capacity_removal_stop_host_deleter_busy`, `artifact_worker_capacity_removal_judge_permit_busy` and `post_close_cleanup_wake_deferred`.
- A memo hit on a lap group still queued for the priority lane now moves the lap cursor, as a full judgment does, so a lap whose last group is such a hit ends in that pass.
- Process-group absence probes (`gates.rs`) keep polling until their deadline on `EPERM` (a group whose last member is exiting) and `EINTR`; only `ESRCH` proves a group gone, as before.

Consumers must tolerate the new fields, stop reason, worker statuses, lock files and counters. Covered by `gc/unattended_pacing.rs` `the_deleter_slot_is_held_only_while_a_tree_is_removed`, `a_busy_slot_puts_a_deletion_off_after_the_bounded_wait`, `pressure_waits_never_hold_the_slot`, `memory_is_read_again_once_a_waited_for_slot_is_held`, `the_checkout_gate_refuses_under_memory_pressure_without_waiting`, `a_pinned_lane_keeps_the_slot_until_the_pin_drops`, `a_long_tree_removal_hands_the_slot_over`, `a_charge_keeps_the_slot_its_removal_took_and_takes_none` `at_most_two_workers_judge_at_once`, `every_tree_removal_takes_the_slot_at_its_first_unlink`, `many_small_tree_removals_each_hold_the_slot`, `a_tree_removal_after_a_checkout_charge_takes_the_slot`, `a_trees_witness_and_root_wait_for_the_slot`, `a_borrowed_pacer_refuses_deletion`, `the_checkout_gate_reads_memory_pressure_without_a_host_directory` and `a_long_charge_hands_the_slot_over`; `gc.rs` `host_deleter_lock_serialises_workers_across_repositories`, `a_lane_that_cannot_pin_the_host_deleter_slot_leaves_its_request_durable` and `a_close_that_finds_the_gc_lock_held_leaves_its_request_for_the_holder`; `auto_cleanup.rs` `a_removal_waits_only_briefly_for_the_host_deleter_slot`, `a_memo_hit_on_a_queued_priority_group_moves_the_lap_on`, `a_lap_group_put_off_by_a_busy_slot_is_judged_first_again`, `a_put_off_group_moves_the_lap_cursor_back_before_it`, `a_hard_retry_set_mid_run_stops_continuation_slices`, `apply_refuses_a_checkout_an_open_session_started_on_after_selection`, `an_unmarked_deletion_keep_is_remembered_until_a_removal_attempt_is_recorded` and `no_keep_is_remembered_when_the_checkout_changes_during_its_judgment`; and `gates.rs` `eperm_keeps_polling_and_never_proves_absence`.


### Merged-pull-request sessions close after two hours; every automatic close records its reason

Introduced 2026-11-15. `[retention] session_merged_after_hours` (default `2`, `0` disables, at most `8760`; `session_abandoned_after_hours = 0` disables it as well) gives the landed lane a shorter window for a spawned session whose every recorded pull request merged (`pull_request_session_links` joined to `pull_request_milestones.merged_at`; at least one link, none with `merged_at` null). The window is counted from the later of the session's last activity (database and worktree) and its last merge. Every proof of `try_close_landed_session` still applies; the lane additionally runs the gate, coordinated-operation, preparation and artifact-use checks for a holder without a task lease, refuses a latest merge-queue entry in `submitted`, `simulating` or `verified`, reads the pull requests again under the claims (a change is `changed`), and re-runs the uncommitted-content classifier after the final HEAD and activity look (`moved_before_close`). An adopted session is declined as `adopted` and keeps the 24-hour path. The `retire_likely_landed_sessions` queue offers these sessions; nothing else changes order.

Stored records:

- The `session.abandoned` payload gains `close_reason` (`merged_pull_request_landed`, `landed`, `inactive_no_pull_request`, `inactive`), `head` and `checkout_deleted` (always `false`). Landed closes add `upstream_tip`, `landing_evidence` (`representation`, `ancestry`, `content`) and `pull_requests` (`[{repository, number, merged_at}]`); abandonment closes add `pull_requests` and `unlanded_commits` (`true`, `false` or `null` when it could not be established). The earlier fields (`idle_ms`, `abandon_after_ms`, `pid_alive`, `retirement_basis`) are unchanged.
- Every automatic close writes one `automation_notes` row from `automation:session-auto-close` with fingerprint `session-auto-close` (and its `automation.note.sent` event) to the closed session.
- `AutoCleanupReport` gains `merged_pull_request_sessions_examined` and `merged_pull_request_sessions_closed` (omitted when zero; counted within the landed counts), and `landed_session_retirement` in `broker status --json` gains `merged_pull_request_examined` and `merged_pull_request_closed`. `landed_sessions_declined` may carry `adopted`. Phase telemetry adds the counters `landed_merged_pr_examined`, `landed_merged_pr_closed` and `landed_sessions_declined_adopted`.

A session command refused because its session is closed now names an automatic close and suggests `aethyme broker start --reuse` when the latest `session.abandoned` event exists. Consumers must tolerate the new payload fields, the new note sender and the new report fields. Covered by `session_auto_close.rs` tests.

### Session retirement no longer starves behind landing proofs

Introduced 2026-11-15. In the oldest-first retirement lap, a session already past `session_abandoned_after_hours` is now asked the abandonment question first (no Git); the landed proof runs for it only when the abandonment close found a live holder (which only the landed lane may outlive). In the likely-landed queue, sessions with a merged pull request are ordered first, the branch listing is bounded by the lane deadline, and the merged-pull-request lane checks the holder before the landing proof. A head the landing search proved landed is remembered (meta key `landed_lane.landed.<session id>`, value `"<head> <default-branch tip> <ancestry|content> <pull-request fingerprint>"`, the fingerprint a digest of every recorded pull request and its merge time) and reused for judging only while the head and fingerprint are unchanged and that tip is an ancestor of the current default-branch tip; a close that rests on it searches again, without the memo, under the session and lifecycle claims (span `landed_reproof_under_claims`). Checkout removal proves containment itself. The decline memo reads a checkout's HEAD from its files and one `for-each-ref` listing instead of two Git processes per session. A holder whose process table could not be taken because the pass had too little time left is declined as `budget_exhausted` instead of `holder_unknown`; a failed process-table snapshot is retried once per pass; an open-file observation older than its two-second bound is taken again while at least three seconds of the pass remain, instead of answering `operation_unknown`. The background retirement worker continues with another pass when a pass ran out of time after judging or closing sessions (bounded by its pass count and time limit). `record_auto_cleanup_report` carries the retirement counts of the previous report into a report whose retirement lane judged nothing, so `landed_session_retirement` (top level of full `broker status --json`, omitted until a lane judged or closed a session) is no longer erased by a zero-budget pass. New report field `sessions_closed_by_reason` (`retirement_evaluated_at_ms` when carried), view field `closed_by_reason`; new phase spans `landed_proof`, `landed_holder_check`, `session_close_cas`, `retirement_abandonment_first`, `landed_likely_branch_listing`, `landed_memo_branch_tips`; new counters `landed_landing_memo_hit`, `landed_memo_head_from_files`, `landed_merged_pr_reached_proof`, `retirement_abandonment_first_closed`, `retirement_abandonment_first_kept`, `sessions_closed_merged_pull_request_landed`, `sessions_closed_landed`, `sessions_closed_abandoned`, `holder_process_table_retried`, `retirement_process_use_resnapshot`, `session_retirement_continued_after_deadline`. Covered by `session_auto_close.rs` tests.

### Automatic-close review follow-ups

Introduced 2026-11-15. The decline memo key (`auto_cleanup.landed_decline_memo`) gains `holder=<latest holder event id|none>`, and `holder_unbound` is remembered under it (a new binding changes the key); `holder_alive_current` (then named `holder_alive`) is never remembered, since a process can exit without any event. An open adopted session on the path and branch of an earlier spawned session (a follow-up started with `start --reuse` after an automatic close) is eligible for the merged-pull-request window like a spawned session. A follow-up task protects its session for `session_landed_after_hours` whatever the holder evidence: from its latest `session.reused` event, or from its creation for such an adopted follow-up (declined as `recent_activity`). A spawned merged session the merged lane declines as `operation_unknown` falls back to the landed lane once past `session_landed_after_hours` (counter `landed_merged_pr_fallback_to_landed`). The merged lane runs the open-file check for every holder binding, legacy ones included. The closed-session refusal says whether the checkout is still on disk, and if so to run `aethyme broker start --reuse` from inside it; a session command run from a checkout without `--session` whose latest session the broker closed automatically prints `note: this checkout's session <id> is closed: ...` on stderr and goes on. The automation note text follows the same wording.

### Automatic-close delta-review fixes

Introduced 2026-11-15. The final open-file look before an automatic close is checked for processes in the checkout like the first look (it used to check only that an observation existed, so a re-taken observation was never read). Open-file observations are throttled per retirement pass: at most one per 10 seconds and four per pass, and only while three seconds of the pass remain; a landed-lane session that cannot get a fresh one is `budget_exhausted` and retried next pass (counters `retirement_process_use_snapshot`, `retirement_process_use_resnapshot` with their time, `retirement_process_use_throttled`). A remembered landing is searched again without the memo before the session and lifecycle claims are taken (span `landed_reproof_before_claims`); the claims re-check only HEAD, the default-branch tip and checkout cleanliness. Checkout HEAD files are read without following symlinks or blocking (`O_NOFOLLOW|O_NONBLOCK`, regular files of at most 4 KiB), falling back to Git. A follow-up task is protected for the longer of `session_landed_after_hours` and `session_abandoned_after_hours`, and the abandonment window also respects that protection when it is the shorter one. The background worker gives session retirement up to 120 seconds per run (in passes of at most 30 seconds, yielding to a waiting foreground GC command and stopping on memory pressure between passes, counter `session_retirement_memory_backoff`), and continues after a pass that judged, examined or closed sessions. `AutoCleanupReport` gains `abandonment_sessions_examined` and `landed_session_retirement` gains `abandonment_examined` (both omitted when zero); a pass that only asked the abandonment window counts as having judged, so its counts and time replace the previous ones.

### Host deleter slot follow-up

Introduced 2026-11-15, after the slot became held only while deleting. Changes:

- `cleanup_retention.last_artifact_sweep.unattended_pacing` gains two counters: `host_slot_unavailable` (times the host directory stopped holding the slot and deletion went on unserialised) and `host_slot_recovered` (times a later take succeeded again). Both are 0 in older summaries. An open error on the host directory no longer turns serialisation off for the rest of the worker run. The next tree removal (each one starts on a fresh batch when no slot is held) or batch tries the slot again.
- The slot taken for a tree's witness or root unlink now reads memory pressure first. If pressure is elevated, the slot goes back and the tree waits for a later pass.
- A refusal from the checkout deletion gate when the pacer is already borrowed is now kept for the slice, which stops on it (`host_deleter_busy`).
- Preparation-cache rotation-debt recovery unlinks single control files outside the per-unlink gate, in a crash-safe sequence. The unattended worker therefore pins the slot for the whole recovery step, waiting at most 2 s. When the slot is busy, the step is left unfinished for the next pass, counted as phase `preparation_rotation_recovery_host_deleter_busy`, never as a recovery error. Foreground callers are not serialised, as before.
- When a put-off group's lap cursor is moved back, the cleanup report is recorded first. A cursor that another pass moved meanwhile is left as it is and counted (phase `auto_cleanup_put_off_cursor_conflict`) instead of failing the pass.
- The stale Git probe-directory sweep at the start of a worker run uses the pinned tree removal, so it takes the slot and its pressure check only while deleting. The fresh Git verdict check runs when that removal opens the tree, before any pacer gate, so it is never reached while the pacer is borrowed. A busy slot during the sweep stops it from waiting again for each remaining directory.

Covered by `gc/unattended_pacing.rs` `every_tree_removal_takes_the_slot_at_its_first_unlink` (now at a limited rate, so the charge leaves the batch counter mid-batch), `a_tree_removal_after_load_released_a_charges_slot_takes_it_again`, `a_transient_host_directory_failure_ends_when_a_take_succeeds`, `a_root_unlinks_slot_reads_memory_pressure_first`, `a_borrowed_pacer_refuses_deletion` and `rotation_debt_recovery_waits_for_the_slot_without_failing`, and by `auto_cleanup.rs` `a_cursor_conflict_while_moving_back_is_not_an_error`.

### Abandonment keep reasons; a budget-exhausted session holds the cursor one pass

Introduced 2026-11-16. Every exit of the abandonment window other than a close is counted per reason: phase counter `session_abandonment_kept_<reason>` for every caller of the window, and in the retirement lap `AutoCleanupReport.abandonment_sessions_closed` and `abandonment_sessions_kept` (per reason code), shown in `broker status --json` as `landed_session_retirement.abandonment` (`examined`, `closed`, `kept`; omitted until the window judged a session). Reason codes: `window_disabled`, `changed`, `session_pid_alive`, `follow_up_protected`, `worktree_activity_unavailable`, `recent_activity`, `holder_unknown`, `holder_alive_current`, `holder_record_unavailable`, `holder_shared_service`, `holder_moved_on_dirty`, `task_lease_current`, `operation_guard_busy`, `operation_guard_unavailable`, `lifecycle_busy`, `deadline_reached`, `gate_running`, `gates_unknown`, `operation_unresolved`, `operations_unknown`, `merge_queue_active`, `merge_queue_unknown`, `preparation_in_progress`, `preparation_invalid`, `preparation_unknown`, `checkout_not_directory`, `artifact_producer_active`, `artifact_protocol_unavailable`, `legacy_artifact_markers`, `open_files_in_checkout`, `open_files_unavailable`, `open_files_throttled`, `lease_held`, `lease_unavailable`, `database_busy`, `policy_unavailable`, `landed_declined`, `unrecorded`. `ActiveProcess` is split into `session_pid_alive`, `holder_alive_current`, `holder_moved_on_dirty`, `holder_record_unavailable`, `holder_shared_service`, `task_lease_current` and `open_files_in_checkout`; `operation_unknown` into `operation_guard_unavailable`, `deadline_reached`, `gates_unknown`, `operations_unknown`, `merge_queue_unknown`, `preparation_invalid`, `preparation_unknown`, `checkout_not_directory`, `artifact_protocol_unavailable`, `open_files_unavailable` and `open_files_throttled`. `AutoCleanupReport.retirement_cursor_start` (view `cursor_start`) records where the cursor stood when the report's first pass began, and each pass adds `retirement_cursor_advanced` or `retirement_cursor_unmoved`.

A session whose landed proof runs out of budget leaves the retirement cursor on itself for one pass only (meta key `auto_cleanup.session_retirement_budget_hold`, counters `retirement_cursor_budget_held` and `retirement_cursor_budget_hold_released`): the next pass judges it once more and then moves on whatever the answer. The earlier rule released the hold only when that one session used ten seconds of a pass, which a pass whose likely-landed queue had spent most of its budget never gave it, so one session pinned the lap and every older session behind it stayed open pass after pass. A failed process-table snapshot is retried at once and then after five seconds, at most three times per pass (it was once per pass), with counter `holder_process_table_failed` per failure.

### A live holder that moved on; finishing closed sessions with a missing checkout

Introduced 2026-11-16. The landed decline code `holder_alive` is renamed `holder_alive_current` (counter `landed_sessions_declined_holder_alive_current`), and `holder_alive_in_use` is added. A live holder whose process (pid and recorded start) has a `session.holder_bound` event on a different session after this session's last activity has moved on: the merged-pull-request and landed lanes close the session when every other proof holds and the open-file observation (no holder exemption), gates, operations, preparation, artifact producers, merge queue and explicit leases show no use; the abandonment window does so only for a clean checkout. The `session.abandoned` payload gains `holder_evidence` (`gone`, `moved_on`, `lease_expired`, `unbound`) and `holder_moved_on_to_session`. `AutoCleanupReport` gains `sessions_closed_by_holder_evidence`, `missing_checkout_examined`, `missing_checkout_finished`, `missing_checkout_kept`; `landed_session_retirement` gains `closed_by_holder_evidence`. New counters: `holder_moved_on_candidates`, `holder_moved_on_dirty_kept`, `session_closed_holder_<evidence>`, `landed_worktree_unclean_<kind>` (`uncommitted_changes`, `unmarked_tracked_deletions`, `strict_tracked_deletions`, `ignored_content`, `ignored_content_unverified`, `operation_in_progress`, `stash`, `other`), `missing_checkout_examined`, `missing_checkout_finished`, span `missing_checkout_finish`. The retirement open-file observation is valid for ten seconds (it was two). New event `session.checkout_missing_finished` with `worktree_path`, `pool_root`, `volume_evidence` (`volume_uuid_matches` or `pool_root_marker`) and `deleted: false`; meta key `missing_checkout.after` holds the lane's cursor.

### Memory warning throttles the worker; retirement gets its share

Introduced 2026-11-16. The unattended pacer reads two memory levels. macOS
`kern.memorystatus_vm_pressure_level` 4 (and anything above 2), or Linux
memory PSI at its limit, stays the hard stop. Level 2, a level of 0 or below,
or a failed read is the warning level: one bounded wait shared with load
(1 second per deletion batch, 5 seconds before a removal), then the work
proceeds (`PressureVerdict::MemoryWarningThrottled`). `UnattendedPacingSummary`
gains `memory_warning_pause_ms` and `memory_warning_throttled` (omitted when
zero); the run's metrics row gains `artifact_worker_pacer_memory_warning_wait`
and `artifact_worker_pacer_memory_warning_throttled`. The checkout removal
slice gains the stop `memory_warning_throttled` (one removal, then at least
120 seconds, a hard steady retry) and the phases
`artifact_worker_capacity_removal_memory_warning_wait` and
`artifact_worker_capacity_removal_memory_warning_throttled`; its
`pressure_seen` reads `memory` for a warning.

The worker queues its own session-retirement request in every run that
starts for any lane (`artifact_worker_retirement_requested`), runs the lane
without pinning the host deleter slot, gives it at least the 30-second
reserve (`artifact_worker_retirement_share_budget`, `_used`,
`_reserve_used`; `artifact_worker_retirement_not_due` when none was due),
and a pass waits up to 5 seconds for the GC lock before deferring for one
minute (`session_retirement_gc_lock_wait`, `session_retirement_gc_lock_busy`).
Retirement passes remember when a session kept for `recent_activity` can be
due again, at most one window ahead (meta key
`auto_cleanup.session_retirement_not_due`, counters
`retirement_not_due_memo_recorded`, `retirement_not_due_memo_skips`). The
agents view behind `status` and `agents` reads that memo without writing it
and judges only sessions past the window by their row
(`agents_view_abandonment_not_due` for the rest). A retirement pass's GC-lock
wait yields to a foreground command that announced it waits
(`session_retirement_gc_lock_yielded_to_foreground`).

Bounded captures (`output_limited`, used by the process-table snapshot; its
deadline starts after the spawn), the environment supervisor launch, the
detached worker launch, the agent supervisor's agent and cleanup-watcher
spawns and the gated agent spawn run under one process-wide spawn lock
(`process_spawn_lock_wait` when one waited): a supervisor launched from a
report command's background thread could inherit the `ps` pipe and hold it
for minutes, so `status` and `agents` recorded `holder_process_table_failed`.
A detached worker that finds another worker holding the repository's worker
lock now exits successfully (`artifact_worker_already_active`), and takes the
lock before the fresh Git probe rather than after it.

### Sessions no dead owner keeps open; no-work sessions close after four hours

Introduced 2026-11-16. Changes consumers may see:

- `[retention] session_no_work_abandoned_after_hours` (default `4`, `0` disables, at most `8760`, never longer than `session_abandoned_after_hours`; `session_abandoned_after_hours = 0` disables it too). It appears in the retention policy of `gc plan --json`. A spawned session (adopted ones keep the full window) whose HEAD is symbolically its own branch, whose branch is still its `diff_base`, whose checkout is clean, whose recorded holder is gone past the lease stale grace and which holds no explicit claim, unresolved operation, preparation in progress, merge-queue entry or unmerged pull request closes after that window. Its `session.abandoned` payload carries `close_reason` `inactive_no_work`.
- Schema v63 (compatible, floor stays 60): nullable `guarded_exec_receipts.boot_id`, the boot identifier (`kern.bootsessionuuid` on macOS, `/proc/sys/kernel/random/boot_id` on Linux) of the boot a receipt was written in. A `starting`/`running` receipt is dead when its boot id differs from the current one; a receipt without one (an older writer's) only when its last write predates the boot time by 10 minutes. Dead receipts stop counting as unresolved; they are settled only once their session closes.
- New event kind `guarded_exec.receipt_abandoned` (payload `receipt_id`, `previous_phase`, `receipt_updated_at_ms`, `receipt_boot_id`, `boot_id`, `boot_time_ms`, `reason` = `boot_id_changed` or `written_before_boot`). The receipt it names moves to phase `completed` with `command_outcome`, `ownership_audit` and `process_group_cleanup` all `unknown`, a combination no live command writes. If its command still reports an outcome, the row is kept and a `guarded_exec.receipt_late_outcome` event (`receipt_id`, `command_outcome`, `exit_code`, `process_group_cleanup`) records it.
- `broker submit prepare status --json` may report `state` `interrupted`: the record still says in progress but its host lease, at the recorded generation, is released and the lease's recorded holder process is gone. It may be retried like `failed`.
- A session created before holder recording shipped (before 2026-10-05) with no holder event of any kind stops holding explicit claims past the abandonment window when nothing is unresolved and no process uses its checkout; any other session without a holder record keeps them.
- New abandonment keep reasons `holder_process_table_denied` and `no_work_*` (`adopted`, `session_commits`, `worktree_dirty`, `holder_unbound`, `holder_alive`, `holder_gone_recently`, `explicit_claim`, `merge_queue`, `pull_request_open`, `pull_request_unknown`).
- New phase counters: `guarded_exec_receipts_abandoned_pre_boot`, `holder_grace_anchored_to_boot`, `legacy_unbound_claims_released`, `preparation_judged_interrupted`, `holder_process_table_native`, `holder_process_table_native_fallback`, `holder_process_table_native_denied`, `holder_process_table_denied`, `session_closed_no_work`, `session_no_work_kept_<reason>`.

Covered by `schema.rs`, `retention.rs`, `session_auto_close.rs`, `store/guarded_exec.rs`, `preparation.rs`, `lease_liveness.rs` and `session_holder.rs` tests.

### One slow landed proof no longer holds the retirement share

Introduced 2026-11-16. Changes consumers may see:

- The cleanliness listings of the landed proof and of every automatic close (`git status`, `rev-parse --absolute-git-dir`, `stash list`, `ls-files --stage`) stop at the pass's Git deadline and are timed as `subprocess_git`; a stopped listing keeps the session. Without a deadline they stop after ten minutes. `git status` there runs with `core.fsmonitor=false`.
- In a budgeted pass one session's landed judgement (the proof, a memoized re-proof before the claims and the re-checks under them) shares one session deadline: the earlier of the pass deadline and the session's budget, 10 seconds to start with. Past it, while the pass still has time, the session is kept with the new landed decline code `proof_timed_out` (counter `landed_sessions_declined_proof_timed_out`) and the likely-landed queue and the retirement lap go on to the next session; `budget_exhausted` still means the pass ran out and stops the lane. `proof_timed_out` never falls through to the abandonment window and is not remembered: a later lap asks again (see below for how its budget grows).
- New phase spans `landed_proof_checkout_read`, `landed_proof_worktree_clean`, `proof_uncommitted_check`, `proof_ignored_check`; new counters `landed_proof_worktree_clean_timed_out`, `proof_git_deadline_before_start`, `proof_git_stopped_at_deadline` (with the budget it had), `proof_git_output_limit`, `holder_process_table_native_denied_retried` (a native process listing refused after its size query was answered is fetched once more before it counts as denied) and `holder_process_table_native_denied_ps_complete` (the native listing was refused but `ps` listed the table).

Covered by `auto_cleanup.rs`, `broker/leases.rs`, `session_auto_close.rs` and `session_holder.rs` tests.

### Slow landed proofs get growing budgets and a long grant

Introduced 2026-11-16. Changes consumers may see:

- Each consecutive `proof_timed_out` doubles that session's budget (up to what is left of the pass); the streaks are kept in meta key `landed_lane.proof_timeouts` (JSON object: session id to `[streak, first timed out at, epoch ms]`). Any conclusive answer, a close included, ends the streak; running out of time, contention or unreadable evidence leaves it. Once per pass the likely-landed queue judges the session timing out longest last, bounded only by the pass deadline (counter `landed_proof_long_budget_granted`). Counters `landed_proof_timeout_streak_1`, `_2`, `_3_4`, `_5_plus`, `landed_proof_timeout_streak_ended`, and `landed_proof_session_budget_doubled` (with the budget given).
- The landed proof's cleanliness check is one `git status --porcelain=v1 -z --untracked-files=all --ignored=matching` pass instead of a status listing and a separate `ls-files --ignored` listing; its ignored entries go through the same classifier, root check, 15-second budget and 16,384-entry limit. A listing that only needs to know whether anything is uncommitted stops at the first change. Its keep reason is then `at least 1 uncommitted or untracked path(s), first: <path>`. Automatic removal still reads the whole list.
- Proof listings run in a process group of their own that is stopped as a whole, use the resolved Git with inherited repository-routing variables removed, and without a deadline stop after `AETHYME_GIT_TIMEOUT_SECS` (ten minutes by default). New counter `proof_git_stopped_at_first_record` and span `proof_unclean_check`.
- A listing that failed or was stopped is no longer reported as `moved_before_close`, `worktree_unclean` or `upstream_moved`. It is `proof_timed_out` when the session's budget ran out, `budget_exhausted` when the pass did, and `worktree_unavailable` otherwise. A landing search in which any Git call timed out is inconclusive (counter `landed_landing_search_timed_out`) instead of `not_landed`.

Covered by `auto_cleanup.rs`, `broker/leases.rs` and `session_auto_close.rs` tests.

### Landed-proof grant pacing, streak hygiene and group cleanup

Introduced 2026-11-17. This corrects the guarantee stated in the previous section. Changes consumers may see:

- What is guaranteed: a session that keeps timing out is, on every fourth likely-landed pass that has such a session, judged first, with what is left of the lane's share less a tenth (at most a second). On other passes it is judged last (counter `landed_proof_long_budget_first` when first; meta key `landed_lane.grant_passes`). A session that still runs out of time when judged first is kept, counted (`landed_proof_unprovable`) and listed in meta key `landed_lane.proof_unprovable` (JSON array of session ids). `broker status --json` shows them in advice `cleanup.landed-proof-unprovable` (severity `notice`, session ids only, no checkout path). The evidence names the first 20 sessions, then `<n> more; <total> in total`. A doubled budget is capped at half of what is left of the pass.
- A streak ends only when the landing proof concluded (`not_landed`, `no_landing_evidence`, `no_session_commits`), the cleanliness listing finished (`worktree_unclean`, `moved_before_close`) or the session closed. Cheap declines decided before the proof ran (`recent_activity`, `process_alive`, `changed`, `upstream_moved`, `lease_held`), contention, running out of time and unreadable evidence leave it. At the cap the oldest entry is evicted. Entries of sessions no longer open are pruned once per likely-landed pass (counter `landed_proof_timeout_streaks_pruned`).
- When the leader of an owned process group finishes, its exit is observed without reaping it. The group gets one `SIGKILL` while the leader's zombie still reserves the group id, and only then is the leader reaped. There is no TERM and no grace period; before, every normal completion paid the 75 ms grace. `ESRCH`/`EPERM` from that kill are counted (`owned_group_kill_after_exit_refused`) and never retried. A group still present after the reap is waited for, bounded (`owned_group_members_after_exit`). The leader's exit status is unchanged.
- The streamed early-stop check reads each listing byte once. The merged cleanliness listing also runs with `core.untrackedCache=false`.

Covered by `bounded_output.rs`, `auto_cleanup.rs` and `session_auto_close.rs` tests.

### Closed checkouts are retired at close: archive if needed, rename, delete

Introduced 2026-11-17. Changes consumers may see:

- Retirement, automatic removal and archive-then-remove of a spawned session's checkout inside a broker-owned pool no longer run `git worktree remove` (a plain `finish`/cleanup still does in this slice). The checkout is renamed (`renameat`) into the pool's `.aethyme-checkout-trash/` directory, and its Git registration is dropped; the branch ref is untouched. The trash is created with mode 0700 and a marker file `.aethyme-trash-marker`, and is refused if it is a symlink, foreign-owned, open to others or unmarked. The registration is looked up from the repository side; a worktree locked by anyone but the broker is refused. Anything else (adopted checkouts, paths outside a pool, a refused move) is removed by Git as before.
- Entries are named `<13-digit ms>-<session>-<pid>-<nonce>` and recorded with their device and inode in meta key `checkout_retirement.trash_rows`. One host-wide deleter (flock on `checkout-trash-deleter.lock` in the host state directory) deletes only recorded entries whose identity still matches, oldest first, and removes a trash left holding only its marker. It never follows symlinks or crosses mounts, runs at 2,000 unlinks/s, at a 200/s floor under load, and unpaced below the artifact-sweep headroom. It never waits for memory pressure. Entries failing 3 times go last and are reported. Unrecorded names are left alone (`checkout_trash_unrecognized`).
- New worker lane `checkout_retirement` (meta keys `checkout_retirement.due_at_ms`, `.cursor`, `.progress`, `.archive_too_large`; absent due key means due). It walks closed spawned sessions that still have a checkout by session id, resolving the open sessions' paths once per run. Each checkout is checked against hard preconditions only: broker-owned, no open session using it, no lease, no gate or operation, no process. A checkout with everything in Git is trashed without an archive. Otherwise the verified schema-6 recovery archive is written first; content over 512 MiB keeps the checkout. No grace period applies to the directory. An automatic close requests a run. The lane waits out the worker's post-boot hold like the other heavy lanes.
- New status advice `cleanup.retire-archive-too-large` (session ids only) and `cleanup.retirement-stalled` (trash or kept checkouts not shrinking for 3 runs).
- New phase spans `checkout_trash_move`, `checkout_trash_drain`, `checkout_retirement_run`, `checkout_retirement_judge`, `checkout_retirement_open_paths`. New counters `checkout_trashed`, `checkout_trash_registration_left`, `checkout_trash_declined_unclean`, `checkout_trash_outside_pool`, `checkout_trash_fell_back_to_git`, `checkout_trash_entries_removed`, `checkout_trash_entry_failed`, `checkout_trash_unrecognized`, `checkout_trash_invalid_dir`, `checkout_trash_deleter_busy`, `checkout_trash_unpaced_runs`, `checkout_trash_floor_runs`, `checkout_trash_drain_time`, `checkout_retirement_retired_clean`, `checkout_retirement_retired_archived`, `checkout_retirement_kept_<precondition>`, `checkout_retirement_open_paths_unresolved`.

- Before any checkout is moved into the trash, checked under the caller's claims:
  - a merge, rebase, cherry-pick, revert or bisect in progress, or unmerged index entries, keep it (`checkout_retirement_kept_operation_in_progress`, advice `cleanup.retire-operation-in-progress` by session id);
  - a fresh process-use observation must show nobody in it;
  - commits only its reflog reaches are pinned as `refs/aethyme/retired/<session>/<commit>`.

  Retirement re-checks cleanliness under the lifecycle claim with ignored files included. Trash rows are written before the rename as `pending` (source identity) and are confirmed after it; every row change is a compare-and-set. A refused move inside the lane keeps the checkout (`trash_refused`) instead of running `git worktree remove`. The rename never replaces a destination (`renameat2(RENAME_NOREPLACE)` / `renameatx_np(RENAME_EXCL)`). The deleter runs only under the host-wide lock and never uses a weaker one. Unrecognized trash entries are named in advice `cleanup.trash-unrecognized`.
- Keeps about content or ownership are remembered in meta key `checkout_retirement.keeps`, keyed by HEAD and index modification time, and judged again after 1 h, 6 h, then 24 h, or at once when the key changes (`checkout_retirement_keep_memo_skips`). The old removal and compaction lanes skip any session the retirement lane kept on purpose or trashed.
- An automatic close with at least 15 s of its pass left retires the checkout inline (`checkout_retirement_inline_retired`); the lane remains the fallback.
- No grace period applies to the directory: the cleanup request's `grace_hours` is ignored by retirement. The close note says the checkout is retired and how to continue. `start --reuse <path>` on a retired checkout recreates it from the session branch, or refuses with the recovery archive's README when an archive holds leftovers.

- Move order: environments bound to the checkout are destroyed before anything moves; a failure keeps everything in place, and no pending row is written. After a successful rename nothing moves the entry back. A trash row records its creator's pid and start time. The deleter never deletes a pending (unconfirmed) row's entry: it is confirmed only by the mover, or once the creator is proven gone and the entry still matches its recorded device and inode (`checkout_trash_pending_confirmed`). Before deleting, and every 256 unlinks, the deleter re-checks that the entry is still in the trash under its name (`checkout_trash_entry_moved_away` otherwise). Unrecognized entries are reported by name only.
- Pins under `refs/aethyme/retired/` expire once a delivery target (the default branch or its remote-tracking ref) or another remote-tracking ref holds the commit, or once their session has been cleaned for `recovery_archive_days` (0 keeps them). At most 64 pins are checked per run, from a cursor (meta key `checkout_retirement.pin_cursor`). They are counted in advice `cleanup.retired-pins` and the progress field `retired_pins`. Inline retirement at close takes only the no-archive path (`kept_deferred_to_lane` otherwise). Recreating a retired checkout for `start --reuse` takes the lifecycle claim.
- A pending trash row whose mover's start time was unreadable is named in `cleanup.trash-unrecognized` once a day old (progress field `stale_pending_trash_entries`). An explicit `gc apply` confirms it when the entry still matches its device and inode and no open session uses the session's path. The meta key `checkout_retirement.trashed_sessions` keeps a tombstone per confirmed move until the session is recorded as cleaned, so a lost cleaned record is reconciled on the next run.
- Changed 2026-11-19 (trash drain keeps up). On the busiest repository the trash grew to 736 entries (~497 GiB): 64 entries moved in per 1.3 h while the drain removed 7 at 2,000 unlinks/s (200/s whenever load exceeded the core count), on whatever the retirement run had left. Now:
  - The trash is *behind* when it holds at least 16 confirmed entries (`TRASH_BACKLOG_ENTRIES`; pending rows and entries failing 3 times do not count) or its oldest entry is a day old. Behind, the deleter catches up at 8,000 unlinks/s (`catchup`), 4,000/s on a loaded host (load above the core count, the unattended pacer's load or Linux I/O-pressure signal) or at the memory warning level (`catchup_loaded`), and the 200/s floor at critical memory pressure. Not behind, the pace is unchanged (2,000/s, 200/s under load, memory ignored); below the artifact-sweep headroom it stays unpaced. A paced drain reads the host's pressure again every 2 s and follows it. The rate is a ceiling over any interval: a limiter that falls more than 250 ms behind its rate (a stalled volume) moves its window forward instead of catching up in a burst.
  - Entries the last run passed over as in use (progress field `trash_in_use_entries`, at most 256 names) are left out of the behind decision, so an entry held open for days does not keep the deleter catching up with nothing deletable; they still count in `trash_pending`, `trash_oldest_entry_ms` and the advice.
  - When the repository's configuration sets `[cleanup] unattended_unlinks_per_second` explicitly to a limit, it is the ceiling of every paced mode (an absent key, `0` or `"unlimited"` set none; below the headroom stays unpaced). Counter `checkout_trash_configured_ceiling_applied` when it lowered the starting rate.
  - The recovery-archive bound now runs before the drain with at least 20 s, instead of after it on what the drain left.
  - The drain has its own share of each retirement run: a third of the window, half while behind (the phases before it share the rest, each keeping its floor). When those phases ran past the lane's deadline the drain still gets its whole share from when it starts (counter `checkout_trash_drain_budget_extended`, time = the extension).
  - Order: fewest failures first (capped at 3), then oldest first. An entry in use or that failed is passed over and retried by a later run; it never blocks the entries behind it.
  - New counters `checkout_trash_catchup_runs`, `checkout_trash_catchup_loaded_runs`, `checkout_trash_budget_stops` (the run's deadline stopped it with entries left), `checkout_trash_entry_interrupted`, `checkout_trash_drain_budget_extended`. New metric attachment `checkout_trash_drain`: `backlog_entries` and `backlog_oldest_age_s` (the deletable backlog, without entries last passed over as in use), `need` (`disk_pressure` | `backlog` | `gentle`), `initial_pace` and `final_pace` (`steady` | `floor` | `catchup_loaded` | `catchup` | `unpaced`), `initial_unlinks_per_second` (the starting rate after any configured ceiling; null when unpaced), `configured_ceiling` (null unless set explicitly), `ceiling_applied`, `pace_changes`, `entries_removed`, `unlinks`, `bytes`, `elapsed_ms`, `unlinks_per_second_observed`, `passed_over` (`in_use`, `failed`, `unrecognized`), `budget_stopped`, `process_use_unknown`, `entries_pending`, `oldest_pending_age_s`.
  - New progress fields (meta key `checkout_retirement.progress`) `trash_oldest_entry_ms` (absent when empty) and `trash_growing_runs` (runs in a row after which more entries were recorded than after the run before). New status advice `cleanup.trash-backlog` (Warning) when the trash holds entries and the oldest is a day old or it grew for 3 runs in a row.
- Changed 2026-11-19 (trash drain throughput). Under 0.8.132 on the busiest repository every drain was catching up yet observed 150 to 2,300 unlinks/s against 8,000 allowed, blocked in the kernel on an external USB volume (`fstatat` 53 %, `unlinkat` 39 %), and drained ~112 s of every ~437 s worker run, while ~1,250 files/s arrived. Now:
  - The trash deleter removes each entry with `PinnedTreeRemoval::remove_trusting_listing_types_with_guard`. Entry types come from the directory listing (`d_type`) and a non-directory is unlinked without a prior `fstatat`. `DT_UNKNOWN`, or a plain unlink refused because the entry is a directory (`EISDIR`, `EPERM`), falls back to `fstatat` as before. Directories are still opened `O_NOFOLLOW | O_DIRECTORY` and must match the pinned root's device, filesystem and mount identity, read from the opened descriptor (a non-directory cannot be a mount point). The ancestors' identities are re-checked at every directory open, before every directory removal, and every 256 unlinks (`LISTED_TYPE_BINDING_CHECK_EVERY`), instead of before every unlink; the trash's own "entry still in the trash" check keeps its 256-unlink cadence. Every other caller of the descriptor-relative removal (cache sweeps, checkout removal, preparation-cache rotation, orphan apply) is unchanged.
  - Without a per-file `stat` the deleter cannot sum file lengths: `bytes`, `stripped_bytes` and `scratch_bytes` of a drain (and the strip ledger's bytes freed fed from them) are now the volume's available-space change across each entry's removal (`fstatvfs`), approximate when other writers use the volume and never below zero.
  - New continuing catch-up lane: after its other lanes, with its worker lock already released, the detached worker keeps draining while the trash is behind, in slices of 30 s (`TRASH_CATCHUP_SLICE`) with a 1 s gap, for at most 20 minutes (`TRASH_CATCHUP_MAX`). It is still the single host-wide deleter (each slice takes `checkout-trash-deleter.lock`), still paced and re-reading pressure every 2 s, and a slice stops at once, resumably, at a reading of critical memory pressure (counter `checkout_trash_yielded_to_pressure`; drain report field `yielded_to_pressure`). One lane per host (`checkout-trash-catchup.lock` in the host state directory). Each catch-up slice uses 3 deleter threads (`TRASH_CATCHUP_THREADS`), each over its own entries (dealt round-robin in drain order; pending rows and foreign names stay with the first, which alone confirms pending rows and counts unrecognized names), each pacing itself at a third of the rate, with one open-file observation for all and this process's telemetry recorder; a thread that panics leaves its entries' rows as they were (counter `checkout_trash_thread_panicked`). Spans recorded inside the threads (`checkout_trash_drain`) add up the threads' time. A retirement run's drain stays single-threaded. A retirement run's drain now waits up to 35 s for the deleter lock (polling every 100 ms) so it gets a turn between slices. The lane ends `caught_up`, `cap`, `pressure`, `no_progress` (a slice with time left removed nothing), `deleter_busy` (lock held elsewhere for 2 minutes) or `lane_busy`; counters `checkout_trash_catchup_end_<reason>`, span `checkout_trash_catchup`.
  - New metric attachment `checkout_trash_catchup`: `end_reason`, `slices`, `entries_removed`, `unlinks`, `bytes`, `drain_ms`, `elapsed_ms`, `files_per_second_observed`, `busy_retries`, `worker_drain_share_pct` (this worker process's time in drains, retirement and catch-up, as a share of its wall time). The `checkout_trash_drain` attachment adds `threads` and `end_reason` (`finished` | `deadline` | `pressure` | `process_use_unknown` | `deleter_busy`).
- Changed 2026-11-19 (trash deleter follow-ups). In the listing-type mode the ancestors are re-checked every 256 unlinks or every 50 ms (`LISTED_TYPE_BINDING_CHECK_INTERVAL`), whichever comes first, so on a slow volume a directory moved out mid-removal loses at most the unlinks of about 50 ms. A parallel catch-up drain measures the trash volumes' available space once before its threads start and once after they join, and reports that change as the slice's `bytes`; its `stripped_bytes` and `scratch_bytes` are zero, so the strip ledger takes bytes only from single-threaded drains (per-entry volume deltas, approximate).
- Changed 2026-11-19 (trash catch-up starts with the worker). Measured on 0.8.133: after a restart, with the external volume idle and ~1,020 trash entries, the detached worker ran its other lanes for 6+ minutes before one catch-up slice, and quiet-volume deletion (2,297 files/s) is what those minutes waste. Now:
  - Whenever the trash holds entries, the detached worker starts the catch-up lane at the start of its life as a detached process of its own (`broker gc __trash-catchup <root>`, environment `AETHYME_TRASH_CATCHUP_WORKER_STARTED_MS` = the worker's start, epoch ms), or after its other lanes when the trash only filled during the run (attachment `checkout_trash_catchup_spawn`: `when` = `worker_start` | `worker_end`, `after_worker_start_ms`; counters `checkout_trash_catchup_lane_spawned`, `_lane_spawn_failed`, `_lane_runtime_unverified`, `_lane_storage_context`). A process, not a thread, because the worker runs at `IOPOL_THROTTLE` in the `PRIO_DARWIN_BG` band at process scope (`gc/background_priority.rs`, `lower_current_process`) and XNU takes a thread's effective I/O tier as the most throttled of its own and its task's, so no thread can lift itself out. The lane process raises itself: it leaves the background band, sets process-scope `IOPOL_IMPORTANT` (never throttled) and lowers CPU niceness to 10 instead; on Linux it keeps the inherited idle I/O class (`SCHED_IDLE`, `IOPRIO_CLASS_IDLE`), left unchanged because the measured starvation was macOS throttling. It opens its own broker without unrelated startup GC maintenance, drains until no entry is deletable, is still the single host-wide lane (`checkout-trash-catchup.lock`) and deleter (`checkout-trash-deleter.lock` per slice), with the same pacing, ceiling, 2 s pressure re-reads, memory-critical yield and 20-minute lap cap, and holds no repository lock. Measured just before: a lap of 20 minutes removed 21,896 files (25/s, 3 threads blocked in `unlinkat`) while `rm -rf` on the same volume did ~12,000 operations/s.
  - A lap that ends at its cap is followed at once by the next, at most 3 laps (`TRASH_CATCHUP_MAX_LAPS`) and 60 minutes per worker; any other end stops (counter `checkout_trash_catchup_next_lap`, and `checkout_trash_catchup_back_to_back` once per worker that ran more than one lap).
  - A starting lane retries the lane lock for 1 s, so a retirement run's momentary probe of it never turns a lane away. While a lane is live, a retirement run skips its own drain instead of waiting up to 35 s for the deleter lock (counter `checkout_trash_retirement_drain_skipped`; drain report field `skipped_for_catchup_lane`, `end_reason` `skipped_for_catchup_lane`); such a run does not count as stalled.
  - `checkout_trash_catchup` is recorded on the lane process's own metric row: `end_reason` (the last lap's), `io_policy` (what the process set: `important`, `inherited` or `unchanged`), `catchup_start_delay_ms` (the worker's start to the first slice; null when none ran), `laps`, and the sums of `slices`, `entries_removed`, `unlinks`, `bytes`, `drain_ms`, `busy_retries`, with `elapsed_ms`, `files_per_second_observed` and `drain_share_pct` (drain time as a share of the process's wall time). `checkout_trash_drain` adds `io_policy`, the process's disk I/O policy at the drain (macOS `throttle` | `utility` | `standard` | `important` | `passive` | `default`, Linux `idle` | `best_effort` | `realtime` | `none`, or `unknown`). `gc trash drain` JSON adds `io_policy`. The lane process also records `checkout_trash_catchup_priority`: `io_policy` (the policy read back after raising, the effective one), `requested`, `darwin_bg_cleared`, `cpu_niced`; a read-back still `throttle` counts `checkout_trash_catchup_priority_not_raised`. The lane process (`gc.__trash-catchup`, no outcome event) and `gc trash drain` (`gc.trash`) write command-metric rows. The hidden action runs the worker entry's invocation checks (worker environment, one repository path that is its own main checkout, writable state, not a test or build environment, not an ephemeral volume, aligned runtime binaries) before opening anything. One lane process per repository: it holds `checkout-trash-catchup-<repository key>.lock` (host state directory; key = 16 hex digits of the SHA-256 of the canonical main root) for its whole life, holding the host lane or waiting for it; a second one ends at once (counter `checkout_trash_catchup_repository_lane_present`), and a worker starts none while it is held. A worker starts a lane process only for a deletable entry (not for unconfirmed, failing or last-seen-in-use rows alone). In a process that raised its priority, below the artifact-sweep headroom the drain is held to the catch-up rate (8,000/s, and the configured ceiling) instead of running unpaced (`initial_unlinks_per_second` is then set while `initial_pace` is `unpaced`); the worker's own throttled drains keep the unpaced mode.
  - New operator command `aethyme broker gc trash drain [--max-minutes <n>] [--json]`: the same lane in the foreground for this repository's pools, until no entry is deletable (not only while behind) or `--max-minutes` (default 20, 1 to 60) pass, in laps as above. Refused while another lane is live on the host. JSON: `end_reason`, `laps`, `slices`, `entries_removed`, `unlinks`, `bytes`, `drain_ms`, `files_per_second`, `entries_remaining`, `oldest_remaining_age_hours`.
  - Review fixes. The lane lock's holder line names the repository (`repository <main root>`), so a live lane is told apart as this repository's or another's. A retirement run skips its drain only for this repository's own lane; with another repository's lane live it drains as before (waiting up to 35 s for the deleter lock), and only an own-lane skip counts as not stalled. A worker starts its lane unless its own repository's lane is live; a lane that finds another repository's lane retries every 45 s within its 60-minute bound (counter `checkout_trash_catchup_lane_retry`; lap field `lane_held_by_own_repository`) and names its repository in `checkout-trash-catchup.waiting` beside the lock; a holder that sees a fresh note (under 2 minutes) from another repository ends after its current lap instead of chaining the next (counter `checkout_trash_catchup_yielded_to_waiter`); a lane removes its own repository's note when it starts. `gc trash drain` does not retry; it refuses. A drain writes back only the row fields it changed (failures, pending) relative to the rows it read, so a confirmation stored meanwhile is not reverted. `gc trash drain` runs at the caller's priority (not throttled).
- Changed 2026-11-19 (trash lane waits for any holder). Under 0.8.134 on the busiest repository the lane process ended `lane_busy` after ~1 s twice, deleting nothing: the host lane lock's holder line named no repository (written by another repository worker's probe, which took the lock to look at it), and a lane retried only a holder line naming another repository. Now:
  - Probes never write a catch-up lane lock's holder line. Whose lane is live (`checkout_trash_catchup_lane_state`, used by the retirement skip and the worker's spawn decision) and whether a repository's lane process is alive come from `probe_catchup_lane_lock`: a non-blocking take that writes nothing and is released at once; a free lock means no lane, whatever its line says (a lane killed without running its drop leaves its line behind), and only while the lock is held is the holder line read. A missing directory reads as free, any other failure as held. A lane lock (`CatchupLaneLock`, the host lane and the per-repository lane-process lock) writes `pid <n> since <ms>` and `repository <main root>` when taken and empties its line, still under the lock, when let go. A lane that could not take the host lane concludes that its own repository's live lane holds it only when two probes 200 ms apart both find it held, naming this repository and a live process other than itself; otherwise it waits and retries.
  - A lane that finds the host lane held waits and retries (every 45 s in the worker, within its 60-minute bound) unless the holder is this repository's own live lane, whatever else the line says (another repository, no repository, empty). The lap field is now `lane_held_by_own_repository`.
  - `checkout_trash_catchup` adds `lane_started_after_worker_ms`: when the lane process started, relative to the worker's start (set whenever the worker's start reached the lane). `catchup_start_delay_ms` stays null when no slice ran.
Covered by `checkout_trash.rs` and `broker/retire.rs` tests.

- Changed 2026-11-19 (retirement reaches its backlog). Measured on the busiest repository over a day: 174 retirement runs judged 775 closed checkouts (median 3 a run) in a closed lap of about 31 s a run, out of a 205 s run, and retired 140; the pool holds about 308 closed checkouts not retired. Now:
  - An archive whose destination refused creating a directory with a permission error (`EPERM`/`EACCES` from creating the archive parent, root or staging directory, decided at that call site, never from the error's words: all 15 of the day's `archive_failed` keeps were such refusals, from two worker runs that had inherited a sandboxed caller's sandbox) is kept as `archive_environment_denied` (counter `checkout_retirement_kept_archive_environment_denied`), not remembered, and no further archive is attempted in that run. Repeated denials back off per repository: the first is retried by the next run, then archives wait 10 minutes, doubling to 6 hours (meta key `checkout_retirement.archive_denied`: `consecutive`, `until_ms`; counter `checkout_retirement_archive_denied_waiting`); an archive that is written resets it. Progress field `archive_denials_consecutive`; status advice `cleanup.retire-archive-denied` (Warning) from 3 in a row. Any other archive failure, a checkout file the archive cannot read included, stays `archive_failed`, remembered with the existing back-off.
  - Expiring retired-commit pins runs at most every 30 minutes (meta key `checkout_retirement.pins_pruned_at_ms`; counter `checkout_retirement_prune_pins_not_due` when skipped) instead of using its whole 20 s budget on every run.
  - While the last strip pass counted 50 or more closed checkouts on disk, the closed lap's floor is 45 s instead of 20 s (still bounded by the lane's deadline); counter `checkout_retirement_closed_lap_backlog_floor` when it applies, and the `checkout_retirement` attachment adds `closed_backlog` and `closed_lap_floor_ms`.
Covered by `checkout_trash.rs` and `broker/retire.rs` tests.

- Changed 2026-11-20 (retirement lap order). On 0.8.139 the closed lap judged 0 to 5 checkouts a run (27 in 15 runs, 2 retired): one checkout's operation-state Git call took 37 to 45 s (the whole lap) on a slow volume, and three `git status` timeouts of about 14.5 s each took another lap, while 0.8.138's 14 a run had been cheap Finder-held `process_using_checkout` keeps. Now:
  - The run's open-file observation is asked before the operation-state Git call, so a held checkout is kept without any Git call (and is no longer marked operation-in-progress).
  - An inspection that failed or timed out on its own (`inspection_failed`) is remembered with the keep back-off (1 h, 6 h, then 24 h, or at once when HEAD or the index changes) instead of being judged again by the next lap; it is never retried within a run. A Git call the lap's own deadline cut short is kept as `deadline`, not remembered. The counter `checkout_retirement_inspection_retried` is retired.
  - The `checkout_retirement` attachment adds `closed_examined` (checkouts the closed lap judged), `closed_stop` (`deadline`, `lap_complete`, `process_use_unknown`, `archive_deferred`) and `failed_inspection_ms` (time in judgements that ended `inspection_failed`); counters `checkout_retirement_closed_lap_stop_<reason>`.

- Changed 2026-11-20 (retirement run reaches its closed lap). On 0.8.141 on the busiest repository (closed backlog 335, 683 open sessions), two of three runs never started the closed lap: resolving the open sessions' paths took 80 s and 63 s against a fixed third of the 180 s window (60 s), and a resolution that runs out keeps nothing. The recovery-archive bound then took 105 s and 339 s to list 2,390 archives with nothing listed (one `lstat` per archive root entry, then one per earlier recorded size), which was the 300 s no checkout-retirement span covered. Now:
  - Resolving open paths may use the window less the closed lap's floor, never less than a third (135 s of 180 s at the 45 s backlog floor).
  - While 50 or more closed checkouts wait, the closed lap runs right after the scratch pass, with the rest of the sweep window (at least its floor), and the strip and idle strip passes follow with their floors (counter `checkout_retirement_closed_lap_first`). Below 50 the order is unchanged.
  - The recovery-archive bound runs at most every 30 minutes while its last recorded pass measured every archive and found them within the bound, and the run's laps (which run before it) wrote no recovery archive. It runs on every run whose last record is over the bound or `partial` (it may undercount), and on every run that archived a checkout or an abandoned operation (from the storage ledger's `observed_at_ms`, `over_bound` and `partial`; counter `checkout_retirement_archive_bound_not_due` when skipped; span `checkout_retirement_archive_bound` around it). The archive listing takes entry types from the directory listing instead of one `lstat` per entry, and carrying earlier sizes forward checks only sizes the pass does not already hold. New spans `recovery_archive_listing_entries`, `recovery_archive_bound_state_read`, `recovery_archive_bound_state_carry`, and `checkout_retirement_setup` (the trash backlog and progress reads before the open paths).
  - The `checkout_retirement` attachment adds `closed_lap_first`, `open_sessions` and `open_paths_ms`, and `closed_stop` is `not_reached_open_paths` when the open paths could not be resolved in time (it was empty).
  - Unchanged and why. The plan's `git status` already runs with `GIT_OPTIONAL_LOCKS=0`, `core.fsmonitor=false` and `core.untrackedCache=false`; `git worktree list` takes no index or lock and reads each of about 1,000 registrations, already once per run. No cheaper listing gives the same answer, so both are left as they are.
Covered by `broker/retire.rs` tests.

- Changed 2026-11-20 (retirement inspections stop timing out at 15 s). On 0.8.142 on the busiest repository the closed lap was reached on every pass but judged 3 to 6 checkouts and retired 2 in 4 passes. 13 inspections ended `inspection_failed`, each at the eligibility `git -c core.fsmonitor=false -c core.untrackedCache=false status --porcelain=v1 -z --untracked-files=all --ignore-submodules=none` (`GIT_OPTIONAL_LOCKS=0`, no `--ignored`, so ignored directories are not walked) stopping at its 15 s limit, 79 to 81 s of an 81 to 84 s lap. A successful plan took about 13 s, and the checkouts hold about 17,000 tracked files on a contended volume. Now:
  - Inside the closed and cleaned laps, each plan's cleanup listings (that eligibility status and the archive inventory's untracked listing) may take up to 60 s (`RETIRE_INSPECTION_ALLOWANCE`) instead of 15 s, still bounded by the lap's deadline. Interactive cleanup and inline retirement at close keep 15 s. What the listings prove is unchanged.
  - The planning context a run captures once now takes each lap's own deadline before every plan. Before, a plan of the cleaned lap ran its Git calls under the closed lap's deadline, already past, so they were not started and could be remembered as `inspection_failed`.
  - The remembered-keep back-off for `inspection_failed` (1 h, 6 h, 24 h) was already applied: the 19 remembered keeps all had one attempt, and each pass judged fresh checkouts.
  - The `checkout_retirement` attachment adds `inspections`: `started`, `succeeded`, `failed` (`inspection_failed`), `cut_by_deadline`, `refused`, `median_succeeded_ms`.
Covered by `broker/retire.rs` tests.

- Changed 2026-11-20 (finish retires through the trash). On the busiest repository's last 400 finishes:
  - 181 (45 %) removed the checkout inline with `git worktree remove`: a median 48 s, p90 109 s, while holding the repository lifecycle claim, so every start and finish waited. Those finishes took a median 72 s, including a 13 s unlocked size walk.
  - 153 (38 %) kept it because the caller passed `--keep-worktree`.
  - 60 (15 %) kept it as not proven safe (dirty paths, or commits not on a delivery target).
  - 6 failed the removal.

  Now:
  - When `finish` removes a checkout it proved clean and delivered under the claim, the removal moves it into the checkout trash in one rename (`remove_checkout_through_trash`: registration removed, reflog-only commits pinned, an operation in progress refuses) instead of `git worktree remove`. The deleter removes the entry later, and only once no process uses it. So `finish` takes no open-file observation, and a process in the checkout (usually the finishing caller's shell) does not refuse the move. Anything the move refuses falls back to `git worktree remove`, as before. New span `finish_retire_move`; counters `finish_retire_trashed`, `finish_retire_git_fallback`, `finish_retire_holder_check_deferred_to_deleter`.
  - When `finish` leaves the checkout in the pool it records why on the session (meta key `checkout_retirement.finish_keeps`: session id to `reason`, `at_ms`; at most 16,384) and counts `finish_retire_kept_<reason>`. The reasons are `keep_worktree`, `dirty`, `not_proven_safe`, `not_broker_owned`, `policy_disabled`, `cleanup_plan_unsafe` and `removal_failed`. The retirement lane still judges the checkout with the same rules. It counts `checkout_retirement_judged_after_finish_keep`, carries the finish reason into its remembered keep's detail (`finish kept it: <reason>`), and forgets the record once the checkout is retired.
Covered by `broker/retire.rs` tests.

- Changed 2026-11-20 (checkout retirement lane process). The busiest repository started 295 sessions and finished 124 in a day, while the worker's closed lap (about 80 s of a 180 s, throttled window, a few times an hour, about 13 s per plan) retired 18; about 300 closed checkouts wait. Now:
  - New detached process `broker gc __retire-lane <root>` (`broker/retire/lane.rs`).
    - Start: the worker starts it at the start of its run, with the trash catch-up lane, when its environment gates pass (worker environment, durable repository, past the post-boot hold) and the last strip pass counted closed checkouts on disk (`checkout_retirement_lane_needed`). Counters `checkout_retirement_lane_spawned`, `_spawn_failed`, `_runtime_unverified`, `_storage_context`.
    - Invocation and priority: it runs the worker entry's invocation checks, then leaves the macOS background band, sets process-scope `IOPOL_STANDARD` (normal disk priority, below the trash deleter's `IOPOL_IMPORTANT`) and CPU niceness 10. Linux keeps the inherited class. Attachment `checkout_retirement_lane_priority`.
    - Opening: it opens its broker without startup GC maintenance.
  - Bounds.
    - One lane process per repository (`checkout-retirement-lane-<key>.lock` in the host state directory, key = 16 hex digits of the SHA-256 of the canonical main root). A second one ends `lane_present`. Probes take the lock for an instant and write nothing.
    - The worker takes the same lock for the length of its own closed and cleaned laps and skips both when it cannot (counter `checkout_retirement_laps_left_to_lane_process`; `closed_stop` `not_reached_lane_process`), so the kernel lock makes the worker's laps and the lane process exclusive and the lap cursors have one writer. The strip passes still run. A starting lane process waits up to 5 minutes for the lock while the worker's laps hold it, then ends `lane_present`.
    - One judgement at a time per volume, host-wide: each judgement holds `checkout-retirement-volume-<device>.lock` for its whole length, waited for until the slice's deadline. So one Git inspection at a time runs on a volume across every repository's lane process.
    - A judgement that never gets the turn is no verdict. It is kept as `volume_turn_unavailable` (counters `checkout_retirement_volume_turn_unavailable`, `checkout_retirement_kept_volume_turn_unavailable`, not remembered). The lap stops there and leaves its cursor before that checkout (`closed_stop` `volume_turn_unavailable`), instead of passing over it and every checkout after it.
    - Turns are shared fairly between repositories' lanes. A waiter keeps a note beside the lock (`checkout-retirement-volume-<device>.waiting`: `pid <n> at <ms>`, rewritten every 250 ms poll), and a process about to take the turn while another process's note is under 1 s old first lets 500 ms go by (counter `checkout_retirement_volume_turn_yielded`).
    - Pacing: before each 5-minute slice it reads host pressure. It waits 5 s while the host is loaded or memory is at warning (`paced_slices`), and ends at critical memory pressure (`pressure`). Each process runs at most 60 minutes (`cap`); the next worker starts another while closed checkouts wait.
  - Work. Each slice takes the open sessions' paths and the open-file observation once, then runs the closed lap, then (when that lap reached its end) the cleaned lap. These are the worker's laps (`retirement_lap`) with the same judgement, keeps, remembered-keep back-off and 60 s inspection allowance, and the cursors are shared with the worker. The process ends `backlog_done` once both laps reached their end in one slice: every checkout not remembered as kept was judged. Other endings are `process_use_unknown`, `open_paths_unresolved`, `no_pool` and `error`; counters `checkout_retirement_lane_end_<reason>`.
  - Telemetry. Each process writes its own command-metric row (`gc.__retire-lane`, no outcome event) with spans `checkout_retirement_lane`, `checkout_retirement_lane_pressure` and `checkout_retirement_lane_paced`. The attachment `checkout_retirement_lane` has `examined`, `retired`, `kept` (by reason), `inspections_started`, `inspections_succeeded`, `inspections_failed`, `inspections_cut_by_deadline`, `median_inspection_ms`, `slices`, `paced_slices`, `end_reason` and `elapsed_ms`.
Covered by `broker/retire.rs`, `gc/background_priority.rs` and `cli/telemetry.rs` tests.

- Changed 2026-11-20 (retirement observations and inspections that fit). On 0.8.143 on the busiest repository, six worker passes each judged one closed checkout and retired none.
  - Three ended `process_use_unknown`. Their open-file observation took 10.4, 10.6 and 10.9 s against `lsof`'s 10 s limit, under a load of 24 to 74 at the worker's throttled disk priority. In the same runs the trash drain's observation completed in 2.0 to 4.7 s and the landed share's in 1.8 s, so those workers were not sandboxed: `lsof` ran out of time. A sandbox refusal ends within a fraction of the budget.
  - Three ended `deadline`, each with one inspection started and cut by the lap's deadline.

  Now:
  - An incomplete observation is judged by how long it took, never by its words. One that used at least 90 % of its budget ran out of time and is retried once with up to 30 s when the run has room (`RETIRE_PROCESS_USE_RETRY_LIMIT`; `lsof`'s limit is a parameter, `open_file_snapshot_under_with_limit`). Counters `checkout_retirement_process_snapshot_retried`, `_retry_recovered`; span `checkout_retirement_process_snapshot_retry`. One that ended early was refused (counter `checkout_retirement_process_listing_refused`).
  - Before starting the lane process, the worker probes the process listing once (one bounded `lsof`, span `checkout_retirement_listing_probe`). A refused listing means a worker inherited from a sandboxed agent command, so it starts no lane, which would inherit the sandbox and judge nothing. It records `checkout_retirement.listing_refused_at_ms` and counts `checkout_retirement_lane_listing_refused`. A probe that only ran out of time still starts the lane, which runs at normal disk priority. The lane process probes once at its start too, and ends `process_listing_refused` before spending a slice.
  - Unsandboxed runners. The lane is started only by a worker that can list processes. Of the existing launchers, the managed GC sweep LaunchAgent (`gc service`) runs unsandboxed. The shared broker service accepts only `gc_plan` jobs and routes nothing. A sandboxed command reaches neither. So until the managed sweep is installed for a repository, its lane starts from the next worker an unsandboxed command (or the managed sweep) opens.
  - A lap does not start an inspection it cannot give its whole 60 s allowance. With less time left before the plan (whose context capture counts toward it), the checkout is kept as `inspection_deferred` (counter `checkout_retirement_kept_inspection_deferred`, not remembered), and the lap stops there with its cursor before that checkout (`closed_stop` `inspection_deferred`). With 50 or more closed checkouts waiting, the worker's closed-lap floor is now 75 s instead of 45 s, so one inspection with its allowance always fits. The lane process's 5-minute slices give each inspection its allowance except near a slice's end, where the same rule defers it to the next slice.
Covered by `broker/retire.rs` tests.

- Changed 2026-11-21 (retirement lane at an ordinary process's disk priority). On 0.8.144 on the busiest repository the first lane run (1,319 s, 5 slices) examined 24 closed checkouts and retired none.
  - All 16 inspections it started stopped in the eligibility `git status` at the 60 s allowance: 12 failed on the allowance and 4 were cut by the slice's deadline. The 15 `git_timeout_not_started` are each failure's follow-up `rev-parse --is-shallow-repository` for its message, which runs under the same expired deadline. A deadline reused from earlier is not the cause.
  - Timed once in an ordinary shell under a load of 25 to 28, the same read-only command took 0.14 s on one of those checkouts and 0.36 s on another closed checkout.
  - A sample of the lane process showed every thread blocked in single `stat` and `read` calls on the checkouts' volume.
  - Cause: at `IOPOL_STANDARD` (tier 1), the lane's I/O, and its Git children's (they inherit it), waited behind the agents' builds (tier 0) on the same volume.

  Now:
  - The lane process sets `IOPOL_IMPORTANT`, an ordinary process's policy (attachment `checkout_retirement_lane_priority` `io_policy` `important`), keeping CPU niceness 10. It stays bounded by one judgement at a time per volume and by host-pressure pacing.
  - Each lane run records its outcome (meta key `checkout_retirement.lane`: `runs_without_retirement`, and the last run's `examined`, inspections started, succeeded, failed and cut, end reason, time). A run that retires one resets the count; a run that ran no slice leaves it.
  - From 3 runs in a row that retired nothing while closed checkouts wait, `broker status` shows the Warning `cleanup.retirement-lane-idle` with the last run's counts, and the lane counts `checkout_retirement_lane_idle_runs`.
Covered by `broker/retire.rs` and `gc/background_priority.rs` tests.

- Changed 2026-11-21 (fast finish). On 0.8.144 on the busiest repository three of four finishes took 60 s, two of them failing on the lifecycle-claim wait. The finish that retired spent 2.2 s in `finish_retire_move`. The rest was `finish_size_walk_unlocked` (14.7 to 45.0 s) and `lifecycle_lock_wait` (13.3 to 34.8 s, `waited_behind` `broker gc __continue-artifacts`). The overlapping worker run held the claim 3 times for 80 s in all: the checkout strip pass's proofs, whose `git status` ran under the claim to the pass's deadline (`proof_git_stopped_at_deadline` 3, 23.7 s each). Now:
  - `finish` walks the checkout for its reported `reclaimed_bytes` for at most 2 s (`FINISH_SIZE_WALK_BUDGET`, `directory_usage_bounded`, never following links). A walk that runs out records no size, reported as 0 exactly like a failed walk (counter `finish_size_walk_budget_stopped`); eligibility never reads the size.
  - A background strip's proofs run under the lifecycle claim for at most 5 s (`STRIP_PROOF_UNDER_CLAIM_BUDGET`, with the Git deadline set to match). A proof that runs out is a transient refusal, tried again by the next pass (counter `checkout_strip_proof_claim_budget_stopped`). What a strip proves is unchanged.
  - A background lifecycle hold of 5 s or more is counted with its time (`lifecycle_lock_held_background_long`), so a slow foreground command can be traced to its holder.

- Changed 2026-11-21 (retirement keeps that never leave). On 0.8.146 the lane's second and third runs on the busiest repository examined 39 and 31 closed checkouts, retired 5 and 3, and judged the same kept ones again each run:
  - `operation_state_unknown` 16 and 18: the 18 closed checkouts whose Git registration is gone. The `.git` file names a missing `.git/worktrees/<name>`, so Git cannot read their state. These are full checkouts left by removals that stopped after Git dropped the registration; one has a recorded interrupted finish-cleanup proof.
  - `ownership_unknown` 2 and 3: the 3 closed folders with no `.git` at all (one holding only `.DS_Store`, two partial). They are remains of interrupted removals, and nothing proves whose they are.
  - `lease_held` 1: 15 implicit leases recorded 24 s after that session's `finish` had released its leases and closed it.
  - `operation_unresolved` 1: a recorded broker operation not resolved since boot.
  - `plan_refused` 10 and 3: 14 of the 16 remembered ones were the recovery patch's `git diff --binary` exceeding its output limit, and 2 were ignored directories over the file cap.
  - `archive_cleanup_failed` 2: unresolved artifact-use markers (`gc artifact-use reconcile`).
  - `changed` 2.

  Now:
  - A closed session's implicit (diff-derived) leases are released when the lane finds them (store `release_implicit_leases_of_closed_session`, recorded as `lease.released` with reason `closed_session_implicit_lease`; counter `checkout_retirement_released_closed_session_implicit_leases`). Explicit claims still keep the checkout.
  - A positively identified orphan of this repository (registration gone, `.git` naming this repository's `worktrees` directory) is resumed only when its recorded interrupted finish-cleanup proof validates as the cleanup that acts on it judges it (`Finish` lane). Otherwise it is kept as `orphan_cleanup_refused` without calling any removal.
  - The resume runs `Broker::cleanup` without force inside `OrphanResumeOnlyScope`. In that scope the recursive orphan removal (`remove_proven_orphan_worktree`) refuses (counter `checkout_retirement_orphan_recursive_removal_refused`), so only the content-verified interrupted-cleanup removal can act. That removal keeps every path changed or added after the proof. Any refusal keeps the orphan as `orphan_cleanup_refused` with the refusal as detail. The operator's `finish cleanup <id>` is unchanged.
  - Span `checkout_retirement_phase_orphan_cleanup`; counters `checkout_retirement_orphan_cleanup_attempted` and `_completed`.
  - `operation_state_unknown`, `ownership_unknown`, `lease_held`, `operation_unresolved` and `orphan_cleanup_refused` are remembered with the keep back-off (1 h, 6 h, then 24 h, or at once when the checkout changes) instead of being judged every run. A folder with no usable registration is keyed by its own modification time (`unregistered:<ns>`). A remembered `lease_held` keep ends as soon as the session holds no unexpired lease, and a remembered `operation_unresolved` keep ends once its operation is resolved (one database read when the lap reaches it; counter `checkout_retirement_keep_condition_cleared`).
  - `broker status` shows the Notice `cleanup.retirement-kept`: the remembered keeps by reason, with up to 8 session ids and one detail each.
Covered by `broker/retire.rs` tests.

- Changed 2026-11-21 (worker Git reads at an ordinary disk priority). In 23 worker runs on 0.8.146 on the busiest repository the removal lane's status proofs took 16.1 s each (`proof_uncommitted_check`, 70) and its cleanup status checks 9.9 s (`auto_cleanup_check_status`, 114). Capacity selection took 57.7 s a run and selected nothing. Meanwhile the retirement lane's same status took a median 0.87 s at an ordinary priority.
  - The worker sets process-scope `IOPOL_THROTTLE` and the background band (`lower_current_process`) to pace its deletions, as its drain's `io_policy` `throttle` shows. Every Git child inherits that policy, so the gap is I/O wait, not a longer walk.

  Now:
  - Before the Git runners spawn a child of a throttled process (macOS `throttle`), `prepare_git_child_io` classifies its subcommand from the arguments (`git_subcommand_is_read_only`).
    - Only a known read runs at an ordinary process's disk priority: `status`, `diff`, `diff-index`, `diff-files`, `diff-tree`, `ls-files`, `ls-tree`, `rev-parse`, `rev-list`, `merge-base`, `for-each-ref`, `show-ref`, `cat-file`, `log`, `show`, `name-rev`, `describe`, `check-ignore`, `check-attr`, `count-objects`, `var`, `grep`, `blame`, `worktree list`, `stash list`, `bundle verify`/`list-heads`, and `config` with `--get`/`--get-all`/`--get-regexp`/`-l`/`--list`. A `pre_exec` hook in the child leaves the background band and sets process-scope `IOPOL_IMPORTANT` before `exec` (counter `git_child_ordinary_io`).
    - Every other Git child keeps the inherited throttle (counter `git_child_kept_throttle`): any write, any deletion (`worktree remove`, `worktree prune`), and anything not known to be a read.
    - Every Git child of a throttled process also gets `gc.auto=0` and `maintenance.auto=false` through Git's environment configuration (`GIT_CONFIG_COUNT`, unless the command already sets it), so none starts automatic maintenance.
    - The runners covered are `run_git_process_output`, `_in_owned_group`, `_limited` and the removal proofs' `run_proof_git`; any other spawn keeps the throttle.
    - Linux keeps the inherited class.
  - The worker's own file deletions, the trash drain and the strip's renames are in-process and unchanged.
  - Overlap with the retirement lane is measured. A capacity-removal pass that ran while this repository's retirement lane process was alive counts `artifact_worker_capacity_removal_pass_while_retirement_lane_live` with the pass's time.
  - The removal lane no longer proves a closed checkout the retirement lane owns (a remembered keep, or already in its trash): removal refused it at its last step anyway. It is kept as `retirement_lane_owns` without a Git call (counter `auto_cleanup_skipped_retirement_lane_owns`).
  - Unchanged: the strip pass's backlog census (33.4 s a run) is the worker's own `lstat` of checkout folders and stays throttled.

## Repository lifecycle lock: ticket queue and telemetry (2026-11-17)

`.aethyme/run/worktree-lifecycle.lock` stays the one flock that serialises
checkout creation, adoption and removal; nothing about exclusion changed. What
changed is who tries it next, what a holder records, and what every claim
reports.

- **Queue.** A blocking claim (start, adopt, finish, cleanup, close, submit,
  gate admission, storage removal) creates a ticket under
  `.aethyme/run/lifecycle-lock-waiters/` named
  `<deadline ns>.<enqueue ns>.<pid>.<process start>.<sequence>`, both times on
  the host monotonic clock (`CLOCK_MONOTONIC`), where the deadline is the
  enqueue time plus the caller's wait budget. Tickets are served earliest
  deadline first, ties by enqueue time, PID and sequence (a total order), and a
  waiter tries the lock only while no live ticket sorts before its own. So gate
  admission (10 s budget) goes ahead of a `start` that still has minutes left,
  and an arrival with budget `b` goes ahead of an earlier waiter only while
  that waiter still has more than `b` seconds left. Tickets also age: a waiter
  passed by 3 later arrivals, or with no earlier arrival ahead of it for 20 s
  (whichever first), rewrites its ticket with `aged=1`. Aged tickets rank before
  every unaged one, in enqueue order. The rank comes from ticket name and
  contents alone (the owner writes `heartbeat=<ns> aged=<0|1> passed=<n>
  eligible_since=<ns>`, others only read), so all processes agree on one total
  order, and a `start` behind a stream of gate admissions waits for at most 3
  of them or 20 s before only earlier arrivals and the holder remain ahead. The head of the queue polls every 50 ms, others rescan
  every 250 ms, and each waiter reuses a ticket's liveness verdict for 1 s.
  While it waits, a waiter writes `heartbeat=<monotonic ns>` into its ticket
  every 2 s and re-creates the ticket under the same name if it disappeared;
  it removes the ticket once it holds the lock, on timeout, or on error. A
  ticket whose PID has exited, whose PID now has a different process start
  time, whose times lie more than 60 s ahead of the clock (written before a
  reboot; the clock is re-read for every ticket a scan reads), or that
  was enqueued more than 15 minutes ago is removed by the next waiter that
  scans. A live ticket whose heartbeat is more than 10 s old is passed over
  but kept. File modification times are not consulted, so a wall-clock jump
  ages nothing. Anything else in the directory is ignored. The wait stays
  bounded at 180 seconds (less under a command Git deadline, at most 10
  seconds for gate admission).
- **Foreground first, background bounded.** A nonblocking claim
  (`try_claim_worktree_lifecycle(lane)`, lanes `retire`, `auto_cleanup`, `gc`,
  `missing_checkout`, `abandoned_close`, `artifact_use_reconcile`) returns
  "busy" while any live ticket exists, even when the lock is free. Each lane
  records its run of yields in `.aethyme/run/lifecycle-lock-yields/<lane>`
  (`since_ns=<monotonic ns> count=<n>`). After 5 consecutive yields or 60 s of
  yielding, the lane's next attempt in a detached background worker process
  (the `gc __continue-artifacts` artifact, cleanup and retirement worker, the
  macOS managed `gc sweep` tick, `storage worker`; each enters that context
  where it starts) takes a ticket itself and waits its turn for at most 30 s
  (less under a command Git deadline). It then holds the claim or reports
  busy, and its run of yields starts over. While that process holds the GC
  lock the wait is capped at 3 s, so a foreground command waiting 30 s for the
  GC lock (`start` at the checkout limit, `gc apply`) is never outwaited. The
  GC lock is not released for the wait: it guards a pass's journaled plan, and
  re-validating that plan after re-taking it is not provably safe. Inline
  maintenance inside a foreground command (`status`, `start`, `push`, ...)
  never waits: a due lane only yields, and its streak carries over to the next
  detached pass. A direct acquisition also clears the record. `broker start` making room at the checkout limit and
  `gc artifact-use reconcile apply` claim inside a foreground scope and never
  yield. The background worker's capacity slice is not foreground.
- **Compatibility.** Binaries older than this change keep no ticket and poll
  unordered, so they can take the lock out of turn; ordering degrades, mutual
  exclusion does not. A reader must tolerate the new directory.
- **Holder record.** The lock file now reads
  `pid=<pid> since_ms=<epoch ms> kind=<foreground|nonblocking|promoted> session=<id|-> operation=<first command words>`
  (`operation=` stays last) and is emptied, still under the lock, when the claim
  is released. The session is taken only from a numeric `--session` argument.
  Readers of the older `pid=… since_ms=… operation=…` form keep working.
- **Refusal text.** A timeout names the holder's command, session, kind and
  hold age, how many other commands are queued, how many were ahead of this one
  then and when it queued, and still ends in "retry this command after it
  finishes".
- **Telemetry.** Every command that takes the claim, the timeout and error
  paths included, adds to its `command-metrics.jsonl` row the timed phases
  `lifecycle_lock_wait` and (when it held the claim) `lifecycle_lock_held` under
  `phase_timings_ms`/`phase_counts`, and a top-level object `lifecycle_lock`
  with `claim`, `outcome` (`acquired`, `timeout`, `failed`), `wait_ms`,
  `held_ms`, `queue_position_at_enqueue`, `queue_length_at_enqueue`, `queued`, `passed_by_later_arrivals`, `aged`,
  `waited_behind` (the holder's command when this claim first found the lock
  held) and `holder_operation`. A command that claims more than once reports
  summed phases and the last queued claim's object. A nonblocking claim adds
  to `lifecycle_lock_held` and counts `lifecycle_lock_nonblocking_acquired`
  (or `lifecycle_lock_yielded_to_waiter` when it yielded) but never writes the
  object. Background lanes write a separate object, `lifecycle_lock_background`:
  `lane`, `context` (`inline` or `detached`), `outcome` (`yielded`,
  `promoted_acquired`, `promoted_timeout`, `promoted_failed`),
  `consecutive_yields`, `yielding_for_ms`, on a yield
  `promotion_deferred_to_detached_worker` and, when promoted,
  `promoted_capped_gc_lock`, `promoted_wait_ms` and
  `queue_position_at_enqueue`, plus the timed
  phase `lifecycle_lock_promoted_wait`. A waiter that had to re-create its
  ticket counts `lifecycle_lock_ticket_recreated`. `broker start`'s own
  `phase_timings_ms.lifecycle_lock_wait` takes precedence over the span of the
  same name on success.
- **Shorter `start` hold.** The sparse-checkout plan is computed before the
  claim. Plain `finish`/`cleanup` still removes through
  `git worktree remove` under the claim; only verified-archive and retirement
  removals rename into the trash. The `start` hold itself is now limited to
  the reservation; see the next section.

### `broker start` holds the lifecycle claim only for its reservation

Introduced 2026-11-17. `broker start` (new checkouts, ordinary and detached)
holds the repository lifecycle claim for the checkout limit, root preparation,
storage admission, reaping dead start reservations, slug choice and recording
its reservation, then releases it. `git worktree add`, the configured-root
worktree lock, contract and witness capture, session registration and the
lease and preparation reporting run outside the claim. Changes consumers may
see:

- **Reservation files.** `.aethyme/run/start-reservations/<slug>.lock`, a
  JSON record (`pid`, `process_start`, `worktree_path`, `branch`, `base`,
  `created_at_ms`) that the start holds an exclusive `flock` on from
  reservation until its session row is committed or its rollback finished,
  then unlinks. The destination is created as an empty directory under the
  claim, after the file. A lock anyone can take means its start died; the next
  start reaps it under the claim, removing only an empty destination and only
  a branch that still names the recorded base while the destination has no
  `.git`. Anything else is left (and named in a warning) for the existing
  recovery lanes. A reader must tolerate the new directory; nothing else may
  write or remove its files.
- **Inline claims stay busy.** From the release of the claim until its row
  and reports are done, nonblocking lifecycle claims made by the same
  `start` (the lease-holder refresh's abandoned close, for one) answer busy
  without touching the lock, as they did while `start` held it.
- **Slug choice** treats a reserved slug as taken even before its path or
  branch exists, and never hands out a path any session row records, cleaned
  rows included (schema v65 adds the index `sessions_by_worktree_path`;
  compatible, floor stays v60). Otherwise a lane acting on the old row could
  reach the new checkout while `worktree add` runs.
- **Checkout limit** counts every live reservation as a checkout (each has no
  session row yet). One whose checkout Git already registered is counted
  twice until its row lands.
- **Free-space reserve.** Storage admission subtracts, per live reservation,
  the median recorded size of checkouts under the pool root (at least
  256 MiB) from the free space it compares with the reserve.
- **Removal lanes refuse reserved checkouts.** Every checkout removal goes
  through `GitRepo::worktree_remove`/`worktree_remove_registered` or
  `checkout_trash::move_worktree_to_trash`; each refuses a path another
  process's live reservation holds, with an error starting
  `start_in_progress:` that the lane records as its reason (a start's own
  rollback is allowed). Storage stray-directory apply refuses one too
  (`a running broker start has reserved this checkout path`). The shared
  helper is `start_reservation::path_is_reserved(main_root, path) -> bool`
  (reservation file present and its lock held); lanes check it under the
  lifecycle claim. The strip lanes' `reused_path_keep` (`start_reserved`)
  calls the same helper; a held file whose record is not written yet matches
  by its file stem, which is the destination folder's name. Git also marks a worktree `locked` ("initializing") while
  `worktree add` runs, so `git worktree prune` keeps it.
- **Nested-destination check** no longer runs `git worktree list` (17 s over
  857 registrations on the busiest repository): it walks from the
  destination's parent up to the pool root and refuses if any of them holds a
  `.git` entry. Root preparation for a fresh binding keeps its listing-based
  check.
- **Failed `worktree add`** retakes the claim, keeps everything if a session
  row owns the path, otherwise removes the empty destination and the branch
  if it still names the base (or rolls back a partial registration through the
  existing clean-checkout rollback), then releases the reservation. The
  error's recovery list names anything retained.
- **Mixed versions.** An older binary holds the claim for its whole start:
  it sees the reserved directory and picks another slug, and its lanes are row
  based as above, so the two cannot corrupt each other. A newer rollback holds
  the claim, so an older start is never mid-flight on the same path.
- **Telemetry.** New `phase_timings_ms` entries `start_reservation_reap`,
  `start_reservation`, `lifecycle_lock_held_reserve` and, on failure,
  `start_rollback`; counters `start_reservation_reaped` and
  `start_reservation_rollback`; and a metric object `start_lifecycle_split`
  with `reserve_held_ms`, `add_outside_lock_ms`, `reservations_reaped`,
  `reaped_directories_removed`, `reaped_branches_removed`, `reaped_retained`
  and `rollback` (`null`, `clean` or `retained`). Existing `lifecycle_lock`
  fields are unchanged; its `held_ms` now covers the reservation only.

Covered by `start_reservation.rs` and `broker/tests/start_reservation_lifecycle.rs` tests.

### Checkout retirement takes one process observation per run and archives abandoned operations

Introduced 2026-11-17. Changes consumers may see:

- A retirement run takes one host-wide open-file observation (`lsof`, at most 10 s, never past the sweep deadline) when the first checkout reaches the process-use precondition, and judges every later checkout of the run with it however old it gets. The per-pass re-take cap and two-second freshness of the older lanes no longer apply to this lane, so `checkout_retirement_kept_process_use_unknown` no longer counts checkouts judged after the cap. A checkout with no process working directory or open file under it, by that observation, is not unknown. New counters `checkout_retirement_process_snapshot` (time) and `checkout_retirement_process_snapshot_unavailable`.
- When the observation cannot be taken completely, the lap stops at the checkout that needed it (the cursor stays there), nothing is remembered, the report and progress field `process_use_unavailable` say so, and the next run is due within a minute. `cleanup.retirement-stalled` names it after 3 such runs.
- The rename into the trash reuses the run's observation (`checkout_trash_run_process_use_reused`) instead of taking a second one. A process that enters the checkout after the observation keeps its working directory, which now lies in the trash (on Unix a rename does not fail for a busy directory). The deleter therefore takes one open-file observation per drain before its first deletion and leaves alone any entry a process has its working directory or an open file under (`checkout_trash_entry_in_use`, report field `entries_in_use`, progress field `trash_entries_in_use`); an incomplete observation deletes nothing that run (`checkout_trash_process_use_unknown`, report field `process_use_unknown`). Callers outside the lane still take a fresh observation before the rename.
- Inside the lane, every live-checkout check (the run's open paths, the plan context, and each check of the archive path) reads the live session list fresh but canonicalizes only sessions the run has not resolved yet, by session id and recorded path (`checkout_retirement_live_paths_reused`). A new session, or one whose recorded path changed, is always resolved again.
- An archive is started only with at least 90 s of the lane's 180 s budget left and may use that whole budget rather than the sweep's two thirds; otherwise the checkout is kept as `archive_deferred` (not remembered) and the lap stops there unless it was the run's first judgement. An archive cut off by the deadline is kept as `deadline` (not remembered; the verified archive is reused next time) instead of `archive_cleanup_failed` for an hour.
- New `[cleanup] abandoned_operation_after_hours` (integer, 1 to 8760, default 24). A merge, rebase, cherry-pick, revert or bisect left in progress (or unmerged index entries) in a closed session's checkout is abandoned only when the newest modification among its operation markers (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG`, `rebase-merge/`, `rebase-apply/`, `sequencer/`), its index, its `logs/HEAD` and every changed, unmerged or untracked working file is older than that, the session's last broker activity and close are older than that, no open session uses the path, and no live holder is bound to the session (`checkout_retirement_kept_holder_alive`, `_holder_unknown`). It is then archived under the lifecycle claim into `<recovery archive root>/<session>-operation-<head12>-<nonce>/`: `git-state/` (the operation files and directories, the raw `index`, `HEAD`, `logs/HEAD`), `worktree/` (every path `git status --no-renames --untracked-files=all` reports, as on disk; missing ones listed as `deleted`), `ignored/` (the protected ignored files), `objects/` (a pack, its index and `ids.txt`: every object the raw index needs at any stage, and the `AUTO_MERGE` tree's objects, that the HEAD tree does not hold; every id is checked present before packing, the pack must pass `git verify-pack` and list every id, otherwise the checkout is kept), `README.md` (restore steps: copy the pack into the repository, recreate the worktree, restore files, index and state; expiry and how to keep it) and, written last, `manifest.json` (schema `aethyme-operation-archive-v1`, `created_at_ms`, `session_id`, each file's size, SHA-256 and original mode, the packed ids and pack hashes). Copies are 0600 and directories 0700; the archive root is made 0700, and refused if it cannot be. HEAD and every commit the state names, the rebase and sequencer todo and done lists included, are pinned as `refs/aethyme/retired/<session>/<commit>`. Immediately before the move every archived source is re-checked (size, modification time, inode; the index by SHA-256), `git status` must report the same paths and the missing ones must still be missing; any difference keeps the checkout as `changed` and forgets its remembered keep. Then the checkout is moved into the trash and the session recorded as cleaned. Over 512 MiB or 50,000 files keeps it as `archive_too_large`; any other failure keeps it as `operation_archive_failed` (remembered). New outcome counter `checkout_retirement_retired_operation_archived`, report field `retired_operation_archived`, progress field `abandoned_operations_archived` (cumulative) and advice `cleanup.retire-abandoned-operations-archived`. These archives expire by age through the storage ledger like every recovery archive (attributed to their pool and session by the manifest; default 30 days, after 7 days named in `broker status`); keep one with `aethyme storage keep-archive <pool>/<archive>`. A partial archive is named `.partial-operation-<session>-<created ms>-<pid>-<process start ms>-<nonce>` and holds no `manifest.json` until it is complete; the storage ledger never lists or expires a `.partial-*` or `*.partial` entry, and each retirement run removes partial operation archives older than a day whose creating process is gone (`checkout_retirement_operation_partial_swept`).
- Retirement plans (the lane only) no longer refuse a checkout for an ignored directory nothing catalogues as regenerable (`ignored path <dir>/ is neither a regular file nor a symbolic link`): every regular file and link under it is archived as a protected ignored file, at most 20,000 files and 512 MiB per checkout; a special file or a non-UTF-8 name under it still refuses. `finish cleanup resolve` keeps refusing unless given the new flag `--archive-ignored-directories` (on both the plan and its `--confirm`, since the digest covers the archived files).
- Remembered keeps carry the refusing step's own words (meta key `checkout_retirement.keeps`, field `detail`, at most 400 bytes), and `broker gc plan --json` lists them as `checkout_retirement_keeps` (`session_id`, `reason`, `detail`, `kept_at_ms`, `attempts`; reporting only, outside the digest, omitted when empty).
- Per-judgement phase spans `checkout_retirement_phase_ownership`, `_sessions`, `_operation_state`, `_process_use`, `_plan`, `_archive`, `_operation_archive` and `_move`; deleter span `checkout_trash_process_snapshot`.

Covered by `broker/retire.rs` and `cleanup_resolve.rs` tests.

### Retiring a checkout pins every commit only that worktree referred to

Introduced 2026-11-17. Before any checkout is moved into the trash (the clean path, the archive path and the abandoned-operation path alike), the commits only that worktree refers to are pinned as `refs/aethyme/retired/<session>/<commit>`: its current `HEAD` and `ORIG_HEAD` (by `rev-parse` inside the worktree, so a detached HEAD is pinned without a reflog; a missing `logs/HEAD` is counted as `checkout_trash_worktree_reflog_missing` and attached to the metric line as `checkout_trash_worktree_reflog`), those its HEAD reflog (`<gitdir>/logs/HEAD`, old and new ids) names, those its per-worktree refs (`refs/bisect/*`, `refs/worktree/*`, `refs/rewritten/*`, listed from inside the worktree) point at, and those `BISECT_LOG`/`BISECT_START` name, each unless already reachable from the default branch, a remote-tracking ref or an existing pin, or (on the recovery-archive path) carried by the verified archive's bundle (its HEAD, branch tips and unlanded commits). Reachability is one `git rev-list --stdin` per checkout over at most 2,000 candidates; more keeps the checkout (`trash_refused`, with the reason in its keep detail), and the pins are written with one `git update-ref --stdin`. This replaces the earlier `rev-list --reflog --not --all` step: run inside the worktree, `--all` counted the worktree's own per-worktree refs as keeping their commits, so a commit only `refs/bisect/*` (or `refs/worktree/*`, `refs/rewritten/*`) reached was never pinned and became unreachable with the registration; `--reflog` also walked every shared ref's reflog, pinning commits the worktree never referred to (311 pins in one move on a busy repository); and a commit kept only by a local branch counted as safe although the pin-expiry rule does not. Pins expire by the existing rule. Counters `checkout_trash_reflog_commits_pinned` (one per pin, unchanged name) and `checkout_trash_worktree_commit_candidates`, span `checkout_trash_worktree_commit_pins`. `finish cleanup resolve` help now lists `--archive-ignored-directories`.

Covered by `broker/retire.rs` tests.

### Closed checkouts are stripped of regenerable directories before retirement

Introduced 2026-11-17. Each `checkout_retirement` run first strips regenerable directories, each only with its witness (`node_modules` with `.modules.yaml`, `.package-lock.json`, `.yarn-integrity` or a `.pnpm` directory; `.pnpm-store` with a `v<digit>...` directory; `.venv` with `pyvenv.cfg` and nothing at its top outside `bin`, `lib`, `lib64`, `include`, `share`, `etc`, `pyvenv.cfg`, `.gitignore`, `Scripts`, `man`; Cargo `target` with a `CACHEDIR.TAG` carrying the standard signature; any of them with a `.git` entry at its top is kept as `nested_repository`; keeps are counted by reason, `witness_missing`, `unexpected_content`, `nested_repository`), out of the checkout folders of closed and cleaned spawned sessions, oldest close first, at most 200 checkouts per run and half of the sweep window. Directories come from `git ls-files --others --ignored --exclude-standard --directory` (collapsed: an ignored directory is one record and Git does not read below it). Each must pass `git check-ignore -q`, an empty `git ls-files -z -- <dir>`, real directories on the checkout's device for every component, and the checkout-level preconditions (closed or cleaned, no overlapping open session, holder gone or unbound, no lease, gate or unresolved operation, no process in the checkout by the run's single observation). It is then renamed into the pool's `.aethyme-checkout-trash` with an exclusive same-volume rename and recorded in `checkout_retirement.trash_rows` with the new row field `stripped: true` (omitted when false). Stripped rows never count as the checkout itself: they do not make the retirement lane own the session, do not prove the checkout trashed, and do not leave a `trashed_sessions` tombstone. Older binaries ignore the field and read such a row as the checkout's own entry, so during a mixed-version window they may skip the session in the old removal lanes until the deleter removes the entry. New meta key `checkout_strip.done` (session id to `closed_at`, at most 16,384) stops a handled folder being listed again until the session closes again. `finish` strips its own closed checkout, after its existing build-artifact reclaim (whose bytes it still reports), unless `--keep-worktree` was given, and `broker start` below the storage reserve releases its lifecycle claim, runs a strip pass (each checkout's renames under its own short claim, not yielding to queued waiters) and an unpaced deleter run outside any claim (30 seconds in all), then takes the claim again, prepares the worktree root again and re-checks storage before choosing the checkout name, or refuses (phases `storage_pressure_relief`, `storage_pressure_strip`, `storage_pressure_drain`, `lifecycle_lock_wait_after_strip`, `worktree_root_after_strip`); the refusal reason keeps its text and appends how many closed checkouts held top-level regenerable directories and what the pass freed (phase `storage_admission_after_strip`).

Immediately before anything is moved out of a checkout or the checkout itself is moved, under the lifecycle claim, the strip pass and every retirement-lane move (clean, archived and abandoned-operation paths, through `prepare_checkout_for_trash`) ask again whether the path was reused since it was judged (`reused_path_keep`): another open session at the path (`open_session`), a newer session recorded there (`leftover_superseded`), a live or unknown holder where the lane checks holders (`holder_alive`, `holder_unknown`), a folder born after the session closed (`created_after_close`: the directory's birth time, else the change time of its `.git` file, against the later of `closed_at` and a minute after the session's creation; `creation_time_unknown` when neither can be read), or a start holding the folder's reservation (`start_reserved`: `.aethyme/run/start-reservations/<folder>.lock` exists and its exclusive lock cannot be taken; `start_reservation_unknown` when that cannot be told). Any of these keeps the folder (counter `checkout_retirement_reused_path_kept`, span `checkout_reused_path_check`).

A plan refusal that is an inspection failure or a timeout (`its worktree is inspection_failed`, a Git timeout, or the run's deadline) is now kept as `inspection_failed` (counter `checkout_retirement_kept_inspection_failed`) and retried on the next run instead of being remembered as `plan_refused` for an hour; the last one is the progress field `last_inspection_failure`. Each retirement run, and each background checkout-cleanup worker pass, lists worktrees once (`WorktreeInventoryRunScope`; counters `git_worktree_list`, `git_worktree_list_reused`) for the lookups that verify on disk what they act on or that concern an existing folder: `checkout_trash::registration_of`, the adopted-registration match and the interrupted-finish path match. A worktree's lock reason is always read fresh from its registration's `locked` file (found from the checkout's `.git`, else by scanning `<common>/worktrees/*/gitdir`), never from a listing. Checks that a branch is checked out nowhere, that an interrupted finish's orphan is unregistered, resuming an interrupted finish and the missing-checkout lane still list afresh. A moved checkout is dropped from the cached listing.

After the closed-session lap, a second lap (cursor meta key `checkout_retirement.cleaned_cursor`) walks sessions already recorded as `cleaned` whose checkout folder is still on disk, oldest first, and sends each through the same retirement (strip already done, the remainder judged, archived when the rules require, then trashed). Such a folder is judged only through the newest session recorded for its path (`kept_leftover_superseded` otherwise) and only when that session's recorded holder is gone or unbound (`holder_alive`, `holder_unknown`). The closed lap may use the time up to halfway between the strip pass and the sweep deadline; the cleaned lap gets the rest. Report fields `cleaned_lap_complete`, `retired_cleaned_leftovers`; span `checkout_retirement_cleaned_lap`, `checkout_retirement_closed_lap`; counter `checkout_retirement_cleaned_leftover_retired`.

Progress field `strip` of `checkout_retirement.progress` carries `last_checkouts_examined`, `last_checkouts_stripped`, `last_directories_stripped`, `last_refused` (by code), `last_phase_ms` (`discover`, `prove`, `move`), `backlog` (`folders`, `closed_folders`, `closed_with_regenerable`, `observed_at_ms`: counts from one `lstat` per folder and per top-level catalogued name, no byte walk), `bytes_freed_total`, and `days` (UTC day number to `sessions_created`, `sessions_closed`, `checkouts_retired`, `cleaned_leftovers_retired`, `checkouts_stripped`, `directories_stripped`, `bytes_freed`, 14 days kept). `TrashDrainReport` gains `stripped_bytes` and `stripped_entries_removed`. The host state directory gains `storage-ledger-checkouts/` (classified as coordination), one atomically written `<sha256 of the repository root>.json` per repository holding its root and those counts, shown as the `checkouts` class of `aethyme storage status --json` (`folders`, `closed_folders`, `closed_with_regenerable`, `bytes_freed_by_strip`, `sessions_created_today`, `checkouts_retired_today`, `repositories`, `why_not`; absent until a run wrote it). `broker status` gains the advice `storage.closed-checkouts-backlog` (notice) at 50 or more closed-session folders. New spans `checkout_strip_pass`, `checkout_strip_discover`, `checkout_strip_prove`, `checkout_strip_move`, `checkout_strip_backlog`, `checkout_strip_at_finish`, `checkout_strip_under_pressure`; counters `checkout_strip_directory_trashed`, `checkout_strip_checkouts_stripped`, `checkout_strip_at_finish_stripped`, `checkout_strip_refused_<code>`, `checkout_retirement_inspection_retried`. Consumers must tolerate the new fields.

Covered by `broker/strip.rs` and `broker/retire.rs` tests.

Changed 2026-11-17 (strip throughput). The strip pass no longer asks Git to list ignored files: it lists the checkout and its sub-directories and probes the level below with `lstat` of the catalogued names (at most 1,024 directories, 4,096 entries per listing and 256 candidates per checkout; never a symlink, a catalogued directory, `.git` or `.aethyme`; counter `checkout_strip_discover_bounded`), and requires a `.git` entry at the checkout's top (`discovery_failed` otherwise). A checkout's candidates are proved in two Git calls (`git -c core.quotePath=false check-ignore -- <dirs>`, one path per line; `git ls-files -z -- <dirs>`) with the same refusal codes. A `.venv` top may also hold the regular files `CACHEDIR.TAG` and `.lock` (written by environment tools) and the exact file names declared under the new `[cleanup] venv_markers` (a list of at most 16 names, each at most 128 characters, no path separator or glob character, never `.env`, `.env.*`, `.envrc`, `.netrc`, `.npmrc` or `.pypirc`; validated by the cleanup policy loader; an invalid value declares nothing); anything else keeps it as `unexpected_content`. The names that did (names only, at most 16) are the report field `venv_unexpected`, the progress field `strip.venv_unexpected` (name to last seen, 7 days, dropped once declared) and the `broker status` advice `storage.venv-unexpected-content` (notice). A checkout kept only for `probe_failed`, `deadline` or `move_failed` is backed off (new meta key `checkout_strip.backoff`: session id to `closed_at`, `failures`, `until_ms`; 5 minutes doubling to at most 4 hours; reset by a clean outcome or a new close; at most 4,096 entries) and, while backed off, skipped without counting toward the per-run cap (counters `checkout_strip_skipped_backoff`, `checkout_strip_backoff_set`, `checkout_strip_backoff_cleared`; report fields `skipped_backoff`, `backoff_set`). The pass starts from one listing of each pool directory, judges each existing folder once through the row that owns it, and counts the backlog after stripping within 5 seconds (backlog field `regenerable_partial` when that ran out; `closed_folders` now excludes only exact open-session paths). Which row judges a folder is one rule, `path_superseded_by`: a cleaned row only when every row at the path is cleaned, a closed row unless a newer row there is not cleaned; the closed lap asks it before planning, and `created_after_close` is measured against the last close recorded at the path. `leftover_superseded` keeps its name. A checkout whose directories were kept for `probe_failed`, `deadline` or `move_failed` is not added to `checkout_strip.done`. `last_phase_ms` gains `folder_listing`. New spans `checkout_strip_folder_listing`, `checkout_strip_preconditions`; counters `checkout_strip_examined`, `checkout_strip_stripped_checkout`, `checkout_strip_stripped_directory`, `checkout_strip_found_<kind>`, `checkout_strip_stripped_<kind>` (`node_modules`, `pnpm_store`, `venv`, `cargo_target`), `checkout_strip_nothing_found`, `checkout_strip_skipped_remembered`, `checkout_strip_stopped_cap`, `checkout_strip_stopped_deadline`, `checkout_strip_backlog_partial`; and one metric attachment `checkout_strip` per pass (counts, refusals by code, `phase_ms` for `folder_listing`, `discover`, `prove`, `move`, and the backlog). Consumers must tolerate the new fields.

Changed 2026-11-18 (incremental strip). The strip pass picks one judge per recorded path from the session records alone, oldest close first, and resumes after the last judge it reached (new meta key `checkout_strip.cursor`: that judge's close time and session id; cleared when a pass reaches the end). A folder's existence comes from one listing of its pool directory by entry type, with no `lstat` per name (bounded to 10 seconds; past it, or when a pool cannot be listed, counter `checkout_strip_folder_listing_unavailable`, each folder is checked when it is judged). Each checkout gets at most 15 seconds; the deepest discovery level probes at most 64 directories. The `closed_with_regenerable` count runs only after 10 checkouts were judged or the pass reached the end (else `regenerable_partial`, counter `checkout_strip_backlog_deferred`). Report fields `folders_missing`, attachment fields `folders_missing`, `lap_complete`; counter `checkout_strip_folders_missing`. A retirement run gives the open-path resolution at most a third of its window, then the strip pass, the closed lap and the cleaned lap each a share of what is left of the sweep window with floors of 20, 20 and 10 seconds (never past the lane's deadline); new spans `checkout_retirement_sweep_partials`, `checkout_retirement_prune_pins`, `checkout_retirement_record_progress`. Open paths are resolved by up to 8 threads when there are 16 or more. At `finish`, a recorded holder that is the finishing agent itself (same pid and start time) no longer keeps the checkout (counter `checkout_strip_holder_is_finishing_caller`); any other live holder still does.

Changed 2026-11-18 (lane throughput). A pool directory that does not exist lists as empty (counter `checkout_strip_pool_directory_absent`), each pool listing has its own 10-second budget from when it is first needed, and an entry that vanishes while listed is skipped instead of dropping the listing. A strip refusal `checkout_missing` now means the folder is absent (`ENOENT`); when its pool directory can be listed the session is added to the new meta key `checkout_strip.absent` (session id to `closed_at`), skipped while its pool's listing cannot say otherwise and dropped when the folder shows up again in that listing (counter `checkout_strip_absent_seen_again`); any other failure to read the folder is the new code `checkout_unreadable`, and a folder that is not a real directory `not_a_real_directory`. The deepest discovery level probes only `node_modules`, in at most 32 directories. A nested `node_modules` without a package-manager marker passes as a pnpm workspace member (counter `checkout_strip_witness_pnpm_member`) only when every entry is a symbolic link, a `.bin` directory of `#!` shims of at most 64 KiB or links, or an `@scope` directory of links, a shim's name is also in the checkout's top-level `node_modules/.bin`, and at least one link points into `node_modules/.pnpm/`; the checkout's top-level `node_modules` still needs its marker. Retirement laps read existence from the same pool listings and remember a session kept as `leftover_superseded` with the row that superseded it (new meta key `checkout_retirement.superseded`; counter `checkout_retirement_superseded_remembered`), judging it again only when that row changes. Expiring retired-commit pins takes at most 20 seconds per run (counter `checkout_retirement_pins_stopped_deadline`) and asks the session's age, then ancestry of the default branch's tip, and only then scans every remote-tracking ref (counter `checkout_retirement_pins_remote_scan`). `gc reclaim plan` removes, when it saves a review snapshot, up to 256 older snapshots (`.aethyme-reclaim-plan-<64 hex>.json`) that are over 7 days old, or over a day old and scoped to a worktree that no longer exists (counter `reclaim_snapshots_pruned`). Stale-session classification reads refs first and asks the working tree only when refs alone class a session below unpushed (counter `stale_work_classification_refs_only`), remembering that answer per session for 6 hours while HEAD, the branch tip and the index size and modification time are unchanged (new meta key `stale_sessions.clean_checks`; counter `stale_work_clean_remembered`). The remembered answer only keeps a session or orders work: the look under both claims that decides a close always asks the working tree afresh.

Changed 2026-11-18 (moved-on holders). The strip pass, its check under the lifecycle claim (`reused_path_keep`), the cleaned-leftover judgement and the abandoned-operation archive share one holder rule for closed and cleaned sessions (`closed_checkout_holder_keep`): a recorded holder that is gone or unbound does not keep the checkout, an unknown listing keeps it (`holder_unknown`), and a live holder (same pid and start time) keeps it (`holder_alive`) unless that same process was bound to a newer session after this one's last activity and close, the rule the stale lane already applies (counter `checkout_holder_moved_on`). A process with its working directory or an open file in the checkout still keeps it through the open-file check. Each checkout retirement run also prunes stale reclaim-plan review snapshots from the bound worktree root with the writer's bounds (span `checkout_retirement_prune_reclaim_snapshots`, counter and attachment `reclaim_snapshots_pruned_by_worker`). In the open-session retirement pass, a landed proof that spends the pass's landed budget still leaves the cursor on that session, but the pass goes on judging the abandonment window for the sessions after it, without landed proofs and without moving the cursor (counter `retirement_abandonment_judged_past_spent_landed_budget`).

Introduced 2026-11-18 (idle strip). New `[cleanup] idle_strip_after_hours` (integer, 0 to 8760, default 4; `0` turns it off; an unreadable or invalid value turns it off and the cleanup policy loader reports it). Each checkout retirement run, after the closed-checkout strip and with its own share (floor 15 seconds), strips catalogued regenerable directories from open spawned sessions whose last activity is older than that, oldest first, at most 100 per run, with the closed-checkout strip's catalogue, proofs, witnesses and trash path. Idle requires no process with its working directory or an open file in the checkout by a complete open-file observation (an incomplete one stops the pass), no unresolved coordinated operation, no running gate and no start reservation; a live holder alone does not block. Under the lifecycle claim the session must still be open at the same path with the same `last_activity_at`. After a strip the broker writes `.aethyme/run/preparation/session-<id>.stripped` (the relative paths removed), which makes `broker submit prepare status` report `stale` ("regenerable directories were stripped while the session was idle; prepare again") with its usual `next_action` until a later preparation record is written, and leaves one automation note (fingerprint `idle-regenerable-strip`). New meta key `checkout_strip.idle_done` (session id to the `last_activity_at` seen) means a session is stripped once per stretch of inactivity and armed again only by new activity. Report field `idle_strip` of the retirement report; progress fields `strip.idle_backlog` and `strip.last_idle_checkouts_stripped`; day counter `idle_checkouts_stripped`; the `storage.closed-checkouts-backlog` advice also fires at 50 idle sessions and names the idle backlog. Only a repository that declares its preparation (`.aethyme/prepare.toml`) is stripped while a session is open (refusal `preparation_not_configured`), so rebuilding is always one command. After a strip, unless another notice is already there or Git would see the file, the broker writes `.aethyme/broker-action-required.md` in the checkout, starting with `<!-- aethyme: idle-strip notice -->`, naming what was removed, saying the work is untouched and giving `aethyme broker submit prepare --session <id>`; the action-required blocker (shown by `broker status`, the agent hook and `broker advance`) then names that command as its `clear` instead of `broker submit`, and both the notice and the blocker clear themselves once a preparation record newer than the strip exists (`broker submit prepare status` reporting current also removes the notice). Counters `checkout_idle_strip_notice_written`, `_notice_skipped_other`, `_notice_skipped_not_ignored`. Span `checkout_idle_strip_pass`, `checkout_idle_strip_preconditions`; counters `checkout_idle_strip_examined`, `_stripped_checkout`, `_nothing_found`, `_noted`, `_note_failed`, `_off`, `checkout_idle_strip_refused_<code>`; attachment `checkout_idle_strip`.

Changed 2026-11-18 (idle strip review). For an open session only catalogued directories that a step of the session's own `.aethyme/prepare.toml` declares as an output (the output names the directory or an ancestor) are stripped; every other catalogued directory is kept and counted as `not_declared_output` (report field `kept_not_declared`, counter `checkout_idle_strip_kept_not_declared_output`). The closed-checkout strip is unchanged. Idle now also requires no recent file-system activity: the modification times of the checkout's Git index and `HEAD` (in its Git administrative directory), of the checkout directory and of up to 32 tracked top-level directories (`git ls-tree -d HEAD`) must all be older than `idle_strip_after_hours` (refusal `recent_file_activity`, report field `kept_recent_files`, counter `checkout_idle_strip_kept_recent_files`, attachment `checkout_idle_strip_recent_file_evidence`). Under the lifecycle claim the strip takes the checkout's artifact-use lock (`.aethyme/run/artifact-use/<checkout key>.lock`, held shared by `broker submit prepare`) exclusively without waiting (busy: refusal `checkout_busy`, not remembered as handled; error: `checkout_lock_unknown`), and takes a fresh open-file observation when the run's is older than 5 seconds (incomplete: `fresh_process_use_unknown`; counter `checkout_idle_strip_fresh_process_use`). The notice is created only when absent (`create_new`): another notice is never overwritten, an idle notice already there is kept, and a tracked path (`git ls-files --error-unmatch`) is never written (counters `checkout_idle_strip_notice_already_present`, `_notice_skipped_tracked`, `_notice_failed`). The action-required blocker of an idle-stripped session no longer depends on the notice: while the strip marker is newer than the last preparation record the session gets blocker `action:<id>` with the new scope `session` (`BlockerScope::Session`, rendered `session`; previously `repo`), whose `clear` is `aethyme broker submit prepare --session <id>`; another session's session-scoped blocker is not counted among the agent hook's warnings. `broker status --session <id>` adds advice `idle_strip_prepare` (severity warning, reason `dependencies_stripped_while_idle`, the prepare command in `commands`; counter `status_idle_strip_prepare_advice`). A conflict notice still takes precedence for the blocker; the advice and the automation note still name the prepare command.

Changed 2026-11-18 (idle strip coverage). For an open session the idle strip also takes a Python environment root (`StripKind::Venv`, proved by the closed-checkout `.venv` witness) whose interpreter a step declares as an output (`<root>/pyvenv.cfg`, `<root>/bin/python*` or `<root>/Scripts/python*`; a root holding other declared output paths but no declared interpreter is kept as `environment_root_creation_unproven`, counter `checkout_idle_strip_kept_environment_root_creation_unproven`, renamed from `checkout_idle_strip_refused_...` in the next release since it keeps one directory and never refuses the checkout), and a workspace member's `node_modules` (proved by the closed-checkout pnpm-member witness, counter `checkout_strip_witness_pnpm_member`) when the top-level `node_modules` is a declared output and is proved strippable in the same pass or already absent; members are proved in the same call as the root, before anything moves (`move_proven`, the moving half of `prove_and_move`), and move only once the root actually moved or was already absent; otherwise they are kept as `member_root_kept` (counter `checkout_idle_strip_refused_member_root_kept`). Coverage counter `checkout_idle_strip_covered_environment_root`. Report field and attachment key `by_kind` (`node_modules`, `node_modules_member`, `venv`, `pnpm_store`, `cargo_target`: `removed`, `removed_bytes`, `kept`, `kept_bytes`, `unmeasured`), measured only for covered candidates, after the cheap refusals and before anything moves, within 2 s per checkout and 6 s per pass (span `checkout_idle_strip_measure`, attachment key `measure_ms`); counters `checkout_idle_strip_removed_<kind>`, `checkout_idle_strip_kept_<kind>`.

Changed 2026-11-19 (idle strip memo rules). New meta key `checkout_strip.idle_done_rules` (integer) records the coverage rules the `checkout_strip.idle_done` memo was written under (`IDLE_STRIP_RULES`, now 2). A memo from older rules is set aside: every idle open session is examined once more under the current rules (counter `checkout_idle_strip_memo_outdated`; attachment key `memo_outdated`), and the first write under the current rules replaces the memo and records the rules. The rules number is bumped whenever what the idle strip may remove from an open session widens.

Changed 2026-11-19 (idle strip on a busy volume). The idle strip's artifact-use lock check (`try_artifact_use_reclaimer_past_exited_producers`) no longer lets a durable marker keep the checkout when its producer is proved gone, by the reconcile lane's proof: a `running` marker with a recorded process identity whose PID and start token are absent or reused and whose process group is absent, checked twice. Such markers leak when a guarded command is killed. They are not removed here. Counter `checkout_idle_strip_exited_producer_marker_passed`; a live or unprovable marker is now refusal `checkout_marker_live` (counter `checkout_idle_strip_refused_checkout_marker_live`), and `checkout_busy` means the lock itself is held. When the changed-file probe's `git status` cannot finish, the idle strip walks the checkout's modification times instead (`recent_mtime_walk`: every file and directory, ignored ones included, links not followed; left out are `.git`, the broker's runtime paths (`runtime_paths`, as the status probe excludes them) and a directory the strip catalogue's own witness accepts as regenerable, so a tracked source directory merely named `target` or `node_modules`, or `.aethyme/prepare.toml`, is walked; within the checkout's deadline and at most 5 s, and 200,000 entries; span `worktree_mtime_walk`, counters `worktree_mtime_walk_complete`, `_found_recent`, `_incomplete`, `_skipped_regenerable`). A recent time is activity (evidence `changed_file_walk`), a finished walk with none proves the session idle, and a walk that stops at its bounds keeps the session as `activity_unknown`.

Changed 2026-11-18 (archive, then delete). New defaults (every key keeps working; a repository that sets one keeps its value): `stale_landed_close_after_hours` 4 (was 24), `stale_pushed_close_after_hours` 24 (was 72), `stale_unpushed_close_after_hours` 24 (was 168), `stale_closing_notice_hours` 4 (was 24; still counted inside the window), `[retention] session_abandoned_after_hours` 24 (was 72), `session_landed_after_hours` 4 (was 24), `recovery_archive_days` 90 (was 30), and the storage ledger's `[budget] unlanded_archive_days` 90 (was 30). `broker start --reuse` (through `recreate_retired_checkout`) on a session whose checkout was retired with a recovery archive restores the newest archive instead of refusing: the archive is verified whole, the commits come from its bundle when the repository lacks the archived HEAD (`git bundle unbundle`), the checkout is created on the session branch at the archived HEAD (the branch is created there when absent; when it has moved, the archived HEAD is checked out detached), the staged and unstaged patches are applied with `git apply`, and the untracked and protected ignored files are copied with their modes (links as links) and read back against their SHA-256; any failure removes the partial checkout, keeps the archive and refuses with the archive's README path (span `recovery_archive_restore`, counters `recovery_archive_restored`, `recovery_archive_restore_failed`, `checkout_retirement_restored_for_reuse`). An `archive_too_large` keep now records the largest untracked or ignored paths (names and sizes only) as its detail in `checkout_retirement_keeps[].detail`, and the `cleanup.retire-archive-too-large` advice points there. A landed proof that runs out of budget backs that session's landed proof off (new meta key `auto_cleanup.landed_budget_backoff`: 1 h doubling to 24 h, cleared by any other outcome; counters `landed_proof_budget_backoff_set`, `landed_proof_budget_backed_off`). A checkout retired through a recovery archive (`RetiredArchived`, `RetiredOperationArchived`) leaves a tombstone at its path: a directory holding only `ARCHIVED-SESSION.txt`, plain text starting `aethyme: retired session checkout`, saying when it was retired, that its work is saved (branch, and the archive named on an `archive: <path>` line) and the one command that restores it there, `aethyme broker start --reuse '<path>'` (counters `checkout_retirement_tombstone_written`, `_failed`). The strip pass keeps a tombstone as `tombstone` (counter `checkout_strip_refused_tombstone`, remembered as done), the closed and cleaned laps skip it (`checkout_retirement_tombstone_skipped`) and remove it once its archive is gone, or, without one, once it is older than `recovery_archive_days` (`checkout_retirement_tombstone_expired`), and `start --reuse` removes it under the lifecycle claim before restoring at the same path (Git's registration was dropped when the checkout was retired; a failed restore puts the tombstone back). `start` never allocates a recorded path, so a tombstone's path is never handed to a new session. New meta key `checkout_retirement.archived_sessions` (session id to the archive path) records every session retired through an archive; `Broker::session_restorable_archive` answers whether a session still has an archive to restore (other lanes, such as merged-branch deletion, should keep such a session's branch). When the session branch is missing at restore time it is recreated at the tip the archive recorded (`branch_tip`, when the repository has that commit, else the archived HEAD; counter `recovery_archive_restore_branch_recreated`). A session-scoped command refused because its session is closed now appends `restore_required: ... restore it at the same path with `aethyme broker start --reuse '<path>'`` when a tombstone is at the session's path. Day counters `checkouts_archived` and `archive_bytes`; each retirement run attaches `checkout_retirement` (examined, retired clean, archived, operation-archived, cleaned leftovers, archive bytes, keeps by reason, trash entries removed, elapsed ms).

Changed 2026-11-18 (archive location, bound and restore review). The retirement lane (and a retirement-mode resolve plan's `archive_root`) writes new recovery archives under `<pool>/.aethyme-recovery-archives/<repository key>/`; the parent directory is created mode 0700 with the marker `.aethyme-recovery-archives-root.json` (`{"schema":"aethyme.pool-recovery-archives/v1","repository_key":...}`) and is identified by marker and path, never by device (counter `recovery_archive_pool_marker_mismatch`; a mismatch falls back to host state). Pool scanners treat the dot-named directory as infrastructure; it is distinct from `.aethyme-session-scratch`. A reviewed `cleanup resolve` keeps the host state root. `Broker::recovery_archive_roots` (pool first, then host state) is searched by archive ownership (`owned_recovery_archive`, prerequisite-ref release), `recovery_archives`, the gc archive inventory, `newest_recovery_archive`, the partial sweep and `has_restorable_state`; nothing is migrated. New `[cleanup] recovery_archive_bound_gib` (integer 0 to 1048576, default 20; `0` off): each retirement run measures both locations (span `recovery_archive_bound_pass`, budget 20 s), removes fully landed pool archives older than `[retention] recovery_archive_days`, then, over the bound, the oldest fully landed archives past their minimum retention under the lifecycle claim (re-proven, manifest last, prerequisite refs released); archives holding unique work are never removed for size (counters `recovery_archive_bound_within`, `_evicted`, `_expired`, `_evict_failed`, `_over`; attachment `recovery_archive_bound`). New ledger directory `storage-ledger-recovery-archives/<sha256 of the repository root>.json` (`RecoveryArchiveUsage`: `pool` and `host_state` archives and bytes, `bound_bytes`, `over_bound`, `unique_kept_over_bound`, `unique_kept_sessions`, `evicted`, `evicted_bytes`, totals, `partial`), shown as `classes.recovery_archives_by_location` by `storage status --json` and as advice `storage.recovery-archives-over-bound`. Storage worker age expiry no longer removes an archive holding unlanded work: the expired candidate is refused (`unlanded at its retention age, ...`; counter `storage_archive_kept_unlanded_at_age`), listed in the summary's new `unlanded_archives_at_age` and named by advice `storage.unlanded-archives-at-age` (session, restore command, dismiss command) until `storage keep-archive <id> --dismiss` writes `<archive>.aethyme-dismissed` (counter `storage_archive_dismissed`) or new `[budget] expire_unlanded_archives = true`. A `--json` refusal of a closed session whose checkout left a tombstone carries `error.action` = `{"code":"restore_required","session_id":N,"path":...,"command":"aethyme broker start --reuse '<path>'"}` (counter `closed_session_refusal_restore_required`). Restore builds at `.<name>.aethyme-restoring` beside the path and `git worktree move`s it into place; after the staging checkout exists, `.<name>.aethyme-restore-incomplete` is written and removed only on completion or a successful undo; while it exists `start --reuse` and `recreate_retired_checkout` refuse (counter `recovery_archive_restore_undo_failed`). A restore onto a moved branch is reported in `start --reuse` output (text first, and `restore` in `--json` with `detached.branch_tip`, `archived_head`, `reconcile_command`, `compare_command`; counter `recovery_archive_restore_detached`). Operation archives are never selected for restore (`newest_operation_archive` finds them); their tombstone carries `operation archive: <path>` and `restore: manual` with manual steps instead of an `archive:` line. A failed tombstone write removes only its file and then the directory with `remove_dir`. Tombstones are recognised by the cleanup lane (not a present worktree, counter `checkout_cleanup_tombstone_skipped`), the strip backlog counts (`checkout_strip_backlog_tombstone`), reclaim discovery (`reclaim_discovery_tombstone_skipped`) and storage root inspection (kept, never a candidate; `storage_root_tombstone_retained`); the gc orphan-roots lane judges whole roots and needs no change.

Changed 2026-11-18 (archive re-review). A gc plan keeps an orphaned worktree root whose `.aethyme-recovery-archives/<key>/` holds any archive not proven landed (blocker `orphan_root_holds_recovery_archives`, naming the count and the location; counter `gc_orphan_root_kept_recovery_archives`) and re-checks it before removal (a failure line, no deletion). A manifest without `ignored_files` (schemas 2 to 4) counts as holding uncommitted content in the broker too (`holds_uncommitted_content`), so it is never landed and never evicted for size. The size bound honours `<archive>.aethyme-keep` (counter `recovery_archive_bound_kept_by_owner`). Activity evidence now includes changed files: one `git --no-optional-locks status --porcelain=v1 -z` without ignored files, bounded by 2 s and 2,000 paths (span `worktree_status_activity`; counters `worktree_status_activity_unavailable`, `_capped`, `_fallback`), whose newest modified or untracked path or parent modification time counts as activity in `checked_worktree_activity_ms` (abandonment and close decisions; fallback counter `worktree_activity_status_fallback`) and in the idle strip (`recent_file_activity` evidence `changed_file`; fallback counter `checkout_idle_strip_status_activity_fallback`). The idle strip's declared-output coverage is now exact: only a catalogued directory a step output names exactly is stripped from an open session. After a restore from a recovery archive, when the repository declares its preparation, the broker writes the preparation strip marker for the declared outputs and the same `.aethyme/broker-action-required.md` notice as an idle strip (wording for a restore; counters `checkout_restore_notice_written`, `_not_written`), so the session-scoped blocker and `idle_strip_prepare` advice apply; `start --reuse` prints the prepare command and adds `restore.next_action` in `--json`. `broker push` and `broker sync` print one line `Next: ... prepare before building or testing: aethyme broker submit prepare --session <id>` and add `preparation_next_action` to their `--json` when the session's preparation is stale because of an idle strip or a restore (counter `prepare_next_action_shown`). Later the same day: the host storage lane (`gc storage`) also keeps an orphaned root holding an archive not proven landed (a root blocker with the count and location; counter `storage_orphan_root_kept_recovery_archives`) and re-checks before removing an `orphan_root` candidate. The changed-file probe lists untracked files with `--untracked-files=all` (within the same 2,000-path cap) and reads paths as bytes; in the idle strip a probe that cannot complete keeps the session (refusal `activity_unknown`, counter `checkout_idle_strip_refused_activity_unknown`, not remembered as handled) instead of falling back.

### Merged branches of cleaned sessions are deleted locally

The detached worker deletes a local `refs/heads/<branch>` when every session that used it is cleaned with no restorable state (`Broker::has_restorable_state`: not open or closed, no recovery archive), its tip is an ancestor of the remote default branch as last fetched (a missing tracking ref skips the slice; a stale one only keeps more), no worktree has it checked out, and the ref still names the judged tip (`update-ref -d <ref> <tip>`). Sessions, restorable state, Git's worktree registrations (`HEAD`, a rebase's `head-name`, a bisect's `BISECT_START`) and the start reservations of the sessions' paths (`start_reserved`) are re-read under the lifecycle claim immediately before each delete. A slice judges at most 200 branches and deletes at most 100, from cursor meta key `merged_branches.cursor`; meta key `merged_branches.summary` records `at`, `cleaned_session_branches`, `deleted`, `deleted_total`, `kept_by_reason` and `skipped`. Status advice `cleanup.merged-session-branches` (severity `notice`) appears while branches of cleaned sessions remain. `[cleanup] delete_merged_session_branches = false` turns it off. No remote branch or remote-tracking ref is deleted and nothing is fetched. Command metrics carry `merged_branches_deleted`, `merged_branches_kept_<reason>` and the `merged_branches` attachment.

Changed 2026-11-21 (shorter claim). A slice judges every branch of its window from facts read without the repository lifecycle claim: ancestry of the judged tip and the fetched default commit, restorable state, sessions and worktree registrations. It takes the claim only when a branch is judged for deletion, judges each such branch again under it just before its delete (unchanged), and holds it at most 4 s, then starts no further deletion; deletions not reached by then resume from that branch on the next slice. A slice that finds nothing to delete never takes the claim, so it no longer yields (`skipped: yielded_to_foreground`) to a foreground holder. On the busiest repository the old slice held the claim for its whole 20 s budget in most worker runs (`lifecycle_lock_held_background_long` 7 to 26 s). New span `merged_branches_judge` (judging, unclaimed) and `merged_branches_claimed` (the claimed part), and count `merged_branches_claim_budget` (the 5 s hold stopped the deletions).

### Background cleanup reads `[cleanup]` from the default branch when the primary checkout is behind

Added 2026-11-18. The detached worker resolves its `[cleanup]` source once per run, before anything reads `[cleanup]` (the unattended unlink pacing included), and installs it for every thread of the process. It reads the `[cleanup]` table from the default branch `refs/remotes/origin/HEAD` names (it must target `refs/remotes/origin/`; never the checked-out branch's upstream; never fetched) when the primary checkout's config file and every format twin equal `HEAD`'s blobs with Git's filters for the path applied (`hash-object --path`), the default branch's config blobs differ from `HEAD`'s, `HEAD` is an ancestor of the default branch, and the default-branch config parses with a valid `[cleanup]` table. Otherwise the working copy is read, as before; foreground commands always read the working copy. A working-copy config whose size or mtime (then content) changes during the run sends the rest of the run to the working copy. Readers routed through it: `AutoCleanupPolicy::load` (and everything built on it), `abandoned_operation_after_hours`, `recovery_archive_bound_gib`, `idle_strip_after_hours`, `venv_markers`, `unattended_unlinks_per_second`, the stale-session keys and `delete_merged_session_branches`. The lookup runs under a 5-second Git deadline. Command metrics carry `cleanup_config_resolve` (time), one of `cleanup_config_source_default_branch` or `cleanup_config_source_working_copy_<reason>` (`same`, `no_default_branch`, `default_branch_unreadable`, `head_unreadable`, `git_timeout`, `working_copy_not_regular_file`, `working_copy_unverifiable`, `local_edit`, `diverged`, `default_branch_unparsable`), `cleanup_config_source_working_copy_changed_during_run`, and the `cleanup_config` attachment (`source`, `reason`, `default_differs`). Meta key `cleanup_config.last_resolution` records `at`, `source`, `reason`, `default_differs`, `default_ref` and `default_commit`; status advice `cleanup.config-source` (severity `notice`) appears whenever the last run saw the default branch differ: with the reason when it kept the working copy, or, when it read the default branch, saying that foreground commands and previews still show the working copy until the primary checkout is brought up to date.

### Broker advance

`broker advance --json` reports `aethyme.broker-advance/v1`, including
the stage reached, preparation state, published head or pull request when
confirmed, and one `next_action`. Exit 5 marks an ambiguous write with
an `operation_id` for read-only inspection before reconciliation. The
transition does not replay remote writes. The existing push coordinator can
re-evaluate one proven pre-write remote tip race.

A different live agent is refused with exit 3 and a `--take-over` hint.
`broker advance --session <id> --take-over` uses the same holder admission as
`broker push`, records the transfer before running the transition, and leaves
the previous live holder subject to the same guard.

Command metrics record `advance_broker_open`, `advance_state_validation`,
`advance_preparation_status`, `advance_prepare`, `advance_commit_state`,
and `advance_publish` when those phases run. Existing child spans still
record the preparation and push work within those phases.

### Early verification

`broker advance --preverify` can start a worker after it confirms a clean
published head and a fetched default base. The advance response's optional
`preverification` field reports whether the worker was scheduled. For
foreground diagnosis, pass the full object IDs to
`broker submit preverify <head> <base> --session <id> --json`. It reports
`state` and individual `gates` using the existing gate outcome shape.

The worker checks session ownership and requires the base's trusted policy.
It replays the pinned inputs in a separate exact-tree slot. There is one
nonblocking worker slot per repository and a 45-second gate budget. A worker
only runs gates that allow verdict reuse and have a complete execution
profile. It yields to occupied verdict and owner locks, host resource
queues, and configured load limits. It reports `no_verdict` when no gate
produces a verdict. It checks for a changed local head
or fetched base every 500 ms while gates run and interrupts its own gate
processes when the candidate changes. Early failures do not qualify for
cache reuse. Equivalent foreground requests can join its reusable gate work
using the exact tree, definition and execution profile lock. Ordinary submit
still decides the full verdict against its own verified candidate, including
any gates excluded from early verification.

The worker logs `gate_preverify_admission`, `gate_preverify_replay`,
`gate_preverify_checkout`, `gate_preverify_execution` and `gate_preverify`
timings, and an `early_verification` metric attachment with `state`,
`executed_gates`, `cached_gates`, `cancelled_gates` and `executed_gate_ms`.
Submit counts owned linear commits already based on the exact replay tip
as `submit_replay_exact_parent`. The replay uses their existing Git trees
in a complete, unmodified history view. Divergent inputs and merge commits
retain their ordinary replay and provenance checks.

The parent records `gate_preverify_launch`. These describe gate work and
scheduling. The worker never creates or updates a submission queue entry.

### Broker command attribution

Command metric JSONL rows may carry a numeric `session_id`, a classified
`outcome`, and `attribution.schema_version = 1`. The timing fields
`child_command_ms`, `queue_wait_ms`, and `broker_and_unattributed_ms`
partition the observed CLI wall clock. Overlapping children count once, and
instrumented lock waits take precedence over concurrent child spans. The
residual includes uninstrumented work. `retries` counts observed remote-tip
redecisions. `output_bytes` counts broker-rendered stdout, excluding forwarded
command output and stderr. Read-only metrics remain opt-in with
`AETHYME_MEASURE_OUTPUT` to preserve the default read-only contract.

`aethyme broker advanced insights --json` adds `task_attribution` with totals
for observed closed or landed tasks and a bounded newest-first breakdown.
A task is identified by its session and start timestamp. A recorded reuse event
starts a new task within that session. The report exposes measurement coverage,
the 16 MiB command-history limit, read errors, and command rows without a known
task. Commands and outcomes refer to the observed window and retained history.
`merge.conflict` opens one episode per session until a verification or landing
resolves it. To explicitly record an intervention that prevented duplicate work,
use `aethyme broker finish close --session <id> --duplicate-of <peer-id>`.
This requires the broker's current live duplicate-work evidence and a clean,
unchanged checkout. Ordinary overlap warnings remain a separate count.

`costs` in each task row and `completed_task_totals` also record
`gate_cache_hits`, `gate_execution_ms_avoided`, `shared_gate_verdicts` and
`shared_gate_wait_ms`. The counters use session-attributed `gate.cached`
events in the reporting window. An event with `shared_wait_ms` records a
successful reuse after waiting for a producer. Older cache events contribute
only to total hits and recorded execution avoided. The source run's recorded
duration measures reused child work. The waiting duration is observed
separately and cannot be subtracted from the source duration to infer a task
wall-time saving. The existing `gate_verdict_wait` span contributes to the
command's queue partition. Finish retains the command's repository identity
through checkout removal so it can still append the timing and outcome.

With `AETHYME_MEASURE_OUTPUT` enabled, recognized agent hooks in a registered
worktree add `hook.<event>` rows to the same ledger. `hook_dispatch` names
the broker work. `hook_delivery` is `context`, `denied`, `silent`,
`unavailable` or `output_error`. `output_bytes` includes the emitted
envelope's trailing newline and is zero for no successful delivery.
The hook's wire format and exit contract are unchanged. The row measures
the native invocation, excluding the host's process launch and shim time.

### Session scratch directories

Introduced 2026-11-18. Every spawned session whose checkout sits in a
broker-owned worktree pool gets a scratch directory the broker creates,
records and removes: `<pool>/.aethyme-session-scratch/s<session>-<hex ms>`
(mode 0700; the container is a private, marked directory owned by the user,
dot-named so pool scanners skip it like `.aethyme-checkout-trash`). It is on
the checkout's volume and outside the checkout, so it never appears in
`git status`. Adopted checkouts get none.

New surface consumers may begin to depend on:

- `broker start --json`, `--reuse --json` and `--adopt --json` carry
  `scratch_dir` (the path, or `null` when the session has none) and
  `scratch_dir_note` (what to put there, or why there is none). The human
  output prints `Scratch directory: <path>` and the instruction under the
  working agreement. `start-agent` output does not carry them.
- The session view of `broker status --json` (`aethyme.broker-session-view/v2`)
  carries `session.scratch_dir` (path or `null`); status never creates one.
- The environment variable `AETHYME_SESSION_SCRATCH_DIR` is exported to
  `broker advanced exec` commands, preparation steps, and gate commands run
  for a session (an inherited value is removed from gate commands). `TMPDIR`,
  `TMP` and `TEMP` never name the scratch directory itself: on the busiest
  machine a scratch path is about 130 bytes (and holds a space), past the
  Unix socket path limit (104 bytes on macOS, 108 on Linux), so tools that
  create sockets under the temp directory (SSH and GPG agents, Python
  multiprocessing, tmux) would fail there. Each command gets a run directory
  inside it instead (see "Command temp directories" below).
- `aethyme storage status --json` adds a `session_scratch` class
  (`directories`, `bytes`, `kept`, `partial`, `bytes_freed_total`,
  `repositories`), written by the retirement lane to
  `<host state>/storage-ledger-session-scratch/` (classified `coordination`).
  `broker status` adds advice `storage.session-scratch-large` (notice) when
  one repository's scratch exceeds 16 GiB or one directory exceeds 8 GiB.
- Broker metadata key `session_scratch.records` (path to record: session,
  pool, entry, device, inode, created time, `superseded`, last `keep` reason
  and `attempts`) and `session_scratch.bytes_freed_total`.
- The bytes in the `session_scratch` class come from a measurement lap
  spread over runs (broker metadata key `session_scratch.measure_lap`, JSON
  `started_ms` and `after`, the lap in progress and the last record it
  measured; `so_far`, its totals; `last` and `last_completed_ms`, the last
  lap that went round; totals carry `measured`, `bytes`, `largest_bytes`,
  `largest_session_id`, `partial`, `finished_runs`, `finished_run_bytes`).
  A run walks the records after `after`, in key order, for at most 2 s (at
  least one record); a new lap starts 15 minutes after the last one went
  round, and between laps a run walks nothing (counter
  `session_scratch_measure_not_due`). The class shows the last complete lap
  (`partial` only when a record's walk stopped at the budget), or the lap in
  progress as `partial` before any went round; `directories`, `kept` and
  `refused` are counted from the records on every run. Through 0.8.158 every
  run walked from the first record for its 2 s, never went round and so
  reported only the first records, always `partial`. Counters
  `session_scratch_measure_lap_started`,
  `session_scratch_measure_lap_completed`; span
  `session_scratch_measure_record` (one per record walked: the phase count
  is the records walked); the `session_scratch_usage` attachment gains
  `measure_due`, `measured` (records walked this run) and `lap_open`.
- Trash rows (`checkout_retirement.trash_rows`) gain `scratch: true` (omitted
  when false); such rows also carry `stripped: true`, so older binaries treat
  them as stripped directories, never as the checkout. `TrashDrainReport`
  counts their bytes as scratch, not strip.

Lifecycle: `start --reuse` marks the previous directory superseded and creates
a fresh one. Each `checkout_retirement` run, after its strip pass and within a
5-second budget, retires the directories of closed or cleaned sessions (or of
sessions no longer in the store) and superseded ones: no process may have its
working directory or an open file inside (the run's single open-file
observation), the lifecycle claim is taken without waiting, the record must be
unchanged, and the directory must verify (walked from `/` without following a
link, private marked container, recorded device and inode, same device as the
pool). It is then renamed into the pool's checkout trash (row recorded pending
first) and unlinked later by the paced host deleter after its own open-file
observation. Keeps are retried every run (`session_unknown`,
`process_using_scratch`, `process_use_unknown`, `lifecycle_busy`, `changed`,
`not_a_recorded_scratch`, `not_a_real_directory`, `foreign_container`,
`identity_changed`, `crosses_device`, `move_failed`); a directory that is gone
drops its record. After a remount (new device number, same inode) the record
is re-identified under the claim by its marked container on the pool's current
device, its session-bound entry name and its inode, and the new device is
stored (counter `session_scratch_reidentified`); otherwise it is kept, counted
as `refused` in the `session_scratch` class, and `broker status` warns with
advice `storage.session-scratch-unidentified`. A superseded directory gets a
fresh open-file observation right before its claim and rename
(`session_scratch_superseded_fresh_observation`). A container holding only its
marker and scratch directories does not make a worktree root non-empty; it is
never a `gc storage` stray candidate, and `gc storage apply` refuses to remove
a root whose container still holds scratch directories. Entries in a container the broker did not record are counted
(`session_scratch_unrecorded`) and never touched. `finish` and `close` ask for
a retirement run when the session has a scratch directory. Counters
`session_scratch_created`, `session_scratch_existing`,
`session_scratch_recreated`, `session_scratch_superseded`,
`session_scratch_unavailable`, `session_scratch_create_failed`,
`session_scratch_retired`, `session_scratch_missing_forgotten`,
`session_scratch_kept_<reason>`, `session_scratch_refused_<code>`,
`session_scratch_trash_entries_removed`; spans `session_scratch_create`,
`session_scratch_move`, `session_scratch_retire_pass`,
`session_scratch_measure`; attachments `session_scratch_retire` and
`session_scratch_usage`; start phase `session_scratch`.

### Command temp directories (introduced 2026-11-21)

Every command the broker runs for a session -- `git push` and its pre-push
dry run (so the repository's pre-push hook), gate commands (submit and the
verification slot), preparation steps and `broker advanced exec` -- gets
`TMPDIR`, `TMP`, `TEMP` and `AETHYME_COMMAND_TEMP_DIR` naming a fresh
directory the broker created for that one run,
`<session scratch>/tmp/r<ms hex>-<pid hex>-<n hex>` (mode 0700), on the
checkout's volume. When the run directory's path is longer than 64 bytes,
the variables name a short symbolic link to it instead,
`/tmp/ae<uid>/<6 hex>`, in a private directory owned by the user; when that
directory is a link, foreign, or open to others, the inherited temp folder
is tried as the base (`<temp>/ae<uid>/<6 hex>`, 62 bytes for macOS's
49-byte per-user temp folder and uid 501), and when no alias path fits in
64 bytes (sized on the actual alias) the command keeps the inherited temp
folder. Socket paths count only the link's length while every byte lands
on the scratch volume. A command started with `AETHYME_COMMAND_TEMP_DIR`
equal to its `TMPDIR`, resolving into the same scratch's run container (a
gate inside a pushed pre-push hook), inherits that run rather than nesting
one. When the scratch's volume is below the gate headroom (8 GiB or 100,000
inodes, the classifier the storage sweeps use, read with one `statvfs`, or
the gate's own reading for a gate) the command keeps the inherited temp
folder. Commands with no session (the main checkout), coordinated `gh` and
`aethyme env exec` are unchanged.

When the command ends, success, failure or timeout alike (the guard drops
after the supervision reaps the command), an empty run directory (same
device and inode as created) is removed with one `rmdir`, and its link if
it is still a link to that run. A non-empty one stays where it is, link
included, so a process the command left behind keeps a working `TMPDIR`;
the broker writes a marker for it, `<session scratch>/tmp-done/<run name>`
(a new 0600 regular file in a private directory beside the runs, never
inside one, JSON `device`, `inode`, optional `alias`, `bytes` and
`complete` as measured within 100 ms). Each `checkout_retirement` run,
inside the scratch pass's budget, takes up to 64 marked runs per open
session's scratch directory: no process may use the run (the run's
open-file observation), the scratch directory must verify as for its own
retirement, the run container must be a private directory on the pool's
device, and the run must be a real directory (a link there is not followed)
with the marker's identity; the trash row (`scratch: true`,
`stripped: true`) is recorded pending first and the identity checked again
before the rename. Then the link (only if still a link to the run) and the
marker go. The paced host deleter unlinks the run. A run whose directory is
gone loses its marker and link. A run the broker never finished (it was
killed) stays inside the session scratch and goes with it. Links of the
broker's naming whose target is gone are pruned when a new link is made.
Nothing in the system temp folder that the broker did not create is ever
touched.

The retirement run is due every 10 minutes when idle (every minute while
busy) and starts from any broker command that opens the repository's
broker. A finished run measured at 1 GiB or more (or not measured
completely), or a session reaching 16 marked runs or 1 GiB of them, makes
the next step that can -- the end of an exec, a preparation step or a push,
or the start of the next command or gate run for the session -- request it
at once, the same request `finish` and `close` make. `broker status` adds
advice `storage.command-temp-backlog` (notice) when one repository has 64
or more marked runs or 4 GiB of them awaiting removal; the storage ledger's
`session_scratch` usage gains `finished_runs` and `finished_run_bytes`.

`[command_temp] enabled = false` in `.aethyme/config.toml` (default `true`,
same trust rule as the other repository policies; an unreadable or invalid
file keeps the default and counts `command_temp_policy_invalid`) opts a
repository whose tools need the system temp folder out.

Counters `command_temp_created`, `command_temp_alias_created`,
`command_temp_alias_base_refused`, `command_temp_inherited`,
`command_temp_path_too_long`, `command_temp_create_failed`,
`command_temp_opted_out`, `command_temp_no_scratch`,
`command_temp_skipped_pressure`, `command_temp_alias_removed`,
`command_temp_alias_pruned`, `command_temp_removed_empty`,
`command_temp_finished`, `command_temp_finish_failed`,
`command_temp_kept_changed`, `command_temp_retirement_requested`,
`command_temp_retired`, `command_temp_missing_forgotten`,
`command_temp_kept_in_use`, `command_temp_kept_process_use_unknown`,
`command_temp_kept_move_failed`, `command_temp_kept_refused`; spans
`command_temp_prepare`, `command_temp_policy`, `command_temp_create`,
`command_temp_finish`, `command_temp_backlog_check`,
`command_temp_retire`, `command_temp_move`. The `session_scratch_retire`
attachment gains `command_temps_retired` and `command_temps_kept`; the
`session_scratch_usage` attachment gains `finished_runs` and
`finished_run_bytes`.

**Scratch pass order, cursor and failure causes.** The scratch pass first
classifies every record from its session's state, read for all records in
one store query (`SELECT id, status, cleanup_state FROM sessions WHERE id IN
(...)`, 500 ids per query; when that read fails, one lookup per record, counter
`session_scratch_retire_session_states_fallback`), with no filesystem access.
It then touches the filesystem only where there can be work, every step
within a budget of its own, because one directory operation on the busiest
repository's volume took from under 1 ms to about 1 s depending on what
else used the volume. The classification, at most 2 s from before its
first filesystem call (the container listing and the signal clear are
charged to it, unlike in 0.8.158), lists each pool's container, clears the
signals it accounts for, and lists by path the `tmp-done` container of each
open record due for a listing (a hint that only decides whether the
verified walk is worth its cost), at least one per pass, resuming after the
last record listed. A record is due when a finished-run signal names it,
when its last listing showed a finished run (it leaves only after a
listing, or the verified walk, showed none; a container that cannot be
read, other than one that does not exist, keeps it due, counter
`session_scratch_retire_listing_unknown`). The full listing lap, the
backstop for a marker whose signal is missing, starts at most once an hour
and never while the previous lap is unfinished (counter
`session_scratch_retire_full_listing`; `session_scratch_retire_lap_completed`
when it went round); it is paced to reach every open record within half an
hour: each pass lists, after where the lap stopped and in key order, its
share of the open records (the records the elapsed fraction of half an hour
owes, minus those already reached, at least 8), for at most 0.5 s (at least
one listing when it owes any). A record already due, or listed by the
classification, is reached without a listing of its own; one whose listing
shows a finished run is taken by this pass's finished-runs phase and stays
due; one whose listing cannot be read becomes due. In 0.8.158 a lap made
every open record due at once (about 600 on the busiest repository), so
every pass spent its whole budget listing 1 to 22 of them. Then a closed
(or replaced) record whose directory is proven gone (the walk from `/`
without links finds no entry, and the pool is not on a volume that is away)
is forgotten at once, with no open-file observation and no lifecycle claim
(counter `session_scratch_missing_forgotten_early`): probed in the closed
phase's order (from its cursor), at least one per pass, for at most 0.5 s.
A finished-run signal is the empty file
`<pool>/.aethyme-session-scratch/.aethyme-finished.<entry>`, created (never
following a link) right after the marker by whichever process finished the
run (counters `command_temp_finish_signaled`,
`command_temp_finish_signal_failed`); the pass reads the signals with the one
listing of each pool's container it already takes for unrecorded entries,
stores what they made due (only when there is a signal to clear), then
clears, through the container opened without links and before any listing, the signals of records stored as due,
of records it retires, and of entries no longer in the container (a signal
of a record it did not read or could not classify stays for the next pass) (when storing fails they stay, counter
`session_scratch_retire_signals_kept`). Signals are broker bookkeeping like
the container's marker: never unrecorded entries, never content that keeps a
container or a worktree root, and removed with an emptied container. It then
takes the run's open-file observation
outside its 5 s budget; when that observation failed, nothing is judged and
the phase cursors stay (what is due stays due). Otherwise it judges the closed (or replaced) directories
with half the budget while finished runs wait (all of it otherwise), then
the finished runs of open sessions until the budget's end, but never less
than half the budget after the closed phase. Each phase resumes after the
record where the previous pass stopped (broker metadata key
`session_scratch.retire_cursor`, JSON `closed` and
`command_temps`, each the last record key judged, absent once it went
round; `classify`, the last record listed; `listing_due`, the record keys
due for a listing; `full_listing_started_ms`, when the last full listing lap
started; `lap_open`, `lap_after` and `lap_listed`, whether a lap is in
progress, the last record it reached and how many it reached; the pass merges `listing_due` into the stored value rather than
replacing it, removing only the keys it proved need no listing and the
keys whose record is gone; and `command_temp_runs`, an object from record key to the name of
the last finished run judged when that record's visit ended before it
went through its runs). Every closed record is classified on every pass; a
finished run's record is due from the pass after its marker (within the
full listing lap without its signal: started hourly, paced over half an
hour, later only when its per-pass budget cannot keep that pace), listed within as many passes as there are due
records, and then judged like any record with finished runs, given passes
whose observation succeeds and whose moves do not fail with a cause that
affects the whole process (`too_many_open_files`). Records still due when
the budget ends count as more to do
only when the pass retired or forgot something. Finished runs are taken in
name order after the record's run cursor: a failed move that is not
systemic goes on to the record's next run, three failed moves in a row in
one record pass to the next record (the record is judged, its run cursor
kept past them), and the phase stops after three records in a row ended on
a failed move. The deadline, the move cap or a systemic failure that
affects the whole process (`too_many_open_files`) leaves the record
unjudged, so the next pass starts with it at its run cursor. A systemic
failure confined to one pool (`trash_open.*`, `trash_record.*`) or one
volume (`no_space`, `read_only`, `unsupported`) closes that pool, or every
record on that device, for the rest of the pass in both phases: the
failing record is judged (its run cursor before the failing run), the
other records there are passed (`skipped_closed_scope` in the
`session_scratch_retire` attachment, counter
`session_scratch_retire_skipped_closed`), and the phase goes on with the
other pools; all are retried on a later lap (counters
`session_scratch_retire_pool_closed`, `session_scratch_retire_volume_closed`).
Each phase attempts at most 256 moves. The pass is not cut short by what
the strip passes or the open-path resolution used of the lane's window, but
starts no new work later than 20 s past the lane's deadline: with one item
in flight (a replaced directory's fresh observation, 10 s, and one rename),
a retirement run ends at most 30 s plus one move past its deadline. Keeps
are written into `session_scratch.records` once per pass. A closed
session's directory that looks missing while its pool root is absent (or is
on another device without the scratch container: an unmounted volume) is
kept as `pool_unavailable` (counter `session_scratch_kept_pool_unavailable`),
never forgotten. An absent pool root counts as an unmounted volume only when
the nearest existing ancestor (looked at without following a link, climbing
only past `ENOENT`) is on another device than recorded; on the recorded
device the volume is mounted, the directory is really gone and the record
is forgotten (counters `session_scratch_pool_absent_volume_away` and
`session_scratch_pool_absent_volume_mounted`). The same rule applies when a
command asks for an open session's directory: on an unavailable volume the
record is kept and no fresh directory is made (counter
`session_scratch_unavailable_pool_unavailable`). Two known limits: after a
remount gives the volume a new device number, a record whose directory is
really gone is kept and judged again every lap (the ancestor's device no
longer matches, and nothing is left to re-identify); and a different disk
that reuses the recorded device number at the same mount path can make a
record be forgotten, which leaks that directory but never deletes
anything. Opening a directory on the recorded path refuses as
`not_a_real_directory` only for `ELOOP` and `ENOTDIR`; any other errno but
`ENOENT` is a failed move at step `open`. A failed move names its step (`open`, `inspect`, `trash_open`,
`trash_record`, `rename`) and cause (`too_many_open_files`, `no_space`,
`unsupported` for `ENOTSUP`/`EOPNOTSUPP`/`ENOSYS`/`EINVAL`, `denied`,
`read_only`, `not_found`, `exists`, `busy`, `io`, `crosses_device`,
`not_a_directory`, `interrupted`, `other`, `unknown`; `not_private` or
`foreign_marker` for a refused trash; `store` for a refused trash row).
There is no fallback to a non-exclusive rename. A phase stops after three
failed moves in a row (the finished-runs phase: three records in a row
ending on one, as above), or at once when the cause affects the whole
process (`too_many_open_files`); the other systemic causes (the trash or
its record, `no_space`, `read_only`, `unsupported`) close only their pool
or volume, as above. The next run retries: a failing move is tried at most once per run. A
stop after failures does not count as more to do, so it never keeps the
retirement run on its one-minute busy cadence.
Counters `session_scratch_retire_cursor_resumed`,
`session_scratch_retire_process_use_unknown`,
`session_scratch_retire_classify_stopped`,
`session_scratch_retire_stopped_after_failures`,
`command_temp_retire_stopped_after_failures`,
`command_temp_record_passed_after_failures` (a record passed after three
failed moves in a row), `command_temp_run_cursor_resumed` (a record's
visit resumed after its run cursor); spans
`session_scratch_retire_classify` (the whole classification),
`session_scratch_retire_session_states` (the store read),
`session_scratch_retire_signals` (the container listings),
`session_scratch_retire_signal_clear` (storing what signals made due and
clearing them), `session_scratch_retire_due_listing` and
`session_scratch_retire_lap_listing` (one per `tmp-done` listing, so the
span total over its phase count is the per-listing latency),
`session_scratch_retire_lap` (the lap's share), `session_scratch_retire_probes`
(the probe step) and `session_scratch_retire_probe` (one per probe); attachment `scratch_move_failures`,
keyed `<session_scratch|command_temp>.<step>.<cause>` (`count`,
`total_ms`, `max_ms`). The `session_scratch_retire` attachment gains
`records`, `sessions_with_finished_runs`, `kept_causes` (directories kept
by `<step>.<cause>`), `stopped_after_failures`, `classify_incomplete`
(records still due for a listing when the budget ended), `listed` (done
containers listed), `probed` (closed records probed for a gone directory),
`signals` (finished-run signals read), `lap_listed` (of `listed`, by the
lap), `lap_remaining` (open records the lap in progress has still to
reach), `listing_due` (records due after the
pass), `listing_unknown` (due records whose container could not be read), `full_listing` (the pass started a full listing lap),
`process_use_unknown` and
`command_temps_kept_by_reason` (`in_use`, `process_use_unknown`, a refusal
code, or `move_failed.<step>.<cause>`).

A command that keeps the inherited temp folder because a directory or link
could not be made (`command_temp_create_failed`) is also counted in the
attachment `command_temp_create_failures`, keyed `<step>.<cause>`: step
`live_container`, `run_dir`, `inspect` or `alias`; cause as above, or
`not_private`, `no_name`, `no_base`. Commands of one session creating the
containers at once never fail on that account (the `mkdir` that loses sees
`EEXIST` and checks the winner's directory).

**Session stacks and remote session-branch cleanup (schema v66).** All fields
below are additive and absent when they do not apply. `broker start --stack-on
<id> --json` adds `stack` (`session_id`, `parent_session_id`, `parent_branch`,
`parent_tip`, `base_branch`, `parent_state` = `open|merged|closed|abandoned`,
`last_parent_tip`, `parent_pr_number`, `parent_merged_head`,
`parent_merge_commit`, `retargeted_at`, `created_at`, `updated_at`,
`parent_pushed`). `broker sync --json` adds `stack_note`. The session view of
`broker status --json` adds `stack` (`parent`, `children`, `pull_request`,
`base_branch`, `retargeted_at`, `data: "recorded"`, and at most one
`next_action`); the compact repository view adds `session_stacks` (`count`,
`needs_attention`, `parents`, `records`) and `remote_branch_cleanup`
(`enabled`, `backlog`, `next_action`). `broker gc remote-branches plan --json`
and the `remote_branch_cleanup` field of `gc sweep --json` use schema
`aethyme.remote-branch-cleanup/v1`: `enabled`, `applied`, `repository`,
`backlog`, `scan_per_run`, `max_deletions_per_run`, `items` (`session_id`,
`branch`, `outcome` = `deleted|absent|kept|failed|would_delete|deferred`,
`reason`, `detail`, `pr_number`, `expected_oid`, `retargets`), `outcomes`,
`kept_by_reason`, `totals` (`deleted`, `absent`, `kept_by_reason`, `failed`,
`retargets`, `retargets_skipped`, `failures`), `skipped`, `stopped`,
`next_action`. A retarget's `outcome` is `retargeted`, `already_targeted`,
`skipped_moved (now based on <branch>)`, `not_attempted`, `failed` or
`planned`. `gc sweep --json` also adds `session_stack_failures` when a stacked session's parent
could not be re-read. Stack notes come from sender
`automation:session-stack`.


### Offline content-gate v2 comparison telemetry

The standalone `content-gate compare-v2` command emits `timings_ms` in its JSON result: `input_read_and_parse_ms`, `compare_ms`, and `total_ms`. Failed input parsing can omit phases not reached. These measurements contain no input paths or content. With `--telemetry` after the manifest path, the result also carries `coverage`: on a match, an object with aggregate counts `policy_gate_count` (resolved gates checked) and `policy_check_count` (item and collection checks expected for the candidate); on any failure, null. Without the flag the field is absent. The counts carry no gate IDs, entity IDs or content. This standalone command is not a broker gate and does not use the broker command recorder.


### Supervised content-gate v2 comparison telemetry

`content-gate run-v2` emits path- and content-free `timings_ms` fields `input_read_and_parse_ms`, `validator_ms`, `input_recheck_ms`, `compare_ms`, and `total_ms` in its standalone JSON output. Failed operations omit phases not reached. It is not a broker gate and does not use the broker command recorder.

### Hook dispatch telemetry

`hook_no_write_target` counts pre-tool events that have no recognized write
target and therefore skip repository discovery and coordination reads.

`hook_registered_snapshot` measures proof of the linked checkout registration,
bound repository identity, current-schema read-only database and exact session.
It can be recorded again for opt-in hook metric attribution.
`hook_post_recorded_view` counts post-tool boundaries served from that snapshot
while the session's own view is still fresh and no notes await delivery. Peer
lease and scope changes remain visible at these boundaries. An ambiguous
registration, due own-view check or unread note takes the full hook route.

### Exact batch verification

`broker submit batch <base> <session=head>... --session <coordinator> --json`
adds an explicit verification-only submit form. Ordered, distinct full
commit IDs identify the members and the locally fetched default. The
JSON result has `state`, `base`, `members` (with `session_id` and `head`),
`tree`, `candidate`, `conflicting_session`, `conflict_paths`,
`graph_advice` and `gates`. Only `verified` exits successfully and applies
to the candidate as a whole. `busy`, `superseded`, `conflict`, `deferred`,
`failed`, `no_candidate_changes`, `no_gate_configuration` and
`no_gates_triggered` are distinct non-success outcomes. Pre-admission,
ownership and execution errors keep the ordinary broker error contract.
Candidate objects are created locally with no ref or submission queue update.

Command metrics include `submit_batch_admission`, `submit_batch_compose`,
`submit_batch_checkout`, `submit_batch_verify`, and
`submit_batch_revalidate`. The `batch_verification` attachment contains the
outcome, member count, executed and cached gate counts, and executed gate
milliseconds. It contains no path, branch, commit or task information.

### Opportunistic queued batch verification

`broker submit queued <coordinator-id> --session <id> [--no-cache] --json`
opts a single submission into sharing the exact group gate when the ordinary
verification pool is already saturated. Only requests already waiting at slot
admission may join. The coordinator must be a member and every member names
that coordinator. Standalone submissions use the existing JSON shape.

A successful group reply carries `queued_batch` (`coordinator`, `verifier`,
`base`, `members`, `candidate`, `tree`, and `gates`), `landed: false`,
`next_action`, and `collaboration_capture`. The individual queue entries
record `superseded` with the combined candidate ID. This receipt does not
satisfy single-session verification or any provider check. Gate and input
details in command output are scoped to the opted-in group.

Command metrics include `submit_queued_batch_register`,
`submit_queued_batch_claim`, `submit_queued_batch_handoff`, and
`submit_queued_batch_follow`. Waiters retain the last immutable snapshot and
reload only when the state file changes, timed by
`submit_queued_batch_read_snapshot`. The `queued_batch` attachment counts admitted
members when known and records `role` (`leader`, `follower`, or `single`) and
`outcome` (`verified`, `fallback`, or `no_cohort`). Executed work appears once
on the verifier's `batch_verification` attachment.
