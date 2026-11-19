# Changelog

All notable user-visible changes to Aethyme are documented here. Release
artifacts and their exact source revision are recorded in each signed
`release-manifest.json`.

## [0.8.138] - 2026-11-19

### Changed

- `submit` hashes its freshly created verification checkout from that checkout's own index instead of from an empty one, so Git re-reads only files whose size, timestamps or identity changed rather than every tracked file. The shortcut is used only for the checkout `submit` has just created, and only when all of these are proven; otherwise the full hash runs as before: no entry is flagged assume-unchanged or skip-worktree; the index is exactly the commit's tree; no tracked path is subject to content conversion (line endings, filters, ident, working-tree encoding) and `core.autocrlf` is off; the index did not change while it was copied; and the copy kept the original's timestamp. Git's strictest stat comparison is forced for these commands. A session's own gate runs, the recheck after waiting for a shared verdict, native gates, semantic-patch preview and graph integrity keep the full hash (#1851).

### Added

- Spans inside every tree hash (`tree_hash_index_build`, `tree_hash_tracked_rehash`, `tree_hash_untracked_walk`, `tree_hash_untracked_add`, `tree_hash_write_tree`, and the `tree_hash_seed*` steps) and one per call site (`gate_inputs_tree_hash`, `gate_shared_wait_tree_hash`, `session_gate_tree_hash`, `native_gate_tree_hash`, `semantic_patch_tree_hash`, `graph_integrity_tree_hash`); attachment `tree_hash_files` with hashes, tracked entries and untracked files; counters `tree_hash_seeded` and `tree_hash_seed_fallback_<reason>` (#1851).

### Upgrade notes

- `AETHYME_TREE_HASH_SEED=off` forces the full hash everywhere.
- A repository whose Git configuration sets `core.autocrlf`, or whose tracked files carry converting attributes, always takes the full hash; `tree_hash_seed_fallback_content_conversion` counts it.

### Known issues

- The saving is not yet measured; the new spans report it.

## [0.8.137] - 2026-11-19

### Fixed

- `submit` no longer re-hashes every tracked file of the verification checkout to report a graph check that is turned off. When the repository's graph policy does not enforce committed fragments, the check returned `disabled` only after building a temporary index and hashing the whole freshly created checkout; on the busiest repository that was 3 to 8 minutes per submission before any gate started. The disabled outcome now carries the tree the checkout was created from, which `submit` already knows, and reads no files. An enforced policy, or an environment that refuses graph verification, takes the full verification as before. Gate selection is unchanged (#1849).

### Added

- Spans `submit_selection_policy_load`, `submit_graph_integrity` and `submit_gate_definitions_load`; counters `graph_integrity_disabled_known_tree` and `graph_integrity_full_verification` (#1849).

### Upgrade notes

- For a disabled graph check, the reported `tree_hash` is the merge candidate's tree rather than a hash of the checkout's files. They differ only where Git renormalizes content on add (line-ending conversion, a non-idempotent clean filter) or in a sparse checkout. Nothing reads the value for a decision.

### Known issues

- Gate execution still hashes the whole verification checkout once before the gates run.

## [0.8.136] - 2026-11-19

### Fixed

- `status` no longer scans every live lease to list overlapping sessions. The peer view joined the session's leases against all live leases with a test no index could serve; on the busiest repository that was about 17,000 leases and one second per call. It now searches the lease path index per path: the path itself, each directory prefix of it, and for a directory lease the range of paths under it. The rows returned are the same (#1844).
- While a host resource lease is quarantined, `status` no longer resolves the checkout path of every recorded session; a path that does not exist is skipped (#1844).
- Opening the broker no longer reads the eight sources of due background work when the background worker is already running. The open checks the worker's lock first and, if it is held, counts the request as coalesced as before. On the busiest repository the worker was already running in 416 of 418 measured opens, at about 57 ms each (#1842).

### Added

- Phase timings: one span per source of `blockers`, the steps of the session coordination and peer views, the steps `finish` runs under the lifecycle claim, and each Git worktree removal (#1844).
- Counters `worker_admission_skipped_live_worker` and `worker_admission_due_checks_run`; span `worker_admission_live_worker_probe` (#1842).

### Upgrade notes

- `coalesced_requests` now also counts opens that found the worker running before reading what was due.
- The main-checkout sync timer slot is claimed by the first open after the worker exits rather than while it runs.

### Known issues

- Nothing detects a background worker that hangs while holding its lock; opens count their request as coalesced and background work stops until that process exits.
- `finish` still removes the checkout while holding the lifecycle claim.

## [0.8.135] - 2026-11-19

### Fixed

- The trash catch-up process introduced in 0.8.134 gave up after about one second and deleted nothing. Every check of whether a catch-up was running took the host-wide lane lock for an instant and left a holder line naming no repository; a starting catch-up that met such a line ended as `lane_busy` instead of waiting. Checks now take the lock without writing anything and trust the holder line only while the lock is actually held, so a line left by a killed process or a reboot is ignored. A catch-up waits and retries every 45 s, within its 60-minute bound, for any holder other than its own repository's live catch-up. Exclusion is still the kernel lock (#1841).
- Stored "which files did this session change" answers are reused again by `agents` and `status`. On macOS all watched worktrees shared one file-event stream; a few worktrees installing dependencies made it report dropped events, and each drop marked every worktree changed, so on the busiest repository only 970 of 10,184 answers were reused in a day. Up to 128 worktrees now get their own stream, so a drop marks only that worktree; the rest share one stream as before. Commands that decide, such as `submit`, still always ask Git (#1839).
- A worktree that cannot be watched is retried with a backoff from 1 s to 4 minutes instead of every second (#1839).
- `submit` reuses a session's own gate results again. A change made earlier the same day added the comparison base to the identity of a cached gate result; `submit` passes a base and a session's own gate run does not, so the identities never matched and every gate the session had already passed ran again. The identity is back to tree, gate definition and execution profile, as documented; gates that read the comparison base must set `cache = false`, as before (#1846).

### Added

- Counters `changed_files_reused` and `changed_files_miss_<reason>`; watcher status fields `own_stream_cap`, `own_streams`, `shared_streams`, `watched_on_shared`, `stream_threads`, `rescans_scoped`, `watch_set_changes`, `watch_failures` (#1839).
- `checkout_trash_catchup` gains `lane_started_after_worker_ms` (#1841).

### Upgrade notes

- `checkout_trash_catchup` field `lane_held_by_other_repository` is replaced by its inverse, `lane_held_by_own_repository`.
- The watcher's `rescans` now counts only invalidations of every worktree; per-worktree ones are `rescans_scoped`.
- The file watcher holds one thread per own stream on macOS, at most 128. The new behaviour starts when the watcher's supervisor restarts.

### Known issues

- If a file-event stream stopped delivering events without reporting an error, a stored answer could be reused for up to 10 minutes by `agents` and `status`. This was already true of the shared stream.
- Other bulk removals inside the worker still run at the lowest disk priority.

## [0.8.134] - 2026-11-19

### Fixed

- The checkout trash is now actually deleted while other work uses the disk. The background worker runs at the lowest disk priority, and on macOS that priority delays each of its disk operations whenever anything else used the same disk recently. On the busiest repository the deleter managed 25 files per second while a plain removal on the same volume in the same minute ran several hundred times faster, so the trash grew to over a thousand entries. The trash catch-up now runs as its own process, started by the worker as soon as a deletable entry exists, at normal disk priority and reduced CPU priority. It keeps every existing bound: the rate limit (8,000, 4,000 or 200 unlinks per second by host pressure), the configured ceiling, the 250 ms debt cap, the yield at critical memory pressure, and a 20-minute lap, with up to three laps back to back. Below the storage headroom it is held to 8,000 unlinks per second instead of running unpaced. What is deleted is unchanged (#1837).
- The catch-up no longer waits for the worker's other lanes, which take 10 to 18 minutes on the busiest repository (#1837).
- One catch-up process per repository; repositories take turns on the host-wide lane, a waiting repository gets it after the holder's current lap, and a retirement run skips its own trash drain only while its own repository's catch-up is live (#1837).
- `finish` measures the checkout's size before taking the repository-wide lifecycle claim instead of while holding it. On the busiest repository that walk held the claim for about 37 s per finish (#1838).
- In the trash deleter, ancestor identities are re-checked every 256 unlinks or every 50 ms, whichever comes first, so a directory moved out of the trash mid-removal loses at most the unlink already under way. Catch-up byte figures are measured once per slice (#1833).

### Added

- `aethyme broker gc trash drain [--max-minutes <n>] [--json]` runs the trash catch-up now for the current repository, with the same guards and pacing, and reports entries removed, files per second and what remains. It refuses while another catch-up is live (#1837).
- Phase timings for `commit`, the checks before and after `exec`, worker admission on broker open, and the session `status` view. `commit` and `prepare` rows now carry the guarded command's `precheck`, `command_and_supervision` and `postcheck` (#1838).
- Metric rows `gc.__trash-catchup` and `gc.trash`, with `io_policy` (the policy read back from the system), `catchup_start_delay_ms`, laps and end reason; counter `checkout_trash_catchup_priority_not_raised` when the priority raise did not take (#1837).

### Upgrade notes

- The `checkout_trash_catchup` attachment moves from the worker's `gc` row to the catch-up process's own `gc.__trash-catchup` row.
- A repository that wants a quieter deleter sets `[cleanup] unattended_unlinks_per_second`; it applies in every mode of the catch-up process.

### Known issues

- Other bulk removals inside the worker still run at the lowest disk priority and can be starved the same way: the artifact cache sweep, checkout removal, compact-checkout removal and recovery-archive eviction.
- The manual `gc trash drain` runs at the caller's priority and is unpaced below the storage headroom.
- The checks before and after each `exec` are unchanged; the new timings show which step costs most.

## [0.8.133] - 2026-11-19

### Fixed

- The checkout trash deleter is faster and runs most of the time while the trash is behind. On the busiest repository, whose pool is on an external volume, the deleter was allowed 8,000 unlinks per second but reached a few hundred to 2,300: over half its time went to a status call before each unlink, and it ran about a quarter of wall time. The trash deleter now takes each entry's type from the directory listing and unlinks without a prior status call, falling back to one for an unknown type or a refused unlink. While the trash is behind, the worker keeps draining after its other lanes, with the repository's worker lock released: 30 s slices with a 1 s gap, for at most 20 minutes, three deleter threads sharing the one rate ceiling, one such lane per host. It still honours the configured ceiling, re-reads host pressure every 2 s and stops at critical memory pressure. Other removal paths are unchanged (#1831).

### Changed

- In the trash deleter only, ancestor identities are re-checked at every directory open, before every directory removal and every 256 unlinks, in place of before every unlink. All operations remain relative to pinned directory descriptors, links are never followed and mounts are never crossed (#1831).
- Trash drain byte figures (`bytes`, `stripped_bytes`, `scratch_bytes`, the strip ledger's `bytes_freed`) are now the volume's free-space change across each entry: approximate, and report-only (#1831).
- Telemetry: attachment `checkout_trash_catchup` (end reason, slices, entries, unlinks, `files_per_second_observed`, `worker_drain_share_pct`); `checkout_trash_drain` gains `threads` and `end_reason` (#1831).

### Known issues

- A directory moved out of the trash by hand while the deleter is inside it can lose up to 255 more of its own files at its new location before removal stops.
- Catch-up byte figures can be overstated up to three times, because three threads measure the same volume at once.
- The worker process can live up to 20 minutes longer while it catches up; its later lanes (cache measurement, transcripts, final status record) wait for it.

## [0.8.132] - 2026-11-19

### Fixed

- The checkout trash drain keeps up with what cleanup moves into it. Everything cleanup removes is first moved to the pool's trash and deleted later by the background worker; that deletion ran at 2,000 unlinks per second (200 whenever the one-minute load was above the core count) in whatever time a retirement run had left. On the busiest repository 7 entries were deleted while 64 arrived, and the trash reached 736 entries, the oldest 16 days old, so removed checkouts and dependencies never gave their space back. Now, when the trash is behind (16 or more deletable entries, or one a day old), the deleter runs at up to 8,000 unlinks per second on a quiet host, 4,000 under load, I/O pressure or a memory warning, and 200 under critical memory pressure, re-reading host pressure every 2 s. The drain has its own share of each run (a third, half when behind). The rate is a ceiling over short intervals: after a stall at most 250 ms of work goes through unpaced. What is deleted is unchanged: recorded trash entries only, one deleter per host, no links followed (#1829).
- An entry that failed to delete is tried after the others; an entry still in use is passed over, retried on later runs, and no longer keeps the trash counted as behind (#1829).
- The recovery archive size pass runs before the trash drain, so it is no longer left without time on a busy repository (#1829).

### Added

- `[cleanup] unattended_unlinks_per_second`, when a repository sets it explicitly, is a ceiling for the trash deleter's paced modes. Left unset, the rates above apply. Below the storage headroom the deleter stays unpaced, as before (#1829).
- Advice `cleanup.trash-backlog` when the trash holds an entry a day old or has grown for three runs in a row. Telemetry attachment `checkout_trash_drain` reports backlog, oldest age, pace, observed rate, entries and unlinks removed, and entries passed over by reason (#1829).

### Upgrade notes

- Consumers of the `checkout_trash_drain` attachment read `initial_pace` and `initial_unlinks_per_second` in place of `pace` and `unlinks_per_second`.
- A repository that wants a quieter deleter sets `[cleanup] unattended_unlinks_per_second`.

### Known issues

- A worker run can last longer than before while the trash is behind: the archive pass and the drain may each extend it by their own bounded budgets.
- The deleter is single-threaded and re-checks each ancestor directory before each unlink; the catch-up rates may not be reached on a slow volume. `unlinks_per_second_observed` shows the real rate.

## [0.8.131] - 2026-11-19

### Fixed

- The idle strip re-examines sessions it handled under older rules. A handled session was remembered until it became active again, so a session stripped by an earlier version never got the removals a later version added. The memo now records the rules it was written under (`checkout_strip.idle_done_rules`); when they change, each idle session is examined once more (`checkout_idle_strip_memo_outdated`) (#1826).
- Leaked artifact-use markers no longer keep an idle checkout busy forever. On the busiest repository, markers left weeks earlier by commands whose processes were gone refused the idle strip on almost half of the idle sessions. The idle strip now passes a marker only when its producer is proven exited: running phase, recorded process identity and start token, process absent or its identity reused (observed twice), process group absent. A live or unprovable marker refuses as `checkout_marker_live`; `checkout_busy` now means the checkout lock itself is held. The lock is still taken and held across proof and removal. Markers are not removed (#1827).
- An idle session whose `git status` cannot finish in time is no longer always kept as `activity_unknown`. A bounded walk of file modification times decides instead (at most 5 s and 200,000 entries, inside the pass deadline): a recent time means active, a finished walk with none means idle, an unfinished walk keeps the session. The walk leaves out only `.git`, the broker's runtime paths, and directories the strip itself accepts as regenerable (#1827).

### Changed

- The counter for an environment kept because preparation is not proven to create it is renamed from `checkout_idle_strip_refused_environment_root_creation_unproven` to `checkout_idle_strip_kept_environment_root_creation_unproven`: it counts kept directories, not refused sessions (#1826).

### Upgrade notes

- Consumers of the renamed counter must read the new name.
- After upgrading, every idle session is examined once more over the following worker runs.

### Known issues

- An idle session holding an operation left `running` by a process that is gone is still refused as `operation_unresolved`.
- Leaked artifact-use markers of open sessions are passed over but not removed.
- A producer on another host or in another process namespace sharing the volume is not visible to the exited-producer proof.

## [0.8.130] - 2026-11-18

### Changed

- The idle strip removes more from idle open sessions. Besides directories a preparation step declares exactly, it now removes: a Python environment whose interpreter a step declares as an output (`<root>/pyvenv.cfg`, `<root>/bin/python*` or `<root>/Scripts/python*`), when it passes the same check the closed-checkout strip uses; and pnpm workspace-member `node_modules` directories proven regenerated by the declared root install, only once the root `node_modules` has moved in the same pass or was already absent. An environment with other declared files but no declared interpreter is kept as `environment_root_creation_unproven`; members whose root stays are kept as `member_root_kept`. `aethyme broker submit prepare --session <id>` runs every step again after a strip and recreates what was removed (#1820).
- The `checkout_idle_strip` telemetry attachment gains `by_kind`: removed and kept counts and bytes per kind (`node_modules`, `node_modules_member`, `venv`, `pnpm_store`, `cargo_target`), measured only for directories being judged, within 2 s per checkout and 6 s per pass (#1820).

### Upgrade notes

- For an idle session's Python environment to be removed, the preparation step that creates it must list the interpreter among its `outputs`, for example `"<dir>/.venv/bin/python"`. Declaring only tools installed into the environment is not enough.

### Known issues

- A repository whose preparation config does not declare the environment's interpreter keeps that environment on idle sessions.

## [0.8.129] - 2026-11-18

### Fixed

- The recovery archive size bound is enforced even when one archive is slow to verify. Removals get their own time budget (60 s), separate from listing and sizing (20 s). An archive whose verification times out or fails is passed over by later runs and retried on its own after an hour, so it no longer blocks the archives behind it. A removal already proven may run up to 30 s past the budget, so it is never left half done (#1817).
- The archive pass judges whether each archive's work landed at most once per run, and stops those checks at its deadline (`landing_unjudged`). Before, an over-bound repository repeated them for every measured archive with no deadline (#1817).
- A failed archive size measurement is no longer remembered as 0 bytes; it is counted `size_failed` and measured again. Size walks stop at the deadline and are retried next run. Archives the broker does not own are measured fresh every run (#1817).
- An archive listing that reaches its time budget resumes after the last archive it listed, instead of always starting from the first (#1817).

### Known issues

- The removal budget is capped by the retirement run's own deadline: a run that has used its time before the archive pass removes nothing in that run.
- An archive that can never be verified (altered content, or too large for the budget) is retried once an hour and never removed for size.
- On the busiest repository most idle sessions keep their dependency directories because those directories are not declared preparation outputs (`not_declared_output`).

## [0.8.128] - 2026-11-18

### Fixed

- The background cleanup worker no longer stalls on recovery archives. In 0.8.127 every retirement run fully walked and hashed every recovery archive, outside the pass's time budget; on the busiest repository one run exceeded 50 minutes and no checkout was stripped, retired or archived meanwhile. The archive size pass now classifies archives from their manifests, keeps measured sizes between runs, stops at its budget, and fully verifies only the one archive it is about to remove. Restore, `gc apply` and `gc plan --full-scan` still verify fully, and an archive holding unlanded work is still never removed for size (#1816).
- Setting `[cleanup] delete_merged_session_branches` no longer makes the cleanup policy fail to load. Before, a repository that set this documented key lost automatic checkout removal and the ignored-output sweep (#1813).
- `aethyme certify` no longer warns "unknown key(s)" on `[cleanup]`, `[main_checkout]`, `[coordination]`, `[console]` and `[watch]` (#1813).

### Changed

- The background worker reads `[cleanup]` from the remote default branch as last fetched (`origin/HEAD` only) when the primary checkout is simply behind it and its config has no local edit. A locally edited, diverged, non-regular or unreadable config, a missing `origin/HEAD`, and a default-branch config that does not validate all keep the working copy, as before. A config edit made during a worker run takes effect for the rest of that run. `broker status` shows advice `cleanup.config-source` when the two copies differ, naming which one the worker follows. Foreground commands and previews still read the working copy (#1813).

### Upgrade notes

- A repository whose primary checkout is held behind its default branch now gets `[cleanup]` settings merged on the default branch. If `origin/HEAD` is not set, run `git remote set-head origin --auto`.

### Known issues

- A landed archive whose full verification cannot finish inside the archive pass's budget is not removed, and archives behind it are not tried in that run; it is counted `recovery_archive_bound_evict_failed`. Nothing is lost; the size bound is not enforced for that repository until this is fixed.
- When a repository is over the archive bound, the landing check runs for every measured archive without the pass deadline.
- `certify` checks `[cleanup]` key names only; an invalid value still certifies.

## [0.8.127] - 2026-11-18

### Added

- Stacked sessions. `broker start --stack-on <session>` starts a session on top of another session's branch; its pull request is based on the parent's branch and says "Stacked on #N". When the parent's pull request merges, the child is retargeted to the nearest unmerged ancestor, only from the base the broker expected, and a failed lookup aborts the retarget. `broker sync --unstack` detaches a child. Schema 66 adds `session_stacks` (#1796).
- Merged remote session branches can be deleted: `[remote_branches] delete_merged = true`. Off by default. A branch is deleted only when its pull request is merged, no open pull request is based on it, no session still uses it, and the remote tip is still the merged head; each fact is read again before each delete, which is a `--force-with-lease` push pinned to that head. At most 25 per run. `gc remote-branches plan` lists what a run would delete without deleting (#1796).
- Idle open sessions lose their regenerable dependency directories after `[cleanup] idle_strip_after_hours` (default 4). Only directories that a declared preparation step output names exactly are removed; anything else is kept and counted `not_declared_output`. The session gets a notice in `.aethyme/broker-action-required.md` (never overwriting another notice, never written to a tracked path), a session-scoped blocker, and advice `idle_strip_prepare`; `aethyme broker submit prepare --session <id>` rebuilds what was removed. A session is not idle while a process uses its checkout (observed again when the observation is older than 5 s), while files Git reports as changed or untracked were modified inside the window, or while its checkout lock is held; when that check cannot complete the session is kept as `activity_unknown` (#1790, #1794).
- Idle sessions are archived, then removed. A session with no activity for 24 hours (4 hours when its work already landed) has its commits, patches, untracked files and protected ignored files archived with a SHA-256 manifest, and its checkout replaced by a small tombstone directory holding `ARCHIVED-SESSION.txt`. `aethyme broker start --reuse '<path>'` restores it: the checkout is rebuilt at a staging path and moved into place only when complete, a restore onto a branch that moved is reported, and the session gets the same rebuild notice as an idle strip. A closing-soon notice is raised 4 hours before. `--json` refusals of a closed session that left a tombstone carry `error.action` (#1794).
- Recovery archives live next to the checkout pool, in `<pool>/.aethyme-recovery-archives/<repository key>`; existing archives in host state are still read and never migrated. `[cleanup] recovery_archive_bound_gib` (default 20) removes the oldest archives whose content is fully landed; an archive holding unlanded work, an older-format archive, and one with a keep marker are never removed for size. Unlanded archives are kept past their retention age and named in advice until `storage keep-archive --dismiss` or `[budget] expire_unlanded_archives`. `storage status` gains `recovery_archives_by_location` (#1794).
- `push` and `sync` print one `Next:` line with the prepare command while a session's dependencies are missing after an idle strip or a restore; `--json` carries `preparation_next_action` (#1794).
- Blocker scope `session`: other sessions' hooks do not count it (#1790).

### Upgrade notes

- Consumers of `broker blockers --json` that match the scope exhaustively must accept `session`.
- Remote branch deletion stays off until `[remote_branches] delete_merged = true` is set. Run `gc remote-branches plan` first.
- An agent returning to a session idle for more than 24 hours finds a tombstone directory in place of its checkout; `aethyme broker start --reuse '<path>'` restores it. Restore does not bring back file modes beyond the executable bit, timestamps, links or extended attributes.
- A pool root whose repository was moved or renamed is not removed while it holds a recovery archive not proven landed.
- An agent returning to a session idle for more than 4 hours finds its dependencies gone and a blocker naming the one command that rebuilds them. Set `[cleanup] idle_strip_after_hours` to change the window.

### Known issues

- The remote-branch cleanup lock is taken for 20 minutes and not renewed; a run is a single pass well inside that.
- `submit`, auto-close and the landed check do not yet treat a stacked child as depending on its parent.
- Archives holding unlanded work have no total size limit; they are kept and named in advice.
- Catalogued regenerable directories (Python environments, build output, test caches) are not archived and are not restored; the rebuild command recreates them.
- A repository that sets `session_abandoned_after_hours` itself keeps its own window.
- `[cleanup]` settings are read from the primary checkout's working copy. On a repository whose primary checkout is held behind its default branch, a setting merged on the default branch is not in effect.

## [0.8.126] - 2026-11-18

### Added

- Dependency bytes are shared between session checkouts. After a repository-shared preparation succeeds, each file of 64 KiB or more inside a declared output's ignored directory is replaced by a copy-on-write clone from a content-addressed pool in the repository's package store, keeping its mode and timestamps. A file enters the pool only after its clone hashes to its name, and a pool entry is verified before its first use in a pass; one that fails is removed and the file keeps its own bytes. Clone-only: volumes that cannot clone keep today's behaviour and report `skipped`. On the busiest repository a prepared checkout owned about 575 MB alone, almost all of it one Python environment; the target is 250 MB. `submit prepare status --json` gains `package_store.output_share` and `package_store.cache_root_placement` (#1792).
- Fully merged local session branches are deleted by the background worker: every session that used the branch is cleaned, its tip is an ancestor of the remote default branch as last fetched, no worktree has it checked out (including a bisect that will return to it), no start reservation is held on its sessions' paths, and nothing restorable remains. Sessions, restorable state and worktree registrations are read again under the lifecycle lock immediately before each compare-and-swap delete. At most 200 branches judged and 100 deleted per run. `[cleanup] delete_merged_session_branches = false` turns it off. Advice `cleanup.merged-session-branches`. Local refs only: nothing is fetched, pruned or deleted on the remote (#1793, #1797).
- Every session gets a scratch directory next to its checkout pool, outside the worktree, for build output and temporary files. `broker start --json` and the session view of `status --json` carry `scratch_dir`; commands the broker runs for a session get `AETHYME_SESSION_SCRATCH_DIR`. It is moved to the pool's trash when the session is closed or replaced by `start --reuse`, never while a process has its working directory or an open file inside, and only when it is the directory the broker recorded for that session. After a remount it is re-identified by its marker, name and inode. Storage class `session_scratch`; advice `storage.session-scratch-large` and `storage.session-scratch-unidentified`. The temp-directory variables are not changed (#1795).

### Changed

- **Breaking:** `[shared_cache] root` defaults to `auto`: the host root when it is on the checkout's volume, the checkout's volume otherwise. Upgrade note: on a host whose cache root and checkouts are on different volumes, each checkout prepares once from a cold store; the old store is not deleted at upgrade and is retired a week after its last use. Set `root = "host"` to keep the old placement (#1792).
- A shared package store unused for a week is retired by the storage sweep, under the exclusive cache lease (#1792).
- The main-checkout sync no longer holds on the tool's own generated files. A locally modified, unstaged file is replaced only when its bytes are proven generator output: equal to the incoming update, an agent-instruction file whose recorded hash still matches its body, a skill copy equal to this binary's render, or bytes committed at that path within its last 200 commits. The repository's `.aethyme/config.toml` qualifies only when it already equals the incoming update. Each replaced file is first backed up under `.aethyme/run/main-checkout-replaced/`, verified against the proven content, and put back if the move then refuses; backups are kept 7 days, at most 20 sets. Edited, staged or unreadable files still hold the sync and are named with the reason. `deploy` warns when it leaves generated files modified in a primary checkout (#1791).

### Known issues

- Retiring the shared package store under space pressure frees little while live checkouts hold clones of it, and the next checkout gets full new copies.
- A package manager configured to read files in place from the shared store, instead of cloning or linking them, loses files when the store is retired.
- A merged branch whose pull request was squashed or rebased is not recognised as merged and is kept.
- Agents use the scratch directory only when they follow the instruction to; the system temp folder is not redirected.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.

## [0.8.125] - 2026-11-18

### Fixed

- Closed and cleaned checkouts were kept whenever their recorded holder process was still alive, even when that process had long since started working on another session. On the busiest repository 380 of 503 judged checkouts were refused this way in under an hour, held by two dozen long-lived agent processes. A live holder (same pid and start time) no longer keeps a closed checkout once the same process was bound to a newer session after this session's last activity and close. One shared rule serves the strip pass, its re-check under the lifecycle claim, the cleaned-leftover lap and the abandoned-operation archive. A process with its working directory or an open file inside the checkout, an unknown process listing, and a shared agent service still keep it (#1787).
- The stale-session pass stopped at the first session whose landed proof used up the pass's budget, so every session after it went unjudged. The cursor still stays on that session, but the rest of the page now gets its abandonment judgement, without landed proofs. The close-after windows, the closing-soon notice and the fresh working-tree check before a close are unchanged (#1787).
- Reclaim plan snapshots at the pool root were pruned only when someone ran `gc reclaim plan`. Every checkout retirement run now prunes them with the same bounds: at most 256 per run, older than 7 days, or older than a day with the scope gone (#1787).

### Added

- Counters `checkout_holder_moved_on`, `retirement_abandonment_judged_past_spent_landed_budget` and `reclaim_snapshots_pruned_by_worker` (#1787).

### Known issues

- A session whose landed proof runs out of budget on every pass still pins the stale pass's cursor, so sessions on later pages get no landed proof.
- Checkouts whose holder cannot be determined (`holder_unknown`) are still kept.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.

## [0.8.124] - 2026-11-18

### Fixed

- The strip pass spent most of each pass on session records whose folder no longer exists. A pool directory that is gone lists as empty; a folder is remembered as absent only on a "not found" answer while its pool directory can be listed, and the memory is dropped when the folder is seen again. Any other read failure is reported as `checkout_unreadable` and never remembered. Each pool listing has its own budget, and an entry vanishing mid-listing no longer discards the listing (#1785).
- Nested `node_modules` of workspace members were refused for a missing package-manager marker. One is now stripped only when every entry is a symlink, a `.bin` directory whose shims also exist in the checkout's top-level `node_modules/.bin`, or a scope directory of symlinks, and at least one link points into the package store. The top-level `node_modules` still needs its own marker; any other content keeps the folder (#1785).
- Retirement runs spent 140 to 457 seconds expiring retired-commit pins, scanning every remote ref once per pin. Pin expiry has a 20-second budget per run and checks the cheapest evidence first. Both laps read folder existence from the pool listings, and a `leftover_superseded` keep is remembered until the superseding session changes (#1785).
- Stale-session classification reads refs first and inspects the working tree only when refs alone class the session below unpushed. A remembered clean answer orders work and can say "not closable yet"; the close decision always inspects the working tree fresh. The close-after windows are unchanged (#1785).
- `gc reclaim plan` no longer leaves one snapshot file per plan at the pool root for ever: each save prunes up to 256 snapshots older than 7 days, or older than a day whose scope is gone (#1785).

### Added

- Keep code `checkout_unreadable` and the `checkout_strip.absent` memo (#1785).

### Known issues

- The landed lane's cleanliness proof still times out on a slow volume.
- Once a checkout's top-level `node_modules` is stripped, nested members whose `.bin` holds files are kept.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.

## [0.8.123] - 2026-11-18

### Fixed

- The strip pass judged no checkout on a host whose pool volume answers metadata calls slowly (about 120 ms each while a deleter runs): it checked every pool folder by name before starting and spent its whole budget there. Candidates now come from the session records, oldest close first, resumed from a persisted cursor; folder existence comes from one listing by entry type capped at 10 seconds, and otherwise is checked when the checkout is judged; each checkout gets at most 15 seconds; the backlog count runs only after 10 checkouts were judged (#1783).
- The closed and cleaned retirement laps were starved by resolving open paths one after another. Open paths use at most a third of the run and are resolved on up to 8 threads; the strip pass, the closed lap and the cleaned lap share the rest with floors of 20, 20 and 10 seconds. An incomplete open-path index is never used (#1783).
- `finish` no longer refuses to strip its own checkout because the finishing agent is recorded as its holder. Any other live holder, and any process with its working directory or an open file inside, still keeps it (#1783).
- The main-checkout sync never moved a primary checkout that had an idle session adopted on it. Such a session now holds the move only while in use: a live spawned process or recorded holder, a process listing that cannot prove the holder gone, an unresolved coordinated operation, or activity in the last 30 minutes. After a move each open adopted session gets a note naming the two commits, and its diff base follows when the old base is in the default branch's history (#1782).

### Added

- `blocking_session_id` in the `main_checkout` record; advice `main-checkout.behind` names the session, why it is in use and its last activity, with the commands to inspect or finish it. The session view of `status --json` carries `main_checkout` and `main_checkout_advice` (#1782).
- Spans for the retirement run's tail (stale partials, retired pins, progress write) and for the strip cursor (#1783).

### Known issues

- With many sessions, abandonment judging and the landed lane's cleanliness proof still time out on a slow volume; the deleter is not yet paced by observed metadata latency.
- A primary checkout whose local changes overlap upstream changes stays behind until those changes are committed, reverted or moved; `status` names the paths.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.

## [0.8.122] - 2026-11-17

### Fixed

- The strip and leftover-retirement lanes added in 0.8.120 reclaimed almost nothing on a busy host. Each pass first counted its backlog with one file-system check per recorded session, which took over 99% of the pass, so discovery never ran. A pass now lists each pool folder once, judges each folder that exists once, and counts the backlog afterwards under its own 5-second budget (#1774).
- Regenerable directories are found by a bounded listing two levels down instead of a Git listing per checkout, then proved with two batched Git calls per checkout. Every earlier proof still applies to each candidate before any move: ignored, no tracked file, same device, no symlink component, witness, nested repository kept. A failed probe refuses every candidate of that checkout (#1774).
- A reused path is judged by one rule: a cleaned session judges a folder only when every session at that path is cleaned; a closed session only when no newer open or closed session uses the path. The closed lap applies it before planning (#1774).
- A checkout kept for a transient reason (`probe_failed`, `deadline`, `move_failed`) is no longer remembered as done. It is retried after 5 minutes, doubling to at most 4 hours, and does not count toward the per-pass limit while it waits (#1774).

### Added

- `[cleanup] venv_markers` in `.aethyme/config.toml`: exact file names a repository's tooling writes inside its Python environments (at most 16, no path separator or glob; `.env`, `.env.*`, `.envrc`, `.netrc`, `.npmrc` and `.pypirc` are never accepted). A `.venv` is stripped only when it holds the usual layout, files environment tools write themselves (`CACHEDIR.TAG`, `.gitignore`, `.lock`) and declared markers; anything else keeps it as `unexpected_content`. Advice `storage.venv-unexpected-content` names the file names seen, never their content, and the key to declare them (#1774).
- Counters for every strip outcome by reason, timings for discovery, proof and move, a per-pass `checkout_strip` summary, and `checkout_strip_skipped_backoff`, `checkout_strip_backoff_set` and `checkout_strip_backoff_cleared` (#1774).

- The repository's primary checkout is kept fast-forwarded to its remote default branch. The background worker attempts once per `[main_checkout] sync_interval_minutes` (15) per repository, with one bounded fetch of the default branch only (explicit refspec, no tags, no prune, no hooks, no credential prompt). It moves only on the default branch, strictly behind, with no local commits, no operation in progress and no sparse, assume-unchanged or skip-worktree entries; unrelated local changes stay in place. It holds, naming the paths, when an upstream change overlaps a locally changed path or an untracked file (compared case-insensitively), when a smudge or process filter applies to a changed path unless `allow_filters = true`, or while a session is adopted on the checkout. Each step writes the files and index first and moves the branch last by compare-and-swap; a marker under `.aethyme/run/` lets the next attempt finish or undo an interrupted step, and anything it cannot prove holds with advice `main-checkout.interrupted`. Jumps over 2,000 changed paths advance in steps along the first-parent line. No repository hooks run. `[main_checkout] auto_sync = false` turns it off. Advice `main-checkout.behind`, a `main_checkout` block in `status --json`, and one line on `start` (#1775).
- The broker's own leftovers in the primary checkout's `.aethyme/` (staged `gc.lock` files and completed maintenance-hold directories) no longer count as local changes; the worker removes a lock file over an hour old whose process is gone and a hold directory a day after its manifest says complete, and keeps anything else (#1775).

### Changed

- **Breaking:** the primary checkout now moves by itself as described above. Before this release it was fast-forwarded only before a few commands run from that checkout, never fetched, and refused on any local change. Upgrade note: set `[main_checkout] auto_sync = false` to keep the old behaviour. Foreground commands attempt before the same commands as before; `status` moves only a checkout with no local changes (#1775).
- Upgrade note: a repository whose tooling drops its own marker file inside `.venv` keeps those environments on disk until it declares the name under `[cleanup] venv_markers`; `aethyme broker status` reports the names to declare.

### Known issues

- Checkouts refused at the trash step for a failed environment-database teardown or a ref lock are still remembered instead of retried.
- `closed_with_regenerable` in the backlog count still looks only at top-level directories.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.

## [0.8.121] - 2026-11-17

### Added

- Stale sessions are closed by the background worker. Foreground commands only count due sessions and queue one detached worker; the worker closes a session past the abandonment window by work class, only with no live or unknown holder and a complete process listing: landed, merged or no commits after `[cleanup] stale_landed_close_after_hours` (24); pushed but unmerged after `stale_pushed_close_after_hours` (72), branch and pull request untouched; unpushed commits or uncommitted work after `stale_unpushed_close_after_hours` (168), through the verified archive path. A session holding work closes only after a closing-soon note has stood for `stale_closing_notice_hours` (24), counted from when the note was left; activity after the note voids it and a failed note write keeps the session. The class is read from the session branch as well as HEAD and the more protective one wins. A holder process that has since bound to a newer session no longer counts as holding the older one. `[cleanup] stale_session_auto_close = false` turns this off. Advice `sessions.stale-backlog`, `sessions.closing-soon` and `sessions.process-table-denied`. A closed session resumes with `broker start --reuse` (#1768).
- One package store per repository, shared across input sets, instead of one per input digest. `gc` and the storage recovery worker rotate it without a preparation when its volume is under the free-space reserve or it exceeds `[shared_cache] max_bytes`, under the exclusive cache lease only (#1765).
- A host-wide cap on concurrent preparation installs: every `submit prepare` holds one unit of a host pool while its steps run, served in arrival order. `[installs] max_concurrent` (1 to 64) in the host's `preparation.toml`; the default is half the CPU count or one install per 4 GiB of memory, whichever is smaller, persisted and reused. The free-space reserve is re-checked after the grant. The prepare report gains `install_slot` (#1771).
- Advice `prepare.runtime-probe-unverified` and `prepare.runtime-probe-unstable` (#1765).

### Changed

- **Breaking:** the preparation key hashes each declared runtime probe's output instead of the executable's size and modification time. Upgrade note: every prepared checkout reports stale once and re-prepares. A probe that fails, times out or cannot start is `unverified`: the checkout never reports current, and a step marked `required_for_hooks` then blocks commit hooks until the probe is fixed; before this release a broken probe was never run (#1765).
- **Breaking:** `[shared_cache] max_bytes = 0` means an 8 GiB cap instead of unbounded (#1765).
- `broker start` holds the repository lifecycle lock only to reserve its destination; `git worktree add` runs outside it and the nested-path check no longer lists worktrees. A path recorded on any session row is never handed out again (schema v65 adds one index; older binaries are unaffected). Every checkout removal refuses a path a running start has reserved. Starts in progress count against the free-space reserve and the checkout limit (#1767).
- Upgrade note: repositories that set nothing now have stale sessions closed automatically as above; sessions with uncommitted work whose holder moved on were previously kept indefinitely (#1768).

### Fixed

- Probe output is never stored raw: only its hash and a short version-shaped extract are recorded (#1765).

### Known issues

- On a busy host the strip and leftover-retirement lanes added in 0.8.120 reclaim very little: discovery is slow per checkout, nested dependency directories are not found, and a leftover folder is kept whenever its path has a newer session row. A fix is in progress.
- `start --adopt` can still hold the lifecycle lock for minutes while comparing patches.
- Sessions held by `artifact_producer_active` are not closed by the new lane.

## [0.8.120] - 2026-11-17

### Fixed

- Closed checkouts no longer pile up on a busy host. Each retirement run first moves the catalogued regenerable directories (`node_modules`, `target`, `.venv`, `.pnpm-store`) out of closed and cleaned checkouts into the pool's trash, oldest close first, at most 200 checkouts per run; `finish` strips its own checkout after its existing reclaim. A directory is stripped only when it is ignored, holds no tracked file, sits on the checkout's device with no symlink on the way, and carries its witness: a package-manager marker for `node_modules`, the store layout for `.pnpm-store`, `pyvenv.cfg` and only the usual top-level entries for `.venv`, a `CACHEDIR.TAG` with the standard signature for `target`. Anything else, and any directory with a nested repository, is kept with a recorded reason. `build` and `dist` are never stripped (#1764).
- Leftover folders of sessions already recorded as cleaned are retired through the careful path, oldest first, in a second lap of each run (#1764).
- A checkout is neither stripped nor retired when, under the lifecycle claim, another open or newer session uses its path, its holder is alive or unknown, the folder was created after the session closed, or a `start` holds a reservation on it (#1764).
- The cleanup paths list worktrees once per run instead of once per checkout; a worktree lock is read fresh from Git's admin directory. Inspection timeouts are retried on the next run instead of being remembered as a keep (#1764).
- `broker start` below the free-space reserve strips closed checkouts and drains the trash for up to 30 seconds before refusing, without holding the lifecycle claim through the drain (#1764).
- A Git timeout names its source (per-call limit, the command's outer deadline, or not started), the subcommand, the directory, the limit and the elapsed time, and never says "killed" for a command that was not started. `finish` keeps its bounded-retry guidance for both wordings (#1766).
- A transcript archive pass no longer takes one host-wide open-file listing per transcript (72% of a measured 3.4-hour pass). One shared observation, renewed every 30 seconds, decides what is compressed; originals are removed in batches of at most 200 copies or 60 seconds, each batch against one fresh observation taken after its last copy was verified, and only when the source is not open and its size, modification time and identity are unchanged. An incomplete observation removes nothing and discards the batch's copies. Verification paces the compressed bytes it reads; it still decodes and hashes the whole archive before any removal (#1769).

### Added

- Transcript counters `open_observation_refreshes`, `open_files_judged`, `removal_batches`, `removals_refused_open`, `removals_refused_unobserved` and `removals_refused_changed`, and phase `transcripts_remove` (#1769).

- Storage ledger class `checkouts`, advice `storage.closed-checkouts-backlog`, and per-day counters `sessions_created`, `sessions_closed`, `checkouts_retired` and `cleaned_leftovers_retired`; the retirement report gains `cleaned_lap_complete` and `retired_cleaned_leftovers`. Checkout counts are written atomically, one file per repository (#1764).
- Counters `git_timeout_per_call`, `git_timeout_outer_deadline` and `git_timeout_not_started` (#1766).

### Changed

- Upgrade note: after upgrading, closed checkouts lose their `node_modules`, `target`, `.venv` and `.pnpm-store`; reopening one with `broker start --reuse` needs `aethyme broker submit prepare` again. Stripped directories stay in the pool's trash until the deleter drains them.

### Known issues

- `git worktree add` in `start` and the directory removal in `finish` still run under the repository-wide lifecycle lock; `start --adopt` can hold it for minutes while comparing patches.
- Stale open sessions are still not closed automatically; a fix is in review.
- The structured form of a Git timeout is not yet carried in any `--json` output.

## [0.8.119] - 2026-11-17

### Changed

- **Breaking:** transcript archiving is on by default. With no settings file, or one that does not set `archive`, closed Codex and Claude Code transcripts are compressed and moved once they have had no activity for `archive_after_inactive_days` (default 3) or `archive_after_inactive_days_claude` (default 14). Activity is the newest of the file's modification time, the newest record near its end, and the last broker activity of any linked session. The old setting names are still read. On a host that never opted in, nothing is archived until 24 hours after the notice `transcripts.archiving-default-on` first appears in `aethyme broker status`, `broker start` or `aethyme transcripts status`. Upgrade note: run `aethyme transcripts disable` to keep transcripts untouched, or `aethyme transcripts enable --after-inactive-days <n>` to choose the age (#1762).

### Fixed

- A coordinated GitHub or Git write whose outcome could not be recorded because the broker database was busy no longer write-blocks the repository. The child's result is journaled under `.aethyme/run/operation-outcomes/` before it is recorded, the record is retried for up to 60 seconds after the coordination lock is released, and any later broker command replays a pending journal before its write-block check. A journal settles only the run that wrote it (run nonce, creation time, session, command and process must match); refused and conflicting journals are kept and reported in `broker status`. An operation with no journal still needs `operations reconcile` (#1759).
- Opening the broker database no longer takes the write lock when the schema floor is unchanged (#1759).
- Retiring a checkout pins the commits only that checkout referred to: its own reflog, its bisect and per-worktree refs, the bisect files, and its current and original HEAD. The previous step skipped bisect and per-worktree refs, treated a local branch as protection, and pinned commits from shared reflogs the checkout never named (#1760).

### Added

- `aethyme broker advanced operations show --json` reports `outcome_journal`; coordinated commands report `outcome_record_pending` and `outcome_conflict` (#1759).
- `aethyme transcripts status` reports `enabled_source` and the first-run state; `enable` accepts `--after-inactive-days` (#1762).
- `--archive-ignored-directories` is listed in the `finish cleanup resolve` help (#1760).

### Known issues

- `git worktree add` and the directory removal in `finish` still run under the repository-wide lifecycle lock.
- A host on which `aethyme broker status` never runs never starts automatic transcript archiving.

## [0.8.118] - 2026-11-17

### Fixed

- Checkout retirement no longer keeps most closed checkouts on a busy host. The lane takes one bounded process observation per run and reuses it for every checkout and for the move into the trash; it used to share a throttled observation and mark every checkout after the fourth as "process use unknown". When the observation cannot be taken the run stops and retries within a minute (#1757).
- The trash deleter takes its own observation per drain and never deletes an entry a process still has as its working directory or an open file; an incomplete observation deletes nothing that run (#1757).
- An archive judgement resolves live checkout paths once per run, gets the lane's full budget, starts only with at least 90 seconds left, and is retried on the next run when cut off (#1757).

### Added

- Abandoned Git operations are archived and retired. For a closed session whose in-progress merge, rebase, cherry-pick, revert or bisect, working files, index and broker activity are all older than `[cleanup] abandoned_operation_after_hours` (default 24), the lane archives the operation state, the raw index, changed and untracked files and protected ignored files, together with a verified pack of every object the index refers to, pins the commits the state names, re-checks every source immediately before the move, and only then retires the checkout. The archive's README gives the restore steps. Advice `cleanup.retire-abandoned-operations-archived` reports the count (#1757).
- The retirement lane archives ignored directories outside the regenerable catalogue file by file, up to 20,000 files and 512 MiB per checkout; over the cap, or on any special or unreadable file, the checkout is kept. Manual cleanup still refuses unless `finish cleanup resolve --archive-ignored-directories` is passed (#1757).
- `aethyme broker gc plan --json` lists `checkout_retirement_keeps` with each kept checkout's reason; per-judgement phase timings are recorded (#1757).

### Changed

- Archived copies are written owner-only (files 0600, directories 0700) and the recovery-archive root is tightened to 0700. Partial archives are named `.partial-*`, ignored by the storage ledger, and swept after a day once their creating process is gone (#1757).
- Upgrade note: operation and ignored-file archives expire by age like other recovery archives (default 30 days, after 7 days of notice in `broker status`); keep one with `aethyme storage keep-archive <id>`.
- The maintainer policy notes name the compact status fields (#1758).

### Known issues

- Commits reachable only from a retired checkout's bisect refs or per-worktree refs are not pinned.
- A coordinated GitHub or Git write whose outcome cannot be recorded because the broker database is busy still needs a manual reconcile; a fix is in progress.

## [0.8.117] - 2026-11-17

### Changed

- **Breaking:** `aethyme broker status --json` returns the compact document by default. The full document requires `--full` (or `--refresh`); `--compact` is still accepted and does nothing. Field names of both forms are unchanged; both gain a top-level `form`, and the compact form a `hint` naming `--full`. In the compact form advice is under `advice.categories[]` (`severity`, `commands`); `advice[]` exists only with `--full` or `--summary`. Upgrade note: a script or tool that parses the full status document must add `--full` (#1754).
- A single advice entry's evidence is capped at 8 KiB in both forms, with `evidence_truncation` naming the count and the command that prints the rest. The storage advice names five archives and counts the others (#1754).
- Every status call appends a command-metrics row with its form, output bytes, `--session` presence, per-section timings and the calling process's name (name only) (#1754).
- Waiters for the repository lifecycle lock are served from a ticket queue, earliest deadline first; a waiter passed three times or for 20 seconds outranks every later arrival. Background lanes yield to waiting foreground commands for at most five attempts or 60 seconds, then queue from a detached worker only; maintenance running inside a foreground command never waits. The file lock still provides exclusion, so mixed versions stay safe (#1753).

### Added

- Standing authorizations, on by default: `[authorization] prepare_session_worktrees` and `disposable_test_environments` in `.aethyme/config.toml`. The generated agent instructions and `broker start` state that an agent may, in every session without asking again, run `aethyme broker submit prepare` in its own worktree (the declared pinned installs, no dependency or lockfile change) and create, use and drop local test databases and environments named for the session; never shared, staging or production data. `false` restores "ask the owner first". `start --json` gains `standing_authorizations`, and preparation reports `authorized_by_policy` (#1752).
- `aethyme broker status --session <id> --json` adds a `session` block: branch, worktree, head, pushed commit, unpushed count, preparation state and `next_action` (#1754).
- `aethyme broker push --session <id> --plan [--json]` shows the remote, destination ref, proposed commit, the remote commit last known and observed now, and the push mode, without pushing. The push report gains the observed pre-push commit and `pushed_refs` (#1754).
- Lifecycle lock telemetry on every command, including timeouts: wait, hold, holder kind, queue position, passes and aging; background lanes record yields and promotions. A timeout names the holder's command kind, session and the queue length (#1753).

### Known issues

- `git worktree add` and the directory removal in `finish` still run under the repository-wide lifecycle lock, so long waits on a busy repository are bounded and measured, not removed.
- Checkout retirement keeps most closed checkouts on a busy host when it cannot determine process use or finds an abandoned Git operation; a fix is in progress.
- The pull-request fields of the `status --session` block are empty: the broker stores no merge state or check results locally.

## [0.8.116] - 2026-11-17

0.8.115 was tagged but never published: its static Linux build failed to link. This release carries everything listed under 0.8.115 plus the changes below.

### Added

- Recovery archives expire by age. `[budget] unlanded_archive_days` in `<host state>/storage.toml` (default 30; 0 turns it off) sets the age, counted from the archive manifest's creation time and never shorter than the archive's recorded minimum retention. An archive is removed only after it has been listed as expiring for at least 7 days, in at least two worker passes a day apart. A pass that follows a gap of more than 7 days, or a clock that went back, removes nothing by age and restarts the notices. Each pass removes at most 50 archives or 1 GiB by age. An archive without a plausible creation time is never removed by age (#1746).
- `broker status --json` advice `storage.archives-expiring`: the archives that expire within 7 days (first 20 and a total), with the command to keep one (#1746).
- `aethyme storage keep-archive <id> [--unkeep]` exempts an archive from age expiry; `aethyme storage archives [--json]` lists archives with landed state, creation time, expiry time and kept state (#1746).
- `storage status` starts with a total and one line per root, shows archives kept for unlanded work as a count with the first five ids, and reports how often age expiry was paused and how many notices were restarted (#1746).

### Fixed

- The static Linux (musl) build links again: the checkout trash calls `renameat2` as a raw system call (#1748).
- The storage check no longer starts a worker, or evicts inline, from a temporary or self-update staging copy of the binary. The worker starts only from an installed copy of the same build; when none is found it is not started and the skip is counted (#1746).

### Changed

- Upgrade note: archives already older than the configured age are not removed on upgrade. They are listed in `broker status` for at least 7 days first; keep one with `aethyme storage keep-archive <id>`, or set `unlanded_archive_days = 0` to keep all.

### Known issues

- Checkout retirement, transcript archiving and archive expiry have been validated on Linux CI only before this release.

## [0.8.115] - 2026-11-17

### Added

- Checkouts of closed sessions are retired as soon as the session closes. A retirement lane walks closed sessions once per run and keeps a checkout only for a hard reason: a held lease, a running gate or unresolved operation, a process using it, an open session on the same path, or an in-progress merge, rebase, cherry-pick, revert or bisect. A clean checkout whose work is in Git is retired directly; otherwise the existing verified recovery archive is written first. An automatic close retires a clean checkout inline (#1738).
- A retired checkout is renamed into a broker-marked trash directory inside the broker's own worktree pool, its Git registration is removed from the repository side, and one host-wide deleter removes recorded trash entries. The deleter never waits on memory pressure, keeps a floor under load, and runs unpaced when the volume is below its headroom. Checkouts outside a broker pool keep the existing Git removal path (#1738).
- Commits reachable only from a retired worktree's own history are pinned under `refs/aethyme/retired/`. A pin expires when the default branch or a remote-tracking ref holds the commit, or after `recovery_archive_days` (#1738).
- `start --reuse` on a retired checkout recreates it from the session branch, or points to the archive when leftovers were saved (#1738).
- `aethyme transcripts` (opt-in): archives closed agent transcripts to reclaim disk. With `[transcripts] archive = true` (or `aethyme transcripts enable`), a transcript older than `archive_after_days` (3; `archive_after_days_claude` 14), open in no process, with no live agent for it and with every linked broker session closed and its checkout gone, is compressed with zstd, verified against the original by hash, moved, and only then removed. Codex transcripts go to `archived_sessions/<name>.jsonl.zst`, which Codex unarchives and resumes natively; Claude Code transcripts go under Aethyme's host state. Commands: `status`, `list`, `archive --dry-run|--apply`, `restore`, `enable`, `disable` (#1740).
- `broker status --json` advice: `cleanup.retire-archive-too-large`, `cleanup.retire-operation-in-progress`, `cleanup.retirement-stalled`, `cleanup.trash-unrecognized`, `cleanup.retired-pins`, `transcripts.linkage-degraded` (#1738, #1740).

### Changed

- The 24-hour grace no longer keeps a closed session's checkout directory: its work is in Git, in pinned refs or in a verified archive. The close note says the checkout was retired and how to continue (#1738).
- The older automatic removal and compaction lanes skip sessions the retirement lane owns, and their removals pass through the same pre-move checks (#1738).
- New dependency: `zstd` (#1740).

### Known issues

- Checkout retirement and transcript archiving have been validated on Linux CI only before this release.
- A checkout with ignored content outside the regenerable catalog that the archive plan refuses is still kept.
- Pool directories with no session record are not swept.

## [0.8.114] - 2026-11-17

### Added

- `aethyme storage status [--json]`: a ledger of everything Aethyme stores on a host, per class and per root, with bytes, entries, budget, reclaimable bytes and free space. It reads saved measurements only and never walks the disk. `aethyme storage run` runs one pass in the foreground; `aethyme storage adopt <dir>` marks an extra directory as Aethyme storage (#1739).
- Storage budgets, enforced by a low-priority background worker: recovery archives 4 GiB, gate cache 4 GiB per repository, symbol index 2 GiB, graph stores 2 GiB, tool caches 8 GiB, host state 16 GiB and host cache 12 GiB in total, and a 10 GiB free-space floor per volume. Override in `<host state>/storage.toml` (#1739).
- `broker status --json` advice `storage.over-budget`, `storage.below-floor` and `storage.unclassified`, with class names and sizes only (#1739).

### Changed

- A gate cache over its budget now keeps only the most recently used entry per gate kind; an entry a gate holds is never removed, and its lease is held across the removal (#1739).
- Leftover release and rollout directories expire after 14 days and generated test pools after 1 day. Anything Aethyme does not recognise by exact name is listed as `unclassified` and never deleted (#1739).
- Recovery archives are never removed by age. One holding unlanded work is never removed; a landed one is removed only when the class is over budget and past its recorded minimum retention (#1739).
- Every broker command does a constant-time storage check and starts the worker when a budget or floor is breached. `start` and `submit` refuse only when the volume they write to is below `hard_minimum_bytes` (2 GiB) after a 500 ms reclaim; the message names the setting and `AETHYME_STORAGE_LEDGER=0` (#1739).
- `gc plan` uses a gate cache size measured within the last hour instead of walking it (#1739).

### Known issues

- Checkouts of closed sessions are still removed by the old lanes, which do not keep up on a busy repository; the replacement is in review.
- The storage worker's macOS priority settings and the budget thresholds have not yet run on a busy macOS host.

## [0.8.113] - 2026-11-16

### Added

- `aethyme mcp`: `transport = "http"` servers. The hub runs one server per sharing key on a loopback port leased from the resources registry, and `mcp connect` bridges the agent's stdio to it over streamable HTTP, one HTTP session per agent. A server counts as ready only when every listener on its port belongs to the server's own process group and is bound to `127.0.0.1`; when that cannot be verified the agent gets a private server or its fallback at once (#1732).
- Every HTTP entry must state `http_auth`: `bearer_env:<NAME>` or `bearer_file:<flag>` (a new random token per start, never on a command line), or `none` together with `http_unauthenticated = true`. After start the hub probes once without the token and refuses a server that answers anyway (`token_not_enforced`) or does not clearly reject the request (`token_probe_inconclusive`) (#1732).
- `require_argv`: an entry can require flags on the final command line, checked after caller arguments and `base_args`; a later contradicting form of a required flag is refused. `http_path` sets the endpoint path (default `/mcp`) (#1732).
- `cleanup.landed-proof-unprovable` advice in `broker status --json`: sessions whose cleanliness proof still times out under a long budget, by session id (first 20 and a total). Counters `landed_proof_unprovable`, `landed_proof_timeout_streaks_pruned`, `owned_group_members_after_exit`, `owned_group_kill_after_exit_refused` (#1733).

### Fixed

- A proof listing that finishes normally no longer waits a fixed 75 ms for its process group: the group is stopped once while the finished leader is still unreaped, then the leader is reaped. This removes about 450 ms per landed session (#1733).
- The retirement lane gives its long proof budget first every fourth pass, caps a doubled budget at half of what is left of the pass, ends a timeout streak only on a conclusive answer, and prunes streak records of sessions that are no longer open (#1733).
- The hub's stale-upstream cleanup signals a recorded process group only when the recorded leader still leads it; its process listings run with a fixed locale and a 2-second timeout; persisted unhealthy marks cannot be extended by a clock set backwards (#1732).

### Changed

- Upgrade note: `aethyme mcp` remains opt-in; nothing reads or changes agent configuration, `PATH` or shell profiles until the owner enables it.

### Known issues

- The HTTP listener ownership check on macOS (`netstat` and `lsof`) has been validated on Linux CI through its parsers only and has not yet run on a real macOS machine. Under a sandbox that hides the listener table the verdict is "cannot verify" and the server is not shared.
- Only a server's leased port is checked, once at readiness. Other ports a server opens are its own responsibility; keep them on loopback with `require_argv`.

## [0.8.112] - 2026-11-16

### Fixed

- The background worker no longer spends a whole run on one session. The "is this checkout clean" proof for a landed session ran `git status`, `rev-parse` and `stash list` with no deadline and no timing; on the busiest repository one proof took 5 to 6.5 minutes, the worker ran 3 times in 45 minutes instead of 17, and no checkout was removed. Those listings now stop at the active Git deadline, are timed as `subprocess_git`, and a stopped or failed listing always keeps the session (#1730).
- Each session's proof gets a bounded budget inside a pass. A session that runs out is kept with the new decline `proof_timed_out` and the queue moves on; its budget doubles on each consecutive timeout, and each pass gives the session that has been timing out longest one long turn (#1730, #1731).
- Where only "is anything unclean" matters, the proof stops `git status` at the first record that counts. The uncommitted, untracked and ignored listings are one `git status --ignored=matching` pass instead of two walks (#1731).
- A stopped proof listing stops its whole process group, so submodule `git status` children are not left running (#1731).
- A Git timeout while searching for a session's landing is inconclusive instead of "not delivered" (#1731).
- On macOS a refused native process-table fetch is retried once before falling back to `ps`; refusals that `ps` then served are counted separately (#1730).
- The worktree watcher's storm test no longer depends on runner speed (#1723).

### Added

- `aethyme mcp`: run MCP servers through Aethyme. `mcp connect <name>` resolves a launcher chain (`npx`, `npm exec`, `pnpm exec`) offline to the installed program and starts it in place, with `--fallback -- <original command>` so an agent is never left without its server. `mcp resolve`, `mcp status` and `mcp config print` (prints the agent config snippet; never writes one). Entries live in the host file `mcp/services.toml`; a repository's `.aethyme/mcp.toml` runs only after `aethyme mcp trust` and can never replace a host entry (#1724).
- Shared servers: `mode = "shared"` stdio entries are served by one per-host hub, started on first use, that multiplexes agents onto one server per sharing key. The key covers the resolved program, working directory, every environment variable passed to the server, the env file, the server's source tree and build output, and the caller's arguments, so a session testing a different branch of a server gets its own. If the hub or a server does not answer within 2 seconds the agent gets a private server (#1728).
- Managed servers: `aethyme mcp manage add|remove|list` marks servers that must go through Aethyme. `aethyme mcp intercept install` creates launcher wrappers that hand a managed server's launch to `mcp connect` and pass everything else straight to the real tool; `aethyme mcp repair` lists, and with `--apply` rewrites, agent config entries that bypass it; `aethyme mcp bypass` reports managed servers running outside Aethyme. Nothing is enabled by default (#1729).
- Counters `proof_git_*`, `landed_proof_timeout_streak_*`, `landed_proof_session_budget_doubled`, `landed_proof_long_budget_granted`, `holder_process_table_native_denied_retried`, and per-key hub counters in `aethyme mcp status --json` (#1728, #1730, #1731).

### Changed

- Upgrade note: `aethyme mcp` is opt-in. Nothing reads or changes agent configuration, `PATH` or shell profiles until the owner runs `mcp intercept install` or `mcp repair --apply`, or sets `repair_on_session_start`.
- Upgrade note: restart long-lived `aethyme env supervise` processes after upgrading from 0.8.110 or earlier so the worktree watcher fix from 0.8.111 takes effect.

### Known issues

- `aethyme mcp` has been validated on Linux CI only; the hub, the launcher wrappers and config repair have not yet run against a real macOS machine.
- `transport = "http"` servers are not in this release.
- Each proof listing waits a fixed 75 ms for its process group, about 450 ms per landed session; the timeout-streak record is pruned only when the retirement lane closes the session; a proof that needs longer than the lane's share still never concludes (#1731).
- An agent host that runs commands under a pseudo-terminal is not intercepted by the launcher wrappers; config repair still applies to it (#1729).

## [0.8.111] - 2026-11-16

### Fixed

- The background worker no longer rebuilds its checkout-alias index on every compaction pass. On the busiest repository the index had been rewound 29,500 times and committed 9.7 million pages for 7,323 rows, and macOS reported 2.1 GB of database writes in 12 minutes. A complete index is now reused; only changed or unresolved rows are re-resolved, a full re-verification lap runs at most every 6 hours, and a row is written only where it differs. Removal still re-audits aliases from the filesystem under the session's lifecycle claim (#1717, #1719).
- The index scan is paced by the unattended pacer (load, memory warning, critical memory), runs before the worker takes the host deleter slot and the repository GC lock, has its own 30-second budget, and never rewinds a lap that a foreground command completed meanwhile. Unresolved rows are retried at most once every 30 minutes, in rotation (#1717, #1719).
- The worktree watcher no longer rewrites a counter file on every small batch of file events (34 GB of writes in 51 minutes on the busiest machine). A command now claims the counter it observed before asking Git, and the watcher publishes a new counter only when the current one has been claimed. Events are coalesced for 50 ms, so the watcher runs at most 20 rounds a second with one write per worktree per round, and an unchanged counter is never rewritten (#1720).
- Sessions are no longer kept open by a guarded command receipt left `starting` or `running` by a process from before the last boot. Receipts record a boot identifier; one from an earlier boot (or, for older receipts, last written more than 10 minutes before boot) stops blocking and is settled after the session closes. A late writer's real outcome is kept as a `guarded_exec.receipt_late_outcome` event (#1718).
- A preparation record whose host lease is released and whose preparer process is gone reports `interrupted` instead of staying `in_progress` forever (#1718).
- After a restart, a holder whose process started before boot is treated as gone since boot instead of getting a fresh stale grace when it is first seen gone (#1718).
- Sessions created before holder recording existed stop holding explicit claims once past the abandonment window, when nothing is unresolved and no process uses the checkout (#1718).
- On macOS the process table is read natively from the kernel instead of by spawning `ps`, with `ps` as the fallback. A refused or partial listing is reported as denied or failed for the whole pass, never as "holder gone" (#1718).

### Changed

- Upgrade note: the worktree watch directory moves from `worktree-watch-v1` to `worktree-watch-v2`. A supervisor started by an older version keeps its old watcher until it exits; restart long-lived `aethyme env supervise` processes after upgrading (#1720).
- The background worker's heavy lanes (checkout removal, compaction, requested checkout cleanup) wait until 10 minutes after boot. Nothing is dropped: the lane is retried when the hold ends. `AETHYME_ARTIFACT_WORKER_POST_BOOT_HOLD_MS` can shorten, never lengthen, the hold (#1717).
- Schema 63 adds the nullable column `guarded_exec_receipts.boot_id`. It is a compatible column-only migration; the schema floor is unchanged (#1718).

### Added

- `session_no_work_abandoned_after_hours` (default 4, `0` disables, capped at `session_abandoned_after_hours`): a spawned session closes after that many hours of inactivity when its branch still equals its base, the checkout is clean, its recorded holder is gone, and it has no unresolved receipt, running preparation, explicit claim, merge-queue entry or open pull request. Adopted sessions and sessions with no recorded holder keep the full window. Close reason `inactive_no_work`; counters `session_closed_no_work` and `session_no_work_kept_*` (#1718).
- Preparation state `interrupted`; events `guarded_exec.receipt_abandoned` and `guarded_exec.receipt_late_outcome`; keep reason `holder_process_table_denied` (#1718).
- Counters for index reuse, walk reasons, rows resolved, rows rewritten versus unchanged, ancestor and group rows written, page commits, pacer waits, unresolved-row retries, catch-up skips and post-boot holds (`auto_compact_cohort_index_*`, `cleanup_cohort_*`, `artifact_worker_post_boot_hold`) (#1717, #1719).
- The worktree watcher writes `status.json` in its watch directory at most once a minute: events received and coalesced, counters published and skipped, review cycles and watched worktrees (#1720).

### Known issues

- The old `worktree-watch-v1` directory is not removed automatically, and an unreadable claims directory is not distinguished in the watcher's stats (#1720).
- The 6-hour re-verification interval counts from when compaction first saw a lap, so a lap completed by a foreground command can be about 12 hours old before the next one. The post-boot hold does not cover the session retirement lane, and removal triggered by a full disk also waits out the hold (#1719).
- If a session closes and the write that settles its dead receipts fails, those receipts stay `running` until reconciled (#1718).

## [0.8.110] - 2026-11-16

### Fixed

- The background cleanup worker no longer stops for ten minutes whenever macOS memory pressure is at the warning level. Warning (or an unreadable level) now throttles like load: one checkout removal per slice, then a gap of at least two minutes per repository (`memory_warning_throttled`). Only critical pressure stops unattended deletion (#1711).
- Session retirement runs in every background worker run. It no longer waits for the host deleter slot, keeps at least 30 seconds even when removal overruns the run, waits up to 5 seconds for the GC lock (never ahead of a waiting foreground command) and retries a held lock after one minute instead of ten (#1711).
- `status`, `agents` and the other commands that reconcile agents no longer re-judge every live session on every call. Sessions inside the abandonment window, and sessions kept for recent activity until that activity plus the window (at most one window ahead), are skipped (`agents_view_abandonment_not_due`, `retirement_not_due_memo_skips`) (#1711).
- `broker status` and `broker agents` no longer fail to read the process table and report holders as unknown. Long-lived spawns are serialized with bounded captures, and a capture's deadline no longer includes the wait for the spawn lock (#1711).
- A detached worker that finds another worker running exits successfully instead of with exit 1, and takes its lock before probing Git (#1711).
- `gc reclaim plan` without `--session` no longer runs for many minutes on a large worktree root. It stops finding candidates after 60 seconds plus the worktree in progress (the plan is then partial: `"confirmable": false`, no review snapshot, no digest in the human output, and apply refuses it) and gives sizing 20 seconds in total, reclaimable candidates first. Candidates not measured in time stay listed with `"bytes": null` and are counted in `sizing.unmeasured`; `--full-scan` lifts both budgets (#1708, #1713).
- Whole-root reclaim plans share work: a plan reuses sizes measured in the last 90 seconds, measuring plans share their sizes every 5 seconds, and a budgeted plan started while another runs waits for it within its own budget. A marker left by a dead process is removed automatically, and never a live plan's marker (#1708, #1713).
- The `host.disk-low` and `host.gate-headroom` advice suggests `aethyme broker gc reclaim plan --session <id>` first (#1708).
- A confirmed GC journal no longer wedges on `.aethyme/logs/command-metrics.jsonl`. Apply drops only rows older than the reviewed cutoff from whatever the log holds now, keeping rows appended since the plan and malformed lines byte for byte. Journals written by earlier binaries resume the same way instead of failing with "reviewed artifact drifted" on every command (#1715).
- A gate log whose content changed after review is retained, never deleted, and listed under `files_skipped`; the rest of the journal completes. A metrics trim that cannot take its lock or keeps seeing concurrent appends is skipped the same way (#1715).
- An open-time GC resume that fails because the journal is invalid warns once, naming `gc apply --confirm` and `gc abandon --confirm`, and is retried only after 15 minutes or when the journal changes. A busy GC lock, a busy database or an I/O error is retried on the next open as before (#1715).
- `gc abandon` help no longer says it accepts only schema 2 or 3 journals; it accepts schema 4 (#1715).

### Changed

- Upgrade note: the GC plan digest binds the metrics-log rewrite as "rows older than the age cutoff", not the file's hashes, so rows appended between `gc plan` and `gc apply` keep the digest. A plan containing that rewrite has a different digest from the one an earlier binary printed; review it again once (#1715).
- Upgrade note: in `gc reclaim plan --json`, a candidate's `bytes` can be `null` for a whole-root plan, and `reclaimable_bytes` and `total_bytes` are then lower bounds (#1708).
- Command-metric appends take a shared lock on `.aethyme/run/command-metrics.lock`; the GC trim takes it exclusively, watches the replaced file for 250 ms after its rename to copy rows from writers that do not take the lock, and removes temporaries an interrupted trim left. No row is lost once every running binary is 0.8.110 or later; an older binary writing during a trim has a narrow window in which one row can be lost (#1715).
- `memory_backoff` now means critical memory pressure only, and sessions skipped as not due are no longer counted in `abandonment_examined` (#1711).
- CI: merges to main run Linux only. The macOS suite runs on a release tag or on request, and the Windows smoke on request; the release workflow still builds every platform (#1714).

### Added

- `reclaim plan --json` gains `confirmable`, `discovery {complete, budget_ms, worktrees_scanned, worktrees_not_scanned, elapsed_ms}` and `sizing {budget_ms, measured, reused, unmeasured, budget_exhausted, deferred_to, elapsed_ms}`. Every plan records a `reclaim.plan` row and every apply a `reclaim.apply` row (refusals included) in `command-metrics.jsonl`, with phase timings and counts (#1708, #1713).
- `AETHYME_RECLAIM_PLAN_DISCOVERY_BUDGET_MS` and `AETHYME_RECLAIM_PLAN_SIZING_BUDGET_MS` shorten, never lengthen, the whole-root plan budgets; `--full-scan` ignores them (#1713).
- `files_skipped` (`path`, `reason`) in the `gc apply` report and journal; broker meta keys `gc_journal_resume_failure` and `auto_cleanup.session_retirement_not_due` (#1715, #1711).
- Counters for the memory warning throttle, the worker's retirement share, retirement GC-lock waits and yields, not-due memo skips, agents-view skips, spawn-lock waits, the metrics trim (`gc_metrics_trim_*`), drifted file skips and journal resume backoff (#1711, #1715).

### Known issues

- An interrupted `gc reclaim plan` records no metrics row.
- The two-minute gap at memory warning is per repository, so several repositories can each remove one checkout in the same two minutes.
- A gate log skipped as drifted stays on disk after its rows are removed and is not collected later.

## [0.8.109] - 2026-11-16

### Fixed

- The session-retirement lap no longer stays on one session forever. A session whose landing proof ran out of time held the cursor on every pass, so no later session was judged and sessions days past `[retention] session_abandoned_after_hours` stayed open. The hold now lasts one pass: the next pass judges that session once more and then moves on (meta key `auto_cleanup.session_retirement_budget_hold`) (#1706).
- A failed process-table read no longer makes the rest of a pass answer `holder_unknown`. It is retried at once, then every 5 seconds, at most 3 times per pass (#1706).

### Added

- Every reason the inactivity lane keeps a session is counted as `session_abandonment_kept_<reason>`; the former `ActiveProcess` and `operation_unknown` outcomes are split by cause. `broker status --json` `landed_session_retirement` gains `abandonment {examined, closed, kept}` and `cursor_start`; the cleanup report gains `abandonment_sessions_closed`, `abandonment_sessions_kept` and `retirement_cursor_start` (#1706).
- Counters `retirement_cursor_advanced`, `retirement_cursor_unmoved`, `retirement_cursor_budget_held`, `retirement_cursor_budget_hold_released` and `holder_process_table_failed` (#1706).

### Changed

- Documentation names `holder_alive_current` and `holder_alive_in_use` in place of the retired `holder_alive` (#1706).

## [0.8.108] - 2026-11-16

### Added

- A running agent process no longer keeps a session it has moved on from. A live holder protects a session only while it is that holder's latest session, or a process has its working directory or an open file in the checkout, or work is running for it. When the same process was bound to a newer session after this one's last activity, a session whose every pull request merged (or whose work landed) closes after its window, and an idle session past the abandonment window closes when its checkout is clean. Nothing is deleted (#1704).
- Each automatic close records `holder_evidence` (`gone`, `moved_on`, `lease_expired`, `unbound`) and, for `moved_on`, the newer session; the returning agent is told. `landed_session_retirement.closed_by_holder_evidence` in `broker status --json` (#1704).
- Closed sessions whose checkout is already gone are recorded as cleaned (`session.checkout_missing_finished`) once the path is proven absent under a pool root that carries this repository's marker on a matching volume, unregistered in Git and unused. Nothing is deleted; sessions whose pool root is missing are kept (#1704).

### Changed

- The decline code `holder_alive` is now `holder_alive_current`; `holder_alive_in_use` is new. `worktree_unclean` declines are counted per kind in the worker metrics (#1704).
- The open-file observation used before a close is valid for 10 seconds and shared across candidates (#1704).

### Known issues

- Subagents share their parent's process, so one subagent starting a session moves the holder on from a sibling's quiet session. That session is still protected by its idle window, the process-use check and the running-work checks, and can be continued with `broker start --reuse`.

## [0.8.107] - 2026-11-16

### Added

- `submit prepare` settles an inconclusive shared-cache budget check by itself. The first check that cannot measure the store queues a full measurement, which the background cleanup worker runs at low priority and at most 10,000 entries a second, one per machine, without holding the cache lease. It pauses under memory pressure and never runs twice for the same request (#1696).
- Because the measurement holds no lease, a store counts as over budget only when it is more than 5% over `max_bytes`, and it rotates only after two consecutive measurements of the same store agree. A measurement between the budget and that margin keeps the store (#1696).
- A result is trusted for 1 to 24 hours depending on headroom and observed growth, so a healthy store is not walked every hour. Repeated failed measurements back off up to a day, and a completed rotation discards the old result (#1696).
- The prepare report and `submit prepare status --json` (`shared_cache_background_measurement`) say when a decision came from a background measurement. The `preparation.shared-cache-budget-inconclusive` notice clears once a measurement settles the question, or says why it could not. New pacing field `scan_limiter_sleep_ms` and `preparation_cache_measurement_*` counters (#1696).

### Known issues

- A store that cannot be walked in 30 minutes stays inconclusive; the notice says so.
- While a measurement runs it keeps directory handles open on the cache volume, so an external volume cannot be ejected until the walk ends (at most 30 minutes). Killing the worker is safe.

## [0.8.106] - 2026-11-16

### Added

- The broker closes a spawned session whose every recorded pull request merged, whose HEAD is proven landed on the default branch and whose checkout is clean, after `[retention] session_merged_after_hours` (default 2, `0` disables) hours without activity, counted from the later of its last activity and the merge. Nothing is deleted; the checkout follows normal closed-checkout removal. A follow-up task is protected for the longer of the landed and abandonment windows (#1697).
- Every automatic close records its reason and evidence in `session.abandoned` and leaves a note for the session. A command on the closed session, or run from its checkout, explains the close and how to continue (#1697).
- `landed_session_retirement` in `broker status --json` gains `closed_by_reason`, `merged_pull_request_examined`, `merged_pull_request_closed` and `abandonment_examined`, and keeps the last retirement counts across passes that judged nothing. New spans and counters cover the landed proof, holder check, close, memo hits and open-file observations (#1697).
- Pacing counters `host_slot_unavailable` and `host_slot_recovered` (#1700).

### Fixed

- Session retirement no longer starves. Sessions past the abandonment window are closed by it first, without Git; merged-pull-request sessions are judged next; landing proofs are remembered per head and searched again before any close; holders and process use are no longer reported unknown merely because a pass ran short; open-file observations are throttled per pass and the last one before a close is always evaluated. The background worker spends up to 120 seconds per run on retirement and stops on memory pressure (#1697).
- Tree removals always take the host deleter slot at their first unlink, witness and root unlinks read memory pressure first, a transient host-directory error no longer leaves deletion unserialised for the rest of a run, and a borrowed refusal stops the slice (#1700).
- Preparation-cache rotation-debt recovery runs under the deleter slot and waits for the next pass when it is busy (#1700).
- Moving a put-off group's lap cursor back can no longer drop the cleanup report (#1700).

### Changed

- Upgrade note: the new retention field changes GC plan digests. Re-plan any plan made by an older binary before applying it (#1697).

### Known issues

- Closed sessions whose checkout is already gone are not yet finished automatically.

## [0.8.105] - 2026-11-15

### Changed

- Background checkout removal no longer queues a whole worker run behind another repository's. The host-wide single-deleter slot is held only while deleting: one checkout removal and its rate-limit charge, or a tree removal's batches, and both hand it over after 20 seconds. No unattended unlink happens without it. Each deletion waits at most 2 seconds; a busy slot puts the removal off (`stop_reason` `host_deleter_busy`), the next pass judges that checkout first, and lanes pinned by a busy slot retry after one minute (#1694).
- At most two repositories judge closed checkouts at once (#1694).
- The removal gate samples memory pressure even when there is no host directory to serialise through (#1694).

### Fixed

- `finish` and `close` no longer warn "wake post-close checkout cleanup" when the worker holds the GC lock; the worker reconciles the cleanup outbox before its requested-cleanup lane. A close made after the running worker has passed that lane waits for the next broker open or worker start (#1694).
- The oldest-first lap cursor advances on a memo hit, and process-group probes keep polling on `EPERM` instead of failing (#1694).

### Added

- `host_slot_*` and `judge_permit_*` in the pacing summary and the slice record, including `host_slot_busy` per slice (#1694).

### Known issues

- A checkout put off by a busy slot is fully judged again on the next pass.
- Single control-file unlinks during preparation-cache rotation recovery are not serialised through the deleter slot.

## [0.8.104] - 2026-11-15

### Changed

- Each Git binary is checked for output-rewriting wrappers once per user instead of in every `aethyme` process. Accepted and rejected verdicts are cached in `<host cache dir>/git-probe/verdicts.json`, keyed on the binary's exact file identity, `PATH` and the dynamic-loader variables, and expire after 24 hours. A cached rejection only ever skips a candidate. Set `AETHYME_GIT_PROBE_CACHE=off` to disable the cache, or delete the file to force a new check (#1695).
- Deleting on Git evidence never rests on a cached acceptance. The deletion primitives (`worktree remove`, branch deletion, tree removal, pinned condemned-tree removal) and the commands that delete (`finish`, `close`, `cleanup`, `gc`, `reclaim`, `storage`, `worktrees`, and the GC worker) re-prove Git with a fresh probe and refuse if it fails; the ignored-directory sweep and GC journal replay inside broker open do the same before they gather evidence (#1695).
- On macOS `ps` and `sysctl` are spawned by absolute path; Linux keeps `PATH` order for `ps` (#1695).
- Probe directories are named `aethyme-git-probe-*`. The GC worker removes stale ones through the paced removal path, including legacy `.tmp*` directories that exactly match the probe's layout; only the worker's own temporary directory is swept (#1695).

### Added

- Counters for probe cache hits, misses, rejected hits and write failures, and the blocked reason `git_verdict_unconfirmed` (#1695).

### Known issues

- Hooks and shells that run with different `PATH` values each probe once, because `PATH` is part of the cache key.
- A process that deletes runs one fresh probe when its verdict came from the cache.

## [0.8.103] - 2026-11-15

### Changed

- Background checkout removal judges much faster. Checkouts still inside their closed-worktree grace or used by an open session are kept without running Git. Keeps for uncommitted changes, unmarked tracked deletions and commits not on the default branch are remembered until the checkout or the default branch changes, and are re-proved within 7 days. Selection runs one verdict per checkout instead of two. No removal proof changed, and removal still re-reads and re-proves everything under its locks (#1692).
- The oldest-first lap and the priority lane take turns, so the lap advances in every pass. A pass that removed nothing leaves the GC lock free for 500 ms instead of as long as it ran. A worker run keeps taking steady slices while the lap is unfinished, within its existing run budget (#1692).
- `capacity_removal.idle_streak` reads 0 while the lap is unfinished; the back-off streak is reported as `empty_laps` (#1692).

### Added

- `capacity_removal` gains `check_timings` (count and milliseconds per check kind), `groups_judged_per_min`, `memo_hits`, `db_in_grace`, `db_session_open`, `priority_db_skips`, `continued` and `empty_laps`. Timed phases `auto_cleanup_check_<kind>`, counters `auto_cleanup_db_kept_*`, `auto_cleanup_memo_hit_<code>` and `artifact_worker_capacity_removal_continued`, and the timed phase `artifact_worker_host_slot_held` (#1692).

### Known issues

- The worker holds the host-wide deleter slot for its whole run, judging included, so other repositories' workers wait longer behind a run that now lasts up to five minutes.
- While a worker run is judging, a session close can print "wake post-close checkout cleanup" more often. The cleanup request is already stored and is picked up by the next pass.

## [0.8.102] - 2026-11-15

### Changed

- Background checkout removal under sustained load (a load average above twice the core count) waits at least 30 seconds after each throttled removal, so workers no longer remove back to back while the repository is at its checkout limit. Without load nothing changes. Memory pressure is still a hard stop (#1690).

### Fixed

- A removal slice is recorded as blocked by load (`pressure_before_selection`) only when the load wait is what stopped its pass. A failed pass, or a budget spent on counting checkouts, keeps `failed` or `out_of_time_before_selection` and its escalating retry (#1690).

### Added

- The slice record and the worker's metrics row show the throttle gap and, per worker run, slices, removals and time spent deleting versus waiting: `throttle_gap_ms`, `run_slices`, `run_removed`, `run_elapsed_ms`, `run_deleting_ms`, `run_waiting_ms`, `run_deleting_pct`, `run_waiting_pct`; counter `artifact_worker_capacity_removal_slice` and timed phases `artifact_worker_capacity_removal_waiting` and `artifact_worker_capacity_removal_throttle_gap` (#1690).

## [0.8.101] - 2026-11-15

### Fixed

- Background checkout removal no longer stops on a busy machine. Since 0.8.96, when the one-minute load average stayed above twice the core count, every removal slice waited out its budget and backed off for ten minutes (`pacer_backoff`), so no checkout was removed on a machine that is always that busy, even at its checkout limit. Load now only slows removal down: the worker waits at most 5 seconds, then removes one checkout per slice at the normal cadence, and file deletion waits at most 1 second per batch on top of `[cleanup] unattended_unlinks_per_second`. Memory pressure still stops removal (`memory_backoff`, 10-minute retry) (#1687).
- A slice that pressure kept from running any pass records `pressure_before_selection` and no longer grows the idle back-off (#1687).

### Added

- The removal slice record gains `memory_wait_ms`, `load_wait_ms`, `pressure_seen` and `throttled`, and the stop reasons `memory_backoff` and `load_throttled`; new records no longer write `pacer_backoff`. Every worker run's metrics row shows its host-deleter wait and pacer waits (`artifact_worker_host_deleter_wait`, `artifact_worker_pacer_memory_pause`, `artifact_worker_pacer_load_wait`, `artifact_worker_pacer_limiter_sleep`) (#1687).

## [0.8.100] - 2026-11-15

### Changed

- Automatic checkout removal no longer keeps a closed checkout just because some other session's gate is running. A gate keeps a checkout only when one of that checkout's own sessions runs it, when the gate's recorded checkout is the checkout (or inside it, or contains it), or when nothing identifies where the gate runs. Records written by older binaries, malformed records and unverifiable per-session inventories still keep every checkout, so the narrowing takes effect as gates are started by 0.8.100 or later (#1686).
- Residual to know: a gate command that works in a sibling checkout (by changing into it or taking its path as an argument) is not attributed to that checkout, and a checkout moved after its gate started is matched by the path recorded at start. The artifact-use marker and the open-file check before removal remain the guards in both cases (#1686).

### Added

- Gate pidfiles gain a sixth field and owner-lock holder lines gain `pid=`, `start=`, `checkout=` and `end`, recording the gate's checkout. Holder lines are cleared on release, including after a partial acquisition. Older binaries read both formats (#1686).
- `kept[].gate_keep` (`own_session`, `path`, `unknown_owner`) and the counters `lanes.removal_gate_checks` and `capacity_removal.gate_checks` (`checks`, `scans`, `gates_seen`, `attributed_here`, `attributed_elsewhere`, `unknown_owner`, `inventory_unknown`, `elapsed_ms`) show which gates kept or no longer keep checkouts. Gate records are read once per selection pass and fresh for each checkout immediately before removal (#1686).

### Known issue

- Background checkout removal does not start while the one-minute load average is above twice the core count (since 0.8.96). On a machine that is always that busy it never runs. A fix that keeps memory pressure as the only hard stop is in progress.

## [0.8.99] - 2026-11-15

### Fixed

- Steady checkout removal keeps judging the closed-checkout lap within a slice while it moves, and starts its idle back-off (5 to 30 minutes) only after a full lap found nothing removable; an unfinished lap resumes a minute later. In 0.8.97 a slice stopped at the first pass that selected nothing and backed off with most of the lap unjudged, so nothing was removed on a repository with many kept checkouts (#1684).
- The priority lane no longer re-judges a checkout kept for a reason only a state change can lift (unproven, legacy or adopted-only ownership, a missing directory, listed ignored leftovers) until its sessions, merged state or files change. Those checkouts filled the eight priority slots every run. Transient keeps (`gate_running`, `in_grace`, a held lease) are still retried (#1684).

### Added

- `capacity_removal` gains `lap_completed`, `priority_memo_skips` and `memo_skips`, and the stop reason `lap_complete`; `auto_cleanup.lanes` gains `removal_priority_memo_skips`. Counters `artifact_worker_capacity_removal_{pass,priority_memo_skips,memo_skips,lap_completed,stop_lap_complete}` and `auto_cleanup_priority_memo_skip` (#1684).

## [0.8.98] - 2026-11-15

### Fixed

- `rotation_volume_key` keeps only the keys a pending rotation debt may still need. Unprotected keys are trimmed to the 16 newest per cache root and volume UUID when the debt completes or its binding is retired, and before each cap check, so the table no longer grows until it refuses that root for good. An unchanged key is not rewritten on every pass (#1680).
- Leaked `rotation_debt_volume` bindings no longer block new bindings at the 512-row cap; the oldest binding with no debt row is evicted (#1680).
- The `gc` cache sweep settles a rotation journal lap in the same pass when recovery moves the root row to the live identity mid-walk; it used to settle two passes later after a failed-acknowledgement warning (#1680).
- A clean `finish` after a remount reports `cleanup.ownership_reproof`, and the orphan device rebind no longer counts a refusal twice (#1680).

### Changed

- Rechecks made during checkout compaction count under `retirement_ownership_reproof_*` instead of `gc_*` (#1680).
- Upgrade note: the volume-key protections hold only while every broker on the host runs 0.8.98 or later. An older binary may prune a protected key; the record that needed it is then refused, never accepted (#1680).

### Added

- Counters `retirement_ownership_reproved`, `retirement_ownership_reproof_refused`, `retirement_ownership_reproof_unavailable`, `preparation_rotation_debt_volume_binding_evicted` and `preparation_rotation_volume_key_cap_reached`, and the timed phase `retirement_ownership_reproof_registration` (#1680).
- Documentation states when a disk that reuses a device slot is refused and when an exact identity is accepted without a volume UUID check (#1680).

## [0.8.97] - 2026-11-15

### Fixed

- The background worker removes closed checkouts whose work has landed whether or not the repository is near `[worktrees] max_checkouts`, so the checkout count follows merges instead of drifting up to the limit. Without pressure a steady slice runs: up to 4 removals in 60 seconds, checkouts with a merged pull request or a due cleanup request judged first, then 16 checkouts of the oldest-first lap. It is due only while a closed session still holds its checkout, runs again 1 minute after a removal and at once when a cleanup request's grace ends, and otherwise backs off 5, 10, 20, then 30 minutes. Every removal proof is unchanged, and behaviour under pressure is unchanged (#1681).
- Session retirement yields to a foreground command waiting for the GC lock (#1681).

### Changed

- A recorded checkout count taken under a different `max_checkouts` no longer reports capacity pressure (#1681).
- Correction to the 0.8.96 notes: they listed `service_context_volume_refused` as a counter. It is a refusal label only; the job service records no counters.

### Added

- `capacity_removal` gains `trigger` (`steady` or `pressure`), `priority_examined`, `priority_removed`, `lap_examined`, `lap_removed` and `idle_streak`. Counters `artifact_worker_capacity_removal_trigger_{steady,pressure}`, `artifact_worker_steady_removal_deferred` and `session_retirement_yielded_to_foreground` (#1681).
- Documentation: a consistency pass for 0.8.96, a troubleshooting section for a slow or silent prepare, and rollback notes for 0.8.96 (#1679).

## [0.8.96] - 2026-11-14

### Fixed

- `submit prepare` no longer drains and re-downloads a repository's shared preparation cache because its budget inventory stopped early. An inventory that stops on its entry or time limit with the bytes seen at or under budget is now inconclusive and keeps the store; one that passes the budget stops at once and rotates. The entry limit rises from 100,000 to 2,000,000, below which one monorepo's package stores no longer fall. After three inconclusive checks in a row `broker status --json` gives the notice `preparation.shared-cache-budget-inconclusive`. Repositories that never share the cache now use the hourly budget-check interval too (#1677).
- A half-finished preparation-cache rotation resumes after an external macOS volume returns under a new device number, including after a second renumber, and the `gc` cache sweep now finishes such rotations. Each rotation debt records its volume UUID in the host rotation journal (new additive tables `rotation_debt_volume` and `rotation_volume_key`; schema version unchanged). Anything unproven stops with `debt_volume_reproof_refused: <condition>` (#1674).
- GC reclamation and checkout compaction, `finish`, `finish cleanup` and resuming an interrupted finish cleanup no longer refuse a checkout after a remount. Every deletion lane uses the one ownership re-proof automatic removal had: unchanged paths, inodes and spawn generation, matching recorded volume UUIDs (or, for records saved without them, an unchanged device-sharing pattern), and the session's Git registration on its branch. Finish recovery proofs record volume UUIDs (additive field `volume_uuids`) and bind HEAD to the recorded commit (`head_moved`) (#1676).
- The job service restarts after a remount instead of failing with "service job context identity changed", and fetch reconciliation survives a renumbered common directory. The service records its volume UUIDs in `broker-service-jobs-context-volumes.json`; a fetch journal records `common_volume_uuid` (#1673).

### Changed

- Background checkout removal under capacity pressure is several times faster (measured about 13 an hour on the busiest repository; expected about 90). A removal slice may run up to 120 seconds and remove up to 12 closed checkouts, one per pass with every proof unchanged. Each pass checks memory and load pressure first, the GC lock stays free at least as long as the last pass held it, and the unattended pacer credits only Git's own removal time and never lifts its rate for disk pressure. `broker start` at the checkout limit, `gc apply`, `gc abandon` and `gc forget-abandoned` wait up to 30 seconds for the GC lock, and the worker yields to them (#1675).
- Upgrade note: a binary older than 0.8.96 cannot resume an interrupted finish cleanup that 0.8.96 started on macOS, and falls back to "fetch again" for a fetch journal 0.8.96 wrote. Both fail closed. Platforms without a stable volume identifier (Linux) keep the exact device comparison (#1673, #1676).

### Added

- Work done while the shared preparation cache is held is visible. Recovery, drain and long inventories print progress at start and every 15 seconds; `submit prepare status --json` gains `current_operation` and, for a waiting session, `waiting_on`; a waiting prepare names what the holder is doing. The prepare report and metric carry `shared_cache_operations` with an outcome per inventory, and the "root identity differs from its durable journal" refusal says what to check and run (#1677).
- Removals that used a re-proof report it (`cleanup.ownership_reproof`, `compact_checkout_removed ownership_reproof=…`). Each refusal has its own reason and counter (`ownership_reproof_refused_<reason>`, `preparation_rotation_debt_volume_refused_<condition>`, `fetch_journal_common_volume_refused`, `service_context_volume_refused`) (#1673, #1674, #1676).
- `auto_cleanup.capacity_removal` gains `stop_reason`, lock held, wait and yield times, Git removal and pressure-wait times, pacer credit, charge and unpaid amounts, `cache_lane_reserved_ms` and `retry_in_ms` (#1675).
- Documentation: a "checkout count keeps growing" troubleshooting section that walks from `status --json` to the fix for each keep code (#1672).

## [0.8.95] - 2026-11-14

### Fixed

- Durable identity records start identifying a volume by its volume UUID instead of a device number, which changes on every remount of an external volume. A new shared volume-identity primitive reads the volume UUID on macOS; other platforms report it as unknown and keep the exact device comparison (#1670).
- Preparation-cache rotation recovery no longer stalls forever after a remount. A journal root row that differs only by device number is rewritten to the live identity when the inode is unchanged, no debt is pending, and either a recorded volume UUID or the parent root marker proves it is the same directory. An exact match must also match a recorded volume UUID. A refusal ends with `root_volume_reproof_refused` and the failed condition (#1670).
- An old artifact-use marker whose only mismatch is renumbered devices is reported as an unresolved marker of its checkout instead of failing with "artifact-use state could not be verified". The checkout is still kept; the message says how to resolve the marker (#1670).

### Added

- Session retirement reports renumbered legacy markers as their own deferred reason, `legacy_artifact_markers`, which `broker status` treats as actionable. Telemetry counts `artifact_use_marker_legacy_device_renumber`, `preparation_rotation_root_reproved` and `preparation_rotation_root_reproof_refused` (#1670).

## [0.8.94] - 2026-11-13

### Fixed

- Capacity removal no longer stalls on a repository with many checkouts. The capacity-pressure probe ran a full worktree listing with no deadline and used the whole slice budget before selection. The checkout count for `start`, `status`, the worker and the probe is now one read of the registration directory, and the per-removal lock check reads the checkout's own registration; the full listing is only a fallback. A slice whose inventory stayed unavailable retries after one minute when it made progress and backs off to ten only without progress (#1668).

### Added

- `[cleanup] regenerable` can release standalone ignored files. A pattern releases a regular file when it matches the file's whole path with wildcards kept inside one path component; a pattern containing `**` never names a file. `blocked_by_ignored` rows for files carry the `config_line` that releases exactly that file, or a `config_refusal` when configuration cannot (#1667).
- The capacity removal record says what a slice waited on: `probe_ms`, `waiting_on` and the index lap position; plan lanes gain `removal_probe_ms` and `removal_inventory_wait`. The background worker writes one command-metrics row per run with its phase timings (#1668).

### Changed

- Upgrade note: existing `regenerable` patterns now also release the standalone ignored files they match. `<dir>/*` releases ignored files directly in `<dir>`, and `*.ext` or `*-cache` release matching root-level files. The load-time probe still refuses patterns that match common secret or ordinary root-file names (#1667).
- Protected names are wider and are never released by configuration: any name starting with `.env`, SQLite and `.db` files with their `-wal`, `-shm` and `-journal` siblings, `*.tfstate`, `*.jks`, `*.keystore`, `.git-credentials` and `.pgpass`, in addition to the existing set. A tagged cache directory holding one of these now keeps its checkout (#1667).

## [0.8.93] - 2026-11-13

### Fixed

- Closed checkouts whose ownership record went stale after a volume remount are removable again. Automatic removal now re-proves a spawned checkout whose only mismatch is renumbered devices: paths, inodes, the root generation, the Git registration on the session branch and the branch tip must all still match. A removal that used it carries `removed[].ownership_reproof`; a refusal is kept as `ownership_reproof_refused` with the failed condition (#1665).
- Under capacity pressure, checkouts with a merged pull request or a due cleanup request are judged before the session-order lap, so removal follows what became removable instead of waiting for the lap to reach it. A checkout kept at the last step is remembered and no longer blocks the next one (#1665).
- A removal slice that ran out of time before judging anything retries after one minute, backing off to ten on repeats, and records `out_of_time_before_selection` with the phase that used the budget. The checkout count no longer lists every worktree (#1665).
- Host resource waits are served in arrival order. A request for a whole pool is no longer starved by later smaller requests; dead waiters are skipped and reaped automatically; waiters back off instead of polling the store four times a second (#1664).

### Added

- Distinct keep codes for checkouts automatic removal cannot prove: `ownership_witness_missing`, `ownership_witness_legacy`, `ownership_adopted_only`, `checkout_missing` and `cohort_unavailable`, each naming the manual route. New fields `lanes.removal_priority_examined` and, on the capacity removal record, `count_ms`, `pre_selection_ms` and `budget_consumed_by` (#1665).
- Resource contention output reports the queue: position, units ahead, holders and time waited, in `acquire --json`, `run --json` and `explain`; `list` shows waiting requests in arrival order. Command metrics gain `resource_queue` with waited time, attempts, position at enqueue, times skipped and reaped waiters (#1664).

## [0.8.92] - 2026-11-13

### Fixed

- The background worker's capacity removal slice now keeps working through a long backlog. A slice that ran out of time part-way through a lap of closed checkouts retries after one minute instead of ten, and a bounded pass resumes a part-built checkout index instead of ending with no inventory. The ten-minute wait applies only when a lap ended with nothing removable (#1662).

### Added

- The capacity removal slice records its own result, which later worker steps no longer overwrite: `auto_cleanup.capacity_removal` in `gc plan --json` and `capacity_removal` in `status --json`. It carries the outcome (`removed`, `lock_busy`, `inventory_unavailable`, `lap_unfinished`, `nothing_removable`), counts by keep code, the ignored directories that block removal with the `[cleanup]` config line that would release each, how much of the lap remains, and phase timings. `auto_cleanup.lanes` gains `removal_scan_ms` and `removal_inventory_unavailable` (#1662).

## [0.8.91] - 2026-11-13

### Fixed

- Closed checkouts are removed again on a repository near its checkout limit. The background cleanup worker now holds the GC lock for at most 30 seconds per cache pass and releases it between passes; under capacity pressure it removes closed landed checkouts itself, one at a time and at most four per slice, waiting out the deletion pacer between removals. Short passes read a recorded checkout count, so they see capacity pressure, and selection stops at three quarters of the removal budget so the removal step always has time (#1660).
- A remote write that provably never reached the remote is recorded as `failed` with a retry hint instead of `outcome_unknown`, so it no longer write-blocks the repository. This covers a `git push` over a built-in transport that died while listing the remote's refs (DNS failure, refused connection, TLS or SSH handshake failure, authentication failure), a `gh` command rejected by its own argument parser, and a single-request `gh api` whose connection never opened. Timeouts, resets and multi-request `gh` commands still end `outcome_unknown` (#1659).

### Added

- A checkout whose automatic removal was interrupted is finished on a later pass. The broker records a removal attempt before it starts and resumes only a checkout with a matching record whose every change is a deleted tracked file. A checkout with tracked deletions and no record is kept and reported as `unmarked_tracked_deletions`; under `[cleanup] strict` a recorded one is kept as `strict_tracked_deletions` (#1660).
- New report fields `auto_cleanup.lanes.removal_selected`, `auto_cleanup.lanes.removal_apply_reserve_ms` and `removed[].resumed_interrupted_removal`, and `remote_write_*` phase counts for how each failed remote write was classified (#1659, #1660).

## [0.8.90] - 2026-11-13

### Added

- Pace the background cleanup worker. It now runs at background CPU and disk priority, deletes at most `[cleanup] unattended_unlinks_per_second` entries per second (default 1000; `0` or `"unlimited"` restores full speed), pauses while memory pressure or load is high, and lets only one worker delete at a time on the machine. `finish`, `finish cleanup` and `gc apply` are not paced. The last run's pacing is reported as `cleanup_retention.last_artifact_sweep.unattended_pacing` (#1657).
- Report which ignored directories keep a closed checkout from being removed: `gc plan --json` gains `auto_cleanup.blocked_by_ignored`, `auto_cleanup.lanes`, and a `code` plus `ignored_blockers` on each kept checkout (#1655).

### Changed

- Remove more checkouts of closed sessions whose work has landed. Each automatic pass is now shared between retiring landed idle sessions and removing closed checkouts, so neither starves the other, and retirement examines likely-landed sessions first. `.hypothesis`, `playwright-report`, `test-results` (when it holds `.last-run.json`) and any directory carrying a valid `CACHEDIR.TAG` count as regenerable tool output (#1655).
- `broker start` at the checkout limit now removes a few removable checkouts itself, for at most about 20 seconds, before refusing (#1655).
- `broker submit` no longer holds the repository's Git lane while it builds its plan and replays commits, so a concurrent `broker push` no longer waits on it. Submit takes two short shared holds, checks the operation journal for repository-wide writes made meanwhile, and rebuilds its plan when something changed (#1656).

## [0.8.89] - 2026-11-13

### Changed

- Unattended maintenance closes a session whose work is on the fetched default branch (merge commit, squash or rebase, by the rule `finish` uses) once it has been idle 24 hours, its worktree is clean, it has no unmerged linked pull request, and no process uses it. This now applies even when the agent process that held it is still running with an expired task lease, provided no gate, operation, preparation, open file or working directory of that process is in the worktree. The checkout is then removed by the existing cleanup lane with its own containment proof. `[cleanup] auto_remove = false` turns this off (#1652).
- Every proof is checked again under the session and lifecycle claims, and HEAD and worktree activity are read once more immediately before the close; a commit made in between keeps the session open (#1652).

### Added

- `broker status --json` reports `landed_session_retirement`, and the auto-cleanup report gains `landed_sessions_examined`, `landed_sessions_closed`, `landed_sessions_declined` (a count per reason code), `retirement_cursor` and `landed_retirement_elapsed_ms`, so every session that stays open has a recorded reason (#1652).
- Phase telemetry `landed_session_retirement`, `landed_landing_search`, `landed_landing_search_memo_skipped` and `landed_sessions_*` counts (#1652).

### Fixed

- Checking a session's worktree for cleanliness no longer refreshes the index timestamp that the idle windows read (#1652).

## [0.8.88] - 2026-11-13

### Fixed

- `broker push` decides against the remote branch's actual tip. A tip that HEAD contains gets a plain push. A rewritten branch leases only against a commit the session itself pushed, including pushes made through `broker advanced git`, which now record their commit. Anything else is refused with exit 3 before Git runs, with the commands that resolve it, instead of failing repeatedly with Git's `stale info` (#1650).
- A session push holds only its branch lane in repositories that set `extensions.worktreeConfig`; that setting alone kept every push on the repository-wide lock. Other `extensions.*` keys still keep it wide (#1651).
- `submit` no longer waits for a worktree lifecycle claim while holding the write lanes for plan reuse; it gives up the reuse instead (#1651).

### Added

- Every coordinated push, commit and fetch records in `coordination_timing` whether it took its narrow lane (`narrow_lane`, `narrow_lane_kind`) and, when it did not, a stable `narrow_lane_unavailable_reason`. `operations stats` counts these under `narrow_lanes` (`taken_count`, `unavailable_count`, `unavailable_by_reason`, `by_kind`) (#1651).
- A write refused while waiting for a lock names the holder (command, session, pid, scope, time held), including holds that have no journal row. Exclusive holders write `<lock>.holder.json` while they hold the lock (#1651).
- Push phase counts `push_plain_fast_forward`, `push_lease_recorded`, `push_lease_journal_proven`, `push_refused_foreign_remote_tip`, `push_remote_tip_redecided`, `push_remote_tip_rejected` (#1650).

### Changed

- `queue_wait_ms` is the total wait across every lock a write waited for. Its previous meaning is `lock_queue_wait_ms`; the other parts are `git_common_lane_wait_ms`, `local_ref_lane_wait_ms` and `repository_lane_wait_ms` (#1651).

## [0.8.87] - 2026-11-13

### Removed

- The legacy `aethyme repo clear-cache` and `aethyme root show|set` commands; neither had a remaining use (#1648).
- The code path in `aethyme explore` that started the engine daemon. It could no longer run: Explore reads the graph store directly. `aethyme-engine-cli daemon` is unchanged (#1648).

### Changed

- `[publication] allow_break_glass` is ignored. It gated only the removed `ship execute --break-glass`. The key still parses, and `broker status` warns with `config.allow-break-glass-removed` saying to delete it (#1647).
- `deploy plan` no longer reads or preserves `aethyme/integration` when a default branch is fetched, and leftover integration commits no longer block first enrollment. Without a fetched default branch it behaves as before (#1647).

### Fixed

- Fourteen more broker commands (`operations`, `advisories`, `exposures`, `note`, `ownership`, `quality-report`, `review state`, `review waive`, `handoff`, `checkpoint`, `queue`, `watch pr`, `watch repo`, `deliveries`) refuse malformed arguments before opening the broker, so a refused call no longer runs startup maintenance (#1647).
- `broker advanced gh` no longer refuses a flag value such as `--title -F` as a missing body file; `-F` is a body file only for the `pr`, `issue` and `release` commands that define it, and stays a field for `gh api` (#1647).

## [0.8.86] - 2026-11-13

### Changed

- `broker push` no longer builds the full agents view to report duplicate work. Candidates come from branch, task and the cached pull-request listing, and liveness is worked out only for sessions that match (count `agents_view_sessions_skipped`). `push`, `start`, `adopt` and `reuse` therefore no longer reconcile or close unrelated sessions as a side effect; the next `status` or `agents` does (#1645).
- `review run`, including review on push, makes its read-only GitHub calls in parallel instead of one after another. Reports and error order are unchanged (#1645).

### Added

- Phase telemetry for review on push (`review_on_push_policy`, `review_on_push_child_start`, `review_on_push_child_wait`) and for the review run's reads, classification and GitHub writes (`review_run_*`) (#1645).

## [0.8.85] - 2026-11-13

### Changed

- A commit run through the broker in a session's own worktree and branch, and a `broker push` of a session branch, no longer hold the repository-wide write lock while hooks run; each holds only its branch's lane. `gh pr edit <n>` (without `--base`) and `gh pr ready <n>` lock one pull request. A waiting repository-wide write takes an intent lock first, so overlapping narrow commits cannot starve it (`coordination_timing.wide_intent_wait_ms`). `operations stats` gains `narrow_lanes` (#1641).
- A repository with a `reference-transaction` hook, `push.followTags`, or Git configuration whose push footprint cannot be proven keeps the repository-wide lane. `[coordination] session_hooks_hold_repository_lock = true` restores it for repositories whose hooks write other branches or shared state (#1641).

### Fixed

- Closed session checkouts are no longer kept forever because of Aethyme's own generated files (`.claude/settings.local.json` as deploy creates it, `.claude/hooks/aethyme-load-context.sh`, deployed skill files) when their bytes are unchanged; an edited copy still keeps the checkout. This also lets `broker start` recover by itself at the linked checkout limit (#1643).

### Added

- `broker status` warns with `cleanup.checkout-capacity` from three quarters of the linked checkout limit (#1643).

## [0.8.84] - 2026-11-13

### Fixed

- `broker push` metrics now record the pre-push hook split (`push_git_process_before_hook`, `push_git_process_hook`, `push_git_process_after_hook`). It was always empty because Git's trace gives a hook's start no relative clock. New counters `push_git_trace_without_hook` and `push_git_process_split_unavailable` separate a push with no hook from a trace that could not be split (#1639).
- A closed session's build output that `finish` could not reclaim while a background worker held the GC lock is now reclaimed by that worker's next sweep or the next broker open, instead of up to a day later (counter `closed_session_artifact_reclaim_deferred_gc_locked`). The grace period before a kept checkout itself is removed is unchanged (#1639).

## [0.8.83] - 2026-11-12

### Fixed

- A coordinated fetch of a single branch no longer write-blocks the repository. A failure or timeout is recorded `failed` (`repeatable_fetch_failed`), and a fetch left behind by a crash is closed automatically at the next broker open or overlapping write (`repeatable_fetch_auto_resolved`, event `operation.fetch_reconciled`). Pushes, tag pushes, deletions and pruning fetches still block until reconciled (#1637).
- `broker advanced gh` writes that name a missing, empty or directory file argument (`--body-file`, `--notes-file`, `-F <file>`, `--input`, `-F key=@file`) are refused before the operation is journaled, so a local mistake no longer write-blocks the repository (#1637).

### Added

- `finish --json` reports `cleanup.reclaim_deferred` and `cleanup.reclaim_worker_pid` when a running reclaim worker holds the GC lock (#1637).

## [0.8.82] - 2026-11-12

### Fixed

- Broker commands refused for malformed arguments now refuse before opening the broker for writing, so they no longer run startup GC maintenance or remove a closed checkout. Covers `finish cleanup`, `gc`, `advanced leases`, `advanced representation record` and `start --adopt`; messages and exit codes are unchanged (#1636).
- Tests that depended on how fast a time-budgeted GC sweep ran are now bounded by an entry count, and the fixtures that keep a closed session's worktree turn unattended removal off through one shared helper (#1631, #1634).

## [0.8.81] - 2026-11-12

### Changed

- `broker push` no longer fetches the default branch when the remote read it already makes under the lock shows the default branch has not moved, saving 1.2-2.3 s on most pushes with no staler advice. The report gains `default_branch_fetch_skipped_unmoved`, and `default_branch_fetch_age_seconds` then counts from that read. `push --pr` still fetches. Set `AETHYME_PUSH_FETCH_SKIP_UNMOVED=off` to always fetch (#1633).

## [0.8.80] - 2026-11-12

### Changed

- In a repository with no fetched default branch whose integration branch still holds promoted commits, `submit` verifies against the local default branch sessions are cut from (`verified_against.source: local_default`, with a `fallback_reason`), and `status` and `doctor` report `integration.leftover-work` with the commit count and the reconcile command (#1629).
- `broker push` with `[review] run_on_push` no longer makes a separate GitHub pull-request lookup when the open-PR listing it fetched in the same push already shows the session's pull request (`pr_lookup_listing_age_seconds`, counter `push_pr_lookup_reused`) (#1630).

### Added

- Push telemetry splits the push process into `push_git_process_before_hook`, `push_git_process_hook` and `push_git_process_after_hook`, and times `push_pr_lookup`, `push_overlap_listing`, `push_overlap_provider_diff` and finalization (#1630).

### Fixed

- Submit's next-step line no longer names `push --pr` where it cannot work (no remote, or pushing session branches is not authorized) (#1629).

### Removed

- `auto_cleanup_landed_integration`, which nothing called once promotion was removed. `integration reconcile` remains for repositories that still hold work on a local integration branch (#1629).

## [0.8.79] - 2026-11-12

### Removed

- Local promotion. `aethyme broker submit` verifies and moves nothing; work lands through the session's pull request. `broker submit promote` is gone, with the promote engine, its recovery at broker open, the ship plan and execute code, and the status rows only promoting repositories emitted (`integration.may-move`, `integration.upstream-main-ahead`, `integration.fast-forward-available`, `integration.stale-promotions`, `main.external-writes`, `integration.behind-upstream`) (#1627).

### Changed

- `[promote] mode = "auto"` or `"manual"` and `promote_on_submit = true` still parse but are ignored; `broker status` shows a `promote.mode-removed` warning asking to delete them. `mode = "verify-only"` stays valid and silent. `submit --verify-only` is accepted and does nothing; `submit --json` always reports `promoted: false` (#1627).
- New sessions always start from the default branch, never from the integration branch (#1627).

### Fixed

- A main-checkout session in a repository with no remote selects its gates from the base before the refresh; promotion had hidden the wrong base (#1627).

## [0.8.78] - 2026-11-12

### Fixed

- `broker finish` recognises work landed by a pull request in repositories that do not promote, including a squash merge. When the fetched default branch holds the session's content on every path any of its commits touched, finish closes the session as published without a manual `representation record`. A follow-up commit that is not on the default branch, including one that only restores a path, still blocks (#1625).
- When finish cannot prove the work landed, its next step is now one that can resolve the state (`broker push --session <id> --pr`, or `finish --abandon --reason`), and `finish --abandon --reason` is honoured there. The abandonment event is recorded once, only when the session closes (#1625).
- A rename landed by a pull request is reported to sessions still editing the old path; the check compared only against the integration branch (#1625).
- Verified queue entries become `externally_landed` once their work is established on the default branch, so queue retention prunes them (#1625).

## [0.8.77] - 2026-11-12

### Removed

- Explicit-lease refusal in promoting repositories. `leases claim`, `start --path`, `adopt --path`, guarded `exec` and `submit` no longer fail because another session holds an overlapping explicit lease; they report the overlap as a warning with a `note send` command, as repositories that do not promote already did. The JSON fields `accepted`, `blockers`, `would_conflict` and `conflicting_leases` remain, with constant values (#1622).

### Fixed

- `broker finish` reclaims a kept worktree's build output again. Its bounded directory walk gave up on the whole worktree at the first directory deeper than six levels, so a cargo `target` directory was often never found and the session ended "incomplete" with gigabytes left behind. An over-deep subtree is now skipped while its siblings are still walked, and ignored directories Git already names are checked directly (#1620).
- `broker finish cleanup --all-cleaned --apply` reclaims build output from the worktrees it keeps (#1620).

### Added

- `finish --json` reports `cleanup.artifact_bytes_reclaimed`; the bulk cleanup reports `artifact_directories_reclaimed` and `artifact_bytes_reclaimed` (#1620).

## [0.8.76] - 2026-11-12

### Removed

- `aethyme broker advanced integration status`, `aethyme broker advanced integration wait-stable`, `aethyme broker advanced main reconcile plan|apply` and `aethyme broker submit promotion-record plan|apply`. They only served the local landing path. Hints that named them now point at `aethyme broker status` or `aethyme broker advanced agents`. `integration reconcile` remains until the promoting modes are removed (#1617).

### Changed

- `aethyme deploy`'s first enrollment publishes its gate-verified commit directly, without a promotion and without the integration branch. It refuses before any remote operation unless the commit's only parent is the reviewed remote base, and still refuses a `review_gated` publication policy. The enrollment journal gains a `verified` phase; journals an older version left at `base_prepared` or `promoted` still resume (#1618).

### Fixed

- A supervisor replaced by an update releases its services before starting its successor, so the worktree watcher is available from the successor's first tick, and the proxy is tried again while its port is still busy instead of being given up on the first attempt (#1619).

## [0.8.75] - 2026-11-12

### Added

- `broker push` records where its Git push step spends time, as `push_git_*` phases (lock wait, remote reads, the push process, the hook dry run), and reports `default_branch_fetch_age_seconds` when it reused a fetch (#1614).

### Changed

- A plain `broker push` reuses a default-branch fetch made successfully by any broker command in the last 60 seconds instead of fetching again; `push --pr` and `sync` always fetch. Set `AETHYME_PUSH_FETCH_REUSE=off` to always fetch (#1614).
- In a repository that does not promote, a session's own changes are measured from its merge-base with the fetched default branch, one Git call, and the integration branch is not read (#1612).
- In a repository that does not promote and has a fetched default branch, the broker no longer creates or moves the integration branch. `status` and the adopt/reuse drift check compare against the default branch, and the drift check's next step is `broker sync --session <id>` (#1615).

### Deprecated

- The `integration.refreshed` advice row and the `broker.integration.refreshed` event are no longer emitted in repositories that do not promote (#1615).

## [0.8.74] - 2026-11-12

### Removed

- `aethyme broker advanced ship plan|execute`, with `--sync-main`, `--only`, `--entry` and `--break-glass`. Pull requests are the only landing path: `aethyme broker push --session <id> --pr`, then merge on GitHub. An existing `[delivery] default` key is parsed and ignored; `[delivery] push_session_branches` is unchanged. The publication engine stays for `aethyme deploy`'s first enrollment (#1609).

### Changed

- Generated agent instructions, the Aethyme skill and the guides no longer present local promotion or shipping as a way to land work (#1609, #1611).
- Failure telemetry separates a wrapped command's own failure under `broker advanced exec` from a broker failure: it is counted per command as `wrapped_command_failures` and kept out of the failure reasons (#1604).

### Fixed

- The host supervisor notices that its executable was replaced by an update and hands over to the new build within two ticks, instead of running the old build for as long as anything needs it. After 0.8.73 an old supervisor kept the new worktree watcher from starting until it was stopped by hand (#1610).
- A worktree's registration answer is remembered until its `.git` file changes, removing a repeated Git call per session in listing commands (#1605).

## [0.8.73] - 2026-11-12

### Added

- One shared file watcher, hosted by the host supervisor (`aethyme env supervise`), lets `broker status` and `broker agents` reuse a session worktree's changed-path answer while nothing in that worktree changed, instead of running `git diff` and `git ls-files` in it again. Git still produces every answer; any doubt (no watcher, a dropped file event, a changed index or baseline) asks Git as before. Commands that decide whether work may land never reuse. Set `AETHYME_WORKTREE_WATCH=off` to always ask Git (#1602).
- The host supervisor hosts several services in one process, so new background work does not need a process of its own (#1602).

### Changed

- This repository's Rust gates draw their cost from the `aethyme-host-build` capacity pool, the same pool local cargo runs reserve, so gates and local builds queue instead of compiling at once (#1601).

### Fixed

- `broker submit prepare` reclaims the shared preparation cache by itself when the lease holding it belongs to a preparation whose process is gone, instead of refusing until someone runs `resources reconcile`. It does so only for a lease that holds nothing but the preparation cache; any other dead holder still needs the reviewed command (#1603).

## [0.8.72] - 2026-11-12

### Added

- Report why commands failed in `broker advanced metrics --json` as `failure_reasons`: for the last seven days, each command's failures grouped by the message it printed (with paths, identifiers and numbers taken out), its exit class, and the five most frequent reasons (#1598).

### Changed

- `broker submit prepare` now exits 6 when the shared preparation cache stayed busy for the whole `--wait` budget, and 3 when the cache is held by a dead holder or a mismatched capacity policy; both were exit 1. The contention message says to rerun or pass a longer `--wait` (#1599).

## [0.8.71] - 2026-11-12

### Fixed

- Stop `broker status` and the other commands that inspect every session from starting one Git process per session worktree to check that it belongs to the repository. A linked worktree is now recognised from its own registration files, and Git is asked only when those are missing or inconsistent. On a repository with 373 sessions this took one `status` from about 640 Git processes to about 95 (#1596).

## [0.8.70] - 2026-11-11

### Added

- Name the release that wrote each row of `.aethyme/logs/command-metrics.jsonl` (`aethyme_version`), and report calls, p50 and p95 time, the costliest phases and exits by class for each command of each release as `by_release` in `broker advanced metrics --json` (#1588).
- Queue local cargo builds on this repository through a host-wide capacity pool with `packages/aethyme/scripts/with-host-capacity.sh`; `test-like-ci.sh` uses it for local runs (#1591).

### Fixed

- Make `broker submit` faster when the session's work has already landed and nothing is left to verify: it now checks for content before the lease audit, which was most of the time such a submit took. The recorded outcome is unchanged (#1592).
- Stop Explore's text search from adding vocabulary taken from one particular codebase to requests that mention words such as "watchlist", "diff" or "revision" (#1589).

## [0.8.69] - 2026-11-11

### Added

- Let several sessions prepare against a repository's shared cache at once with `[shared_cache] max_concurrent` (1 to 6, default 1); rotation, sweeps and recovery still hold the cache alone. Set it only when every broker on the host is 0.8.69 or later (#1586).
- Report an object store with about 25,000 or more loose objects in status advice, and start one background `git gc --no-prune` a day after `broker finish`; `AETHYME_OBJECT_STORE_MAINTENANCE=0` turns it off (#1583).
- Record how long each phase of every broker command takes, and how often it ran, in command metrics (#1579).

### Fixed

- Give `broker finish` 60 seconds for its Git checks by default instead of 10, which one finish in seven exceeded (#1581).
- Scan session leases and overlap states in parallel, which roughly halves `broker advanced agents` in a repository with a hundred sessions (#1580, #1582).

## [0.8.68] - 2026-11-11

### Added

- Report time spent in each push phase, each preparation step and the shared-cache wait in command metrics, and let a review run name the head it expects (#1570).

### Fixed

- Make `broker finish` faster in repositories with long histories: the landing search reads fewer paths per candidate and compares patches only for commits that changed the same files (#1573, #1575).
- Make bulk cleanup plans and audits ask each remote for its branches once per pass instead of once per remote-tracking ref of every retained worktree (#1576, #1577).
- Find unresolved coordinated operations without scanning the whole operation history, which every push and submit paid for (#1572).
- Wait for a busy shared preparation cache instead of failing the preparation (#1572).

## [0.8.67] - 2026-11-10

**Breaking:** Previously refused repeated `git` and `gh` command names are now accepted, and focused status exposes new revision options and output. See [upgrade and rollback guidance](UPGRADING.md#v0867).

### Added

- Add session-focused compact status and scoped revision deltas (#1565).

### Fixed

- Back off repeated automatic shared-environment deployment failures (#1567).
- Accept redundant Git and GitHub program names in coordinated commands while preserving safety checks (#1568).

## [0.8.66] - 2026-11-09

### Added

- Record phase timings for guarded exec and broker submissions to identify latency (#1561).
- Retire safely delivered, inactive sessions after a configurable interval and focus compact status on a session (#1559, #1558).
- Report checkout work states and preserve ignored data during cleanup (#1544).

### Fixed

- Clarify advisory leases, deferred gate recovery, graph authority, and deployment verification failures (#1550, #1553, #1555, #1556, #1557).
- Preserve the full main-branch test suite while optimizing submission checks (#1542).

### Changed

- Reduce preparation hashing, bound status advice, and batch lease operations (#1546, #1547, #1554).
- Improve cleanup guidance for PR links and persisted permissions (#1543, #1545).

## [0.8.65] - 2026-11-08

### Fixed

- Retry transient SQLite sidecar open races without accepting unsafe files (#1540).
- Recognize supported review configuration and direct generated-policy repair to the readiness planner (#1539).

### Changed

- Document how to diagnose generated-policy drift when no schema migration is pending (#1538).

## [0.8.64] - 2026-11-08

**Breaking:** `env logs` now includes available hook-phase logs by default, and supervisor output is timestamped. See [upgrade and rollback guidance](UPGRADING.md#v0864).

### Added

- Filter environment logs by phase and the latest run, inspect supervisor exits, and configure log retention (#1530, #1531).
- Record session replacement so cleanup can recognize a reused checkout's predecessor (#1533).

### Fixed

- Reclaim obsolete shared-environment exports and clear resolved redeploy errors (#1530).
- Verify generated deployments when the inert local bridge is present (#1520).
- Explain inferred gate failures without presenting an unconfirmed host cause as fact (#1529).
- Restore release signing and make release gate verification resilient (#1525, #1526).
- Correct cleanup refusal advice for artifact-use markers (#1528).

### Changed

- Avoid a redundant Git history walk when measuring branch drift and redundant searches during broker finish (#1532, #1534).
- Run focused broker submission checks for ordinary Rust changes while retaining the full workspace suite on main (#1535).

## [0.8.63] - 2026-11-08

**Breaking:** Broker database schema advances to 62, and `broker sync` and `broker submit` no longer clean a diverged integration branch merely because its work has landed. See [upgrade and rollback guidance](UPGRADING.md#v0863).

### Added

- Check configurable source-code shape guidance in warning or enforcing modes with `aethyme quality code-shape` (#1503).
- Collect local agent feedback and broker friction signals on demand (#1496, #1497).

### Fixed

- Reclaim eligible closed checkouts automatically when the checkout limit is reached, while preserving protected or unproven worktrees (#1507).
- Resume interrupted cleanup of external broker worktrees after their Git pointer has already been removed, using the recorded recovery proof (#1514).
- Submit synchronized session histories whose merge parents are already in integration without rewriting those histories (#1503).
- Allow artifact-use marker review to recover safely after mount renumbering, including a separately mounted checkout, with clearer refusal guidance (#1487, #1515).
- Let artifact-use marker review proceed during unrelated session-index changes while retaining checkout safety checks (#1516).
- Retire proven markers from removed predecessor checkouts when a worktree path is reused, including after a drive remount (#1518).
- Discover sibling worktree roots when a host container is reached through a symlink (#1506).
- Isolate broker test fixtures from host state and macOS temporary-path differences (#1506, #1508).

### Changed

- Reduce broker status, lease-claim, no-op sync, and integration-status work on repositories with large histories (#1495, #1500, #1504).
- Fail fast when quarantined host resources make a requested capacity impossible (#1504).
- Size local Rust gates to available CPU capacity and run affected tests locally while keeping the full suite in CI (#1517).
- Isolate contract-gate builds from workspace-test cache contention (#1499).
- Document contract preflight and fixture-isolation practices, preserving mirrored skill parity (#1501, #1509, #1511).
- Restore the workspace dependency contract for code-shape parsers without changing their pinned versions (#1510).

## [0.8.62] - 2026-11-06

### Fixed

- Keep `broker submit prepare` and `broker gc storage` working after an external drive reconnects with a new device number. The preparation-cache root marker failed with "does not match its pinned directory", and no command could repair it. The marker is now matched to its directory by path and inode (#1486).

## [0.8.61] - 2026-11-06

### Fixed

- Keep working in a repository after its external drive reconnects with a new device number. Broker commands failed with "bound worktree root or parent identity changed", and no recovery command covered it. A device-only change is now accepted when the registry's saved volume UUID matches the live volume (#1484).

### Changed

- Record broker startup, session push, and review-on-push phase durations in successful push command metrics (#1479).
- Run full CI on merged `main` instead of on pull requests, which keep only the cross-process contract check (#1483).
- Document how to refresh generated guidance without losing it (#1482).

## [0.8.60] - 2026-11-06

### Added

- Show the other session's changes to the overlapping files on every lease overlap, including refused claims and the post-commit conflict radar; set `[leases] overlap_diffs = false` to turn this off (#1462, #1471).
- Optionally flag another session changing a file that the file being edited directly calls or imports, off by default through `[leases] overlap_impact` (#1467).

### Fixed

- Let `start --reuse` reuse adopted sessions in verify-only repositories, and in `auto` repositories where the integration branch never existed; a session behind or diverged from its base is still refused and not moved (#1473, #1475).
- Give local-merge guidance from `finish` when no default branch has been fetched (#1473).
- Stop concurrent broker commands refusing each other's SQLite sidecars and metadata probes (#1470).
- Keep key material out of overlap diffs (#1471).

### Changed

- Request pull request labels and repository labels concurrently during review-on-push (#1477).
- Keep test fixtures out of the host's worktree root, and remove flaky test dependencies on host timing (#1469, #1472, #1474, #1476).
- Clarify cleanup-service rollout verification, command-metric profiling and process measurement guidance (#1460, #1466, #1478).

## [0.8.59] - 2026-11-05

### Added

- Add bounded, age-scoped cleanup of closed worktrees, with recently reused checkouts excluded and ownership rechecked under the lifecycle lock (#1453).

### Fixed

- Reuse pinned preparation-cache roots during recovery validation, halving full ancestor-path walks while retaining ownership and replacement checks (#1463).
- Keep host-state worktrees under a symlinked parent (#1461).
- Wait for lifecycle claims and identify the session holding them (#1458).
- Hold environment requests through a redeploy instead of failing them (#1457).
- Follow the newer run when merge-chain encounters a cancelled base run (#1455).
- Make verify-only the default promotion mode (#1456).

### Changed

- Parallelize fresh worktree checkout (#1459).
- Render only retained Explore source-search hits (#1451).
- Reuse integration branches and retention indexes during cleanup, and avoid decoding operation history during GC planning (#1445, #1447, #1449).
- Stop repeating subsets of the workspace test suite on main (#1450).
- Clarify paused artifact-sweep status and document lightweight profiling techniques (#1448, #1452, #1454).

## [0.8.58] - 2026-11-05

### Added

- Use revision-bound graph evidence to guide broker synchronization, retaining synchronization when relatedness cannot be established (#1439, #1441, #1442).
- Expose the Compose project name to environment provisioning hooks (#1436).

### Fixed

- Allow activity probes to be updated without destroying components and distinguish probe errors from idle results (#1435).
- Wait briefly for concurrent worktree registry writes instead of failing immediately (#1433).
- Default closed-checkout cleanup to a one-day grace period; explicit retention overrides remain in effect (#1444).

### Changed

- Run the cargo-test gate in shards and improve host resource admission (#1430, #1434, #1438).
- Reuse patch IDs when checking whether checkout work has already landed (#1443).

## [0.8.57] - 2026-11-04

### Added

- Preview an unmerged shared environment declaration in an isolated worktree-owned environment without changing the shared serving alias (#1420).
- Plan and apply opt-in issue-set manifests with validation and same-host recovery journaling. Independent-host concurrency and sessionless coordination remain unsupported; do not run concurrent applies for the same set from independent hosts (#1424).
- Scope garbage-collection plans to exclude unprovable orphan roots while retaining conservative storage-budget verdicts (#1418).

### Fixed

- Roll back every dispatched provisioning attempt, including timed-out hooks, and retry transient container removal while retaining capacity when cleanup cannot be proven (#1417).
- Keep resource supervision running when a caller disconnects from progress output; killed supervisors and externally managed resource operations still require safe reconciliation (#1423).
- Search larger landing histories within a separate total work budget while retaining per-batch limits and fail-closed deadlines (#1425).

### Changed

- Reduce CI test-binary linking and run the Rust workspace suite in ten shards (#1416, #1419, #1422, #1426).

There is no broker database schema change in this release.

## [0.8.56] - 2026-11-04

### Added

- A coordinated `broker advanced issues sub-issue add|remove` helper resolves issue numbers to GitHub IDs and records each mutation. Native sub-issue and dependency API writes are recognized as issue operations without Git ref-write acknowledgement (#1408, #1411, #1414).
- Environment components can set `hook_timeout_seconds`; timed-out or preempted hooks retain their partial output and failure reason in component logs (#1407).
- Opt-in Git performance diagnostics can isolate many-files settings, probe older Git clients' reftable refusal, and report preparation and gate timing for reuse (#1395, #1396, #1397).

### Fixed

- Environment inference measures synced broker sessions from their recorded current base, so incoming upstream changes do not inflate the session's profile (#1402).
- Compose teardown removes remaining containers bearing the exact project label after `down` and retries cleanup (#1407).
- Gates refuse to verify sparse checkouts with omitted tracked inputs; graph-free Explore reports incomplete source coverage without discarding cached entries (#1398, #1401).
- Gate admission briefly waits for a concurrent checkout lifecycle claim instead of immediately failing on transient contention (#1399).
- Git performance diagnostics measure the current submit patch profile and surface alternate-client warnings (#1393, #1405).

There is no broker database schema change in this release.

## [0.8.55] - 2026-11-03

### Added

- Named ports per environment component (#1385): `ports = ["api", "db", "web"]` reserves each port with the environment, exposes it as `AETHYME_ENV_PORT_<COMPONENT>_<NAME>` and lists its URL variable in plans. The `listen = true` primary port is unchanged.
- Scalar hook-receipt fields as variables (#1383): later components receive top-level string, number and boolean receipt fields as `AETHYME_ENV_<COMPONENT>_<FIELD>`, alongside the full JSON in `AETHYME_ENV_RECEIPT_<COMPONENT>`. Colliding or reserved names are skipped.
- Profiles may share a rank (#1384). Choosing an equivalent profile at the inferred rank needs no reason, and the `duplicate_rank` validation diagnostic is no longer emitted.

### Changed

- The Local controller's native gates default to off (#671). It records the frozen native job and abandons it before dispatch, so candidate code does not run with host authority; the confined Docker build and independent verifier still decide acceptance. Setting the policy's `native_gates` to `required` restores the earlier behaviour. The broker's submit and merge gates are unchanged.

### Fixed

- Environment profile inference uses the broker session's recorded start commit, so a stale integration branch no longer inflates a session's changes (#1381).
- Merge commits count only their first-parent diff during range inference (#1379).

There is no broker database schema change; rollback to 0.8.54 is unrestricted.

## [0.8.54] - 2026-11-02

### Added

- Repository-level keep-awake caps: `max_pin_seconds` and `max_exec_busy_seconds` in `environments.toml` tighten the host limits for that repository and take effect on the next `env up` without a reconfigure; `env pin --for` past the repository cap is refused with `pin_too_long`.
- Template retention by count: a component's `template_keep` keeps its newest N templates, and cache pruning protects them instead of evicting the oldest first.
- Prebuilt environment artifacts (#1237, #1366): declared prebuilds run when the default branch advances, during quiet windows, for templates or cacheable builds, with an optional host memory gate and preemption when an environment becomes active. A fetch or push queues them; `AETHYME_PREBUILD_NUDGE=0` turns that off.
- Automation outcomes (#1238): `broker advanced owners --commits` resolves landed commits to the sessions that produced them, `note send --from automation:<name>` reaches live sessions, and `[[automation.outcomes]]` rules with `broker advanced outcomes report` file, update and resolve one issue per outcome and notify the responsible sessions. Sustained budget breaches can route through a `budget:environments` rule.
- Reviewed recovery of a stale Git `objects/maintenance.lock`, and storage reports that include the shared object database and rerere cache.
- `resources pressure` and `status doctor` report preparation-cache rotation recovery progress per repository.

### Fixed

- Preparation-cache rotation recovery converges on large caches (#1236): it works in bounded slices up to ten minutes and stops only when a slice makes no progress, instead of failing every attempt at a fixed five-second deadline.
- An `env up` waiting on a cache key held by a long prebuild waits for that build instead of timing out after 30 minutes.
- Storage plans bound preparation-cache liveness checks under one deadline instead of rehashing every live checkout per cache store.
- `enhance` honours Cargo `workspace.exclude` when inventorying members (#1363).
- `broker advanced owners` and automation `note send` no longer open mutable broker state or act for the session whose worktree they run from.
- Spawned-session witnesses include stable volume UUID evidence so a remounted primary volume can still validate the same process identity (#1370).
- When both UUID captures succeed, new artifact-use markers persist stable identities for the primary and checkout volumes, allowing device-number drift while retaining exact path and inode checks. Legacy markers remain strict; stale records require reviewed recovery (#1373).
- Ownership claim listing derives liveness from a read-only snapshot and leaves session, event, lease, and claim state unchanged.

The broker database advances to schema 61 (one additive table) while the compatibility floor remains at 60, so 0.8.53 can open the store. Older readers fail closed on the new spawn-witness and artifact-use marker formats and may retain affected checkouts until reviewed recovery; rollback is database-compatible but not unrestricted.

## [0.8.53] - 2026-11-01

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0853).

### Added

- Renewable task ownership, opt-in expiry, bounded proactive maintenance and durable cleanup diagnostics, with a five-day default closed-checkout grace period.
- Opt-in sparse worktrees and shared package-download caches while retaining isolated writable environments and full-checkout fallback.
- Explicit, default-off repository service commands for private, identity-bound shared GC planning, with pinned runtime pairs and orderly shutdown. Service job-history retention remains unbounded; this release does not qualify its opt-in use for bounded storage.
- Environment restart, optional components, path routes, entry URLs, static SPA fallback, empty profiles, rule exclusions, input-sensitive shared redeployment, and recorded budget reporting.

### Fixed

- Contract checks use a memory-bounded literal index instead of a regex-set fallback that could time out on large integration diffs, preserving symbol boundaries and per-file removal accounting.

- Environment cache reclamation uses bounded inventories, exact filesystem identity, coordinated use checks and resumable deletion journals. Incomplete evidence retains cache entries; cache storage is not yet charged to the managed-storage ledger.
- Bundled resource requests receive distinct ports and cumulative same-pool capacity checks; environment pin and busy limits are validated consistently.
- Closed-session finish remains idempotent without rebinding ownership. Cleanup writer tests and captured child output have bounded waits.

## [0.8.52] - 2026-10-31

### Added

- Explicit, digest-confirmed recovery of stale artifact-use markers for closed sessions, retaining ambiguous process or checkout identity evidence instead of forcing cleanup.

### Fixed

- Targeted archive-resolution cleanup resumes already-confirmed GC recovery without starting unrelated automatic cleanup during the command.
- Cooperating managed routers, engine daemons and updaters pin the exact runtime bundle through daemon startup and pruning, and validate daemon protocol and full build identity. Older launchers and updaters do not participate in this protection; mixed-generation rollout still requires a reviewed transition.

## [0.8.51] - 2026-10-30

### Added

- An opt-in macOS cleanup service pins the installed router and engine and schedules bounded public GC sweeps every 15 minutes, subject to broker eligibility and retention. Installation is inert; activation and control changes require exact reviewed plans.

### Fixed

- Fresh worktree-root recovery completes on APFS after interrupted Cargo-config publication while preserving exact file and inventory checks.
- Session retirement has a separate durable request and retry lane, so cache pressure and future closed-checkout grace periods cannot postpone abandoned-session checks indefinitely. Existing holder and lease proofs remain required.
- Checkout cleanup distinguishes a proven removed historical checkout from a new checkout at the same path using complete repository, generation and filesystem identity evidence.
- Final recovery-archive revalidation receives a bounded proof budget, and cleanup reports specific retained-cohort blockers.
- Recovery archives without a recorded minimum age retain the 15-day minimum. Clean preparation journals can recover a changed root identity only when root-wide durable debt is absent.

## [0.8.50] - 2026-10-30

### Fixed

- Automatic retirement persists first verified holder-absence evidence for its bounded candidate page, so lease grace can advance without a status refresh. Live, unknown and rebound holders remain protected.
- Compact cleanup admits its archive lane after cache handoff overhead while preserving the full archive minimum, any due whole-checkout cleanup reserve and the existing worker time limit.
- Explicit artifact sweeps report incomplete when another GC holds the namespace lock, leaving skipped work visibly retryable.

## [0.8.49] - 2026-10-30

### Fixed

- Detached recovery workers pass consistent storage bindings through launch and admission while preserving exact database selection and test isolation.
- Abandoned-session cleanup can release leases only after a fresh process snapshot and matching durable evidence prove the recorded service holder has exited. Ordinary lease checks remain conservative.
- Background compaction visits multiple due candidates per worker while reserving time for each verified archive; a foreground GC sweep also wakes compact-only recovery work through the existing admission checks.
- Recovery verification reuses exact bundled-root evidence and removes a redundant initial bundle check while retaining the final archive and content verification before checkout removal.
- Preserved-branch retirement skips an unused preliminary inspection while retaining final content, checkout, prerequisite and archive checks.

## [0.8.48] - 2026-10-29

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0848).

### Added

- Lease-bound environment up/down, park/wake, activity-aware idle parking and request-triggered wake routes are delivered together.
- Recovery archive schema 6 retains incremental-bundle prerequisites in repository-local refs before checkout retirement.

### Fixed

- Retirement batches remain confirmable during ordinary forward delivery progress while still refusing changed content or lost prerequisite coverage.
- Plan-local ancestry queries reuse exact completed results without reusing stale evidence during apply.
- Archive GC releases prerequisite refs only after verified complete removal and complete sibling-reference inspection; uncertainty retains them.
- Environment teardown, rollback and generic lease recovery preserve resource ownership when state or completion cannot be verified. Per-environment operation locks serialize lifecycle effects.

## [0.8.47] - 2026-10-29

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0847).

### Added

- Exact-ID recovery batches preserve verified archives before retiring closed checkouts, continue independent targets, and report the exact remaining IDs. Replanning the original set safely skips completed targets; adopted retirement requires explicit opt-in and preserves branches.
- Default-off background compact recovery keeps the minimum recovery interval without retaining a full prepared checkout throughout that interval. Archives preserve the registered and actual checkout branch identities and all required commit roots.
- Transactional post-close cleanup requests keep compact retries separate from the normal full-checkout grace and survive a missed process wake.
- YAML configuration is accepted alongside TOML for supported policy files, with one selected format per file stem and explicit validation errors. Environment profiles remain TOML-only.
- Declarative environment profiles add validation, planning, supervised component state and process/container drivers. Container-engine conformance requires an explicitly enabled engine.
- Supervised resource runs report phase timings, including finalization after preparation, spawn or child-observation failures.

### Fixed

- Cleanup discovery checkpoints bounded session pages and reindexes changed rows instead of restarting on routine session activity or each successful removal.
- Cleanup eligibility and final ownership checks preserve protected lifecycle states and report retained targets without blocking unrelated progress.
- Recovery GC respects each archive's recorded minimum age and every preserved root, including after configuration changes or partial delivery.
- Interrupted recovery reuses verified archives and bounds incomplete staging to one deterministic per-session location.
- Lease release-request resolution uses durable event order, preserving an acknowledged result when sibling leases expire.
- Lease release auditing reserves the SQLite writer before reading its snapshot, avoiding read-to-write upgrade failures between concurrent sessions.
- Configuration loading, scaffolding and resolution protection preserve policy read errors.
- The recorded composition diff-engine version matches the locked dependency.

## [0.8.46] - 2026-10-29

### Changed

- New-session startup fetches its own upstream without scanning or moving the
  unrelated primary checkout. Fresh-base and lease admission checks remain.
- New worktree startup reports numeric phase timings for fetch, lifecycle-lock
  wait, storage admission, checkout, registration and response preparation in
  JSON and local command telemetry. Detached starts also report their setup
  phase. Totals overlap subphases; the reported pre-output total excludes
  serialization/output and router dispatch.
- Command elapsed metrics now include pre-dispatch primary-checkout maintenance
  for commands that still perform it.

## [0.8.45] - 2026-10-28

### Changed

- The default `broker gc plan` and fresh-plan `broker gc apply` now share a
  cooperative 10-second inspection budget. Incomplete plans have no apply
  digest. `--full-scan` skips that default budget and discovery entry cap while
  retaining the existing six-level artifact-discovery depth; the generated
  apply command preserves the selected mode. Default planning defers a
  nonempty recovery-archive inventory.

## [0.8.44] - 2026-10-28

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0844).

### Added

- Bounded, resumable idle-session retirement makes abandoned sessions eligible for later cleanup while preserving active or uncertain task holders and the default fifteen-day closed-checkout grace.
- Optional `[cleanup].ignored_directory_sweep` broadens in-place reclamation to Git-ignored directories in retained closed checkouts. It defaults off until the new runtime protocol is activated.
- Maintenance reports optional retirement counts and typed deferral reasons separately from removed checkouts.

### Fixed

- Cleanup considers every session sharing a checkout, rechecks ownership and current use under lifecycle claims, and preserves ambiguous cohorts.
- Repository-selected clean filters and version probes publish artifact-use protection before running. Verified process-group completion settles that protection even when hashing times out; uncertain completion remains fenced.
- Runtime marker files no longer perturb working-tree hash keys, while tracked runtime-path changes remain inputs.
- Fresh database publication verifies the current compatibility floor rather than requiring the historical binding floor.

## [0.8.43] - 2026-10-28

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0843).

### Changed

- State-only closed checkouts now have a fifteen-day default grace period before automatic cleanup or reviewed GC may propose removal. Explicit repository retention overrides keep their configured value.
- Recovery plans use schema 3 and a versioned content confirmation; archives use schema 4. Delivery observations are recorded separately, so advancing delivery refs alone need not invalidate unchanged checkout content.

### Fixed

- Recovery preservation no longer performs an exact-content landing-history search. It still verifies complete committed and uncommitted content, branch identity, archive integrity, live use and retained Git prerequisites before removal.
- Human recovery plans explicitly say when landing was not evaluated. Legacy archive schemas and frozen digest formats remain readable.

## [0.8.42] - 2026-10-28

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0842).

### Added

- Eligible coordinated Git fetches and pushes use exact ref lanes while retaining common-directory and remote compatibility locks. Unknown forms remain in the conservative repository-wide lane.
- Strict fetches retain provider-completion witnesses. Recovery can accept a completed fetch only when its recorded process, repository, configuration, and current local/remote refs prove the outcome; uncertain outcomes remain blocked.
- Lease diagnostics show bounded changed-line ranges and grouped owners. Explicit claims, notes, and release guidance support shared-edit handoffs without inferring semantic independence.
- Git lock waits identify their lane, holder evidence, and next actions where available.
- Deployment and upgrade plans can preview bounded generated-file diffs with raw-content digests and escaped terminal output.

### Fixed

- Releasing an explicit path claim preserves same-path implicit lease observations.
- Host database contention respects the remaining admission deadline, and scoped Git operations preserve parent/resource crash barriers.
- Broker operation test fixtures quote paths containing spaces and apostrophes and keep timed-out providers owned and reaped.

### Validation

- Adds a warning-only paired Playground matrix for broker actions. Five local pairs showed no warning-band regression in ten comparable actions, but no broad speedup; routine status remained above its 150 ms warning budget. Strict fetch and complete diff evidence have no equivalent baseline comparison.
- Hosted workspace qualification passed 6,200 tests with 37 skips before this metadata-only release update; focused native qualification passed 225 tests. Release metadata and artifacts are checked separately by the release workflows.

## [0.8.41] - 2026-10-27

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0841).

### Added

- `aethyme broker commit --session <id> -- <git-commit-args>` runs a local commit under its session lock, preserving Git hooks and ownership checks without holding the repository-wide operation lock throughout the hooks. Existing `broker advanced git` behavior is unchanged.
- Installation health diagnostics explain when a private host-state override separates host resource leases and capacity from other installations.
- Session-start diagnostics identify when the running CLI build differs from PATH's selected router and list other known installation roots without executing their binaries.

### Fixed

- Unattended worktree cleanup and finish recovery bind ownership to a random generation marker created with the checkout. A replacement directory cannot inherit deletion authority merely by reusing the former directory's device and inode. Missing or changed markers retain the checkout.
- Orphan checkout removal preserves unexpected files, links and modified tracked content rather than treating lost Git metadata as permission to delete arbitrary residue.
- Shared-cache discovery bounds its search for an eligible owned cache; unrelated or foreign entries do not grant cache ownership.
- Unknown-operation guidance names the original operation to inspect and reconcile, rather than the currently blocked request.
- Release manifest and Homebrew helpers are built alongside the Linux binaries and handed to the attach job with source and checksum checks, avoiding a second cold helper build.

### Internal

- Adds managed-service lifecycle groundwork; automatic service teardown and the complete managed-service controller remain unimplemented.

## [0.8.40] - 2026-10-27

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0840).

### Changed

- `aethyme broker submit prepare --json` advances its report schema to 2.
  A time- or entry-limited cache inventory can trigger whole-store rotation;
  its report exposes a nullable exact `bytes_before`, inventory completeness,
  a lower bound and typed reason. Configuration and preparation-state schemas
  remain version 1.

### Fixed

- Explicit lease claims no longer refresh implicit leases and overlap diagnostics
  across every live checkout before checking the requested path.
- Cleanup budgets landing-proof work by actual query batches, so an early exact
  proof can complete without requiring the full candidate/path window to fit;
  exhausted evidence still refuses cleanup.
- Shared-cache inventory renews its host resource lease and rechecks storage
  during a bounded 30-second traversal. Time- or entry-limited inventories can
  rotate eligible owned caches; other incomplete inventories or failed lease
  checks still refuse.

## [0.8.39] - 2026-10-27

### Changed

- Guarded execution no longer refreshes implicit leases and overlap diagnostics across every live checkout before each command. Before/after dirty snapshots, artifact-use admission, and the post-command ownership audit remain; that audit still uses explicit leases, current holder policy, and the foreign-file snapshot. The global overlap-diagnostic timestamp remains tied to the last actual refresh.
- SQLite sidecar guard failures now identify the first failed metadata predicate at each existing guard stage. Existing refusal conditions and error prefixes remain unchanged; this improves diagnosis without changing race-handling behavior.

### Tests

- The process-handoff test fixture writes watcher output to a temporary sibling and renames it after the copy completes, so observers see complete output rather than a partial file.

## [0.8.38] - 2026-10-27

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0838).

### Fixed

- Cleanup inspects leases for its candidate sessions, including retained leases on closed sessions, and preserves unknown holders.
- Recovery bundle capture streams into a bounded file with a hard 1 GiB cap. Manifest schema 3 verifies bundle length and SHA-256. Destination admission includes this enforced cap when a bundle is needed; allocation overhead and concurrent writes remain outside the estimate.
- Closed-session artifact sweeps acquire an exclusive per-checkout guard before recording deletion intent or removing files. Busy or uncertain producers retain that candidate while unrelated candidates can progress. Open-session artifact reclamation remains disabled.
- Cleanup status and archive untracked-path inventories have output and path-count caps. Cleanup proof rechecks share the inspection deadline; incomplete observations retain the checkout.

## [0.8.37] - 2026-10-27

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0837).

### Fixed

- Session finish reads the relevant submission records and lease state instead of decoding unrelated queue history.
- Recovery archives preserve protected standalone ignored files, links and Unix file modes, and verify the complete durable archive before removing the source checkout.
- Recovery patch capture, manifest serialization and file copying have explicit bounds. Archive admission checks free space on the destination volume against known payload sizes plus the storage reserve; bundle size, filesystem allocation overhead and concurrent writes remain outside that estimate.
- Cleanup requires exact changed-path tree entries before accepting patch-equivalence evidence as proof that work landed.

### Added

- Managed preparation, gates and guarded commands record durable artifact use. Unknown producers remain protected; this does not enable automatic artifact deletion from open worktrees.

## [0.8.36] - 2026-10-27

### Fixed

- Cleanup discovers ignored directory roots through a bounded Git listing rather than recursively enumerating their contents. Oversized, incomplete or malformed listings retain the checkout.
- Cleanup keeps worktrees with dirty tracked submodules even when local Git display settings hide their changes.

## [0.8.35] - 2026-10-26

### Fixed

- Shared Codex app-server processes no longer count as individual task holders. Legacy bindings retain ownership safeguards and require explicit takeover rather than allowing the shared daemon to prove task activity.
- Cleanup reports distinguish closed sessions from physically removed worktrees, including when delivery or process checks retain a checkout.
- Full cleanup audits reuse complete landing evidence within the command, avoiding repeated history queries while retaining fresh ownership and liveness checks.
- Archive cleanup refuses protected ignored residue that the archive does not preserve, and rechecks this protection before checkout removal.

## [0.8.34] - 2026-10-26

### Fixed

- Adoption and reuse skip submission-provenance hashing when a session is behind or diverged from integration, preserving drift guidance while avoiding unnecessary history work under the lifecycle lock.
- When automatic checkout removal is enabled, general artifact sweeps reserve part of their existing time budget for whole-checkout cleanup so cache discovery cannot consume the entire pass first. Cleanup ownership, delivery and liveness checks remain in force.
- Default-branch refreshes have a bounded 30-second total deadline covering preparation, lock admission and Git execution. Fresh starts also honor a shorter configured Git timeout, and timeout diagnostics distinguish general limits from caller caps. Unknown write outcomes still require reconciliation.

## [0.8.33] - 2026-10-26

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0833).

### Fixed

- Closed-worktree cleanup treats every Git-ignored directory as disposable by default, without recursively inspecting generated dependency fixtures, databases or symlinks. Tracked changes, nonignored files, unpublished commits, active owners and the primary checkout remain protected.
- Finish-time and interrupted cleanup recheck ignored-content policy. Artifact-root links are classified without following their targets.

### Added

- `[cleanup] strict = true` preserves all ignored leftovers and pauses automatic ignored-artifact sweeps, including queued removals. Remove residue manually or review an explicit discard when exact parity is required.

## [0.8.32] - 2026-10-26

### Changed

- Broker operating guidance documents the existing configurable checkout cap, its fetched-default-branch authority, and targeted cleanup of closed checkouts. Runtime behavior and database compatibility are unchanged from v0.8.31.

## [0.8.31] - 2026-10-26

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0831).

### Fixed

- Routine text, compact and summary status use a read-only observation path instead of repeating startup maintenance; successful Git identity and configuration reads are reused within the invocation.
- Verify-only worktree startup selects the freshly fetched default branch without reconciling historical integration promotions. Native allocation remains independent of the general Git write lock held by unrelated commit hooks.
- Startup cleanup pages historical sessions, resumes deferred work fairly, and preserves spawned-worktree identity and uncertain process-use protections.
- Checkout and preparation-cache reclamation validate repository-root ownership, retain active users, reject incomplete liveness scans, and serialize root deletion with worktree allocation.
- Fresh-base diagnostics retain fetch failures and actionable recovery guidance; guarded execution records bounded helper observations after caller loss.
- Graph consumers retain revision-bound documentation sections, typed import endpoints and addressable configuration references, including after checkout relocation.
- PR observation bounds provider work and preserves legacy commit-status failures and merge-state changes. Explicit opt-in service plans bind their runtime identity and installed generation.

### Changed

- Broker schema 57 adds worktree-root binding, guarded-exec receipts, base-failure incidents and helper observations. Root binding can raise the stored compatibility floor to 54; review the upgrade procedure before changing databases or runtimes.
- Shared preparation caches follow the selected worktree volume. Storage admission and bounded cleanup retain conservative ownership and rollback rules.
- Performance diagnostics retain workload, binary and source identity; latency budgets remain warnings rather than ordinary test timing assertions.

## [0.8.30] - 2026-10-22

### Fixed

- Git batch queries use bounded streaming, enforce active deadlines, and reject incomplete or truncated results.
- Submission planning reuses patch identities for identical commits shared by the session and integration candidate batches, preserving all provenance matches.
- Cross-process contract CI matches the trusted Rust cache inputs while keeping pull-request cache use read-only.

### Changed

- `aethyme broker advanced exec` adds an explicit `--non-interactive` opt-in; default interactive execution remains unchanged.

## [0.8.29] - 2026-10-22

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0829).

This patch includes the broker inspection, verification-identity and cleanup
repairs described in v0.8.28. Upgrades from v0.8.26 migrate each repository's
broker database from schema 49 to 52; databases already at schema 52 need no
additional migration.

### Fixed

- Agent startup consumes already-available readiness bytes without sleeping
  between bytes; deadline, token and message-size checks remain enforced.
- Successful deployment no longer repeats the full readiness scan after
  verification. It prints the explicit `aethyme broker status readiness`
  follow-up for the deployed repository.
- Coordinated provider commands enforce their deadline while waiting for
  inherited output pipes, including after the provider process exits.
- Coordinated command capture retains at most 16 MiB per stream, keeps
  draining longer output, and marks truncation so consumers reject incomplete
  evidence.
- Worktree Cargo defaults are synced before publication and preserve an
  existing configuration without replacement.
- Ordinary coordinated GitHub reads use their own shorter admission budget;
  explicit caller budgets remain authoritative.
- Submission preflight reuses its invocation-local plan when its exact inputs
  remain unchanged, and rebuilds it when ownership or history is uncertain.
- Cleanup reuses one target snapshot while retaining recovery and identity
  checks.
- Rust formatting selects the containing Cargo package's edition.
- Release signing recovers equivalent transparency-log entry conflicts and
  verifies the published standard signature bundle before publication.
- Status and blocker summaries exclude stale explicit lease observations whose
  holders no longer block; path ownership and incomplete-inspection warnings
  remain visible.
- Analysis views validate producer profiles, input generations and result
  status before using derived facts. Unsupported or partial producer output
  cannot establish an absence claim.
- Failed Git commands preserve bounded diagnostic messages, and merge
  simulations retain deadlines while inherited output pipes are open.

### Changed

- Repository owners can configure `[broker.pr] open_as = "draft" | "ready"`
  on the trusted default branch for newly created session pull requests. Draft
  remains the default; existing pull requests keep their current state.
- Start and adopt can report offline PR overlap advice for explicit planned
  paths. Missing, stale, unknown or truncated evidence remains incomplete;
  this advice does not claim a fresh network inspection.
- Performance checks include a narrow CI lane and an optional isolated
  Playground workload with aged, released resource-lease history. These are
  measurement and validation tools; no end-to-end latency gain is asserted
  before combined-runtime measurements.

## [0.8.28] - 2026-10-21

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0828).

By default, each repository's broker database advances from schema 49 to 52 on
its first writable open. Broker status and guarded coordination paths do less
repeated inspection, and verification and gate records retain the identity
needed to avoid reusing stale state.

The v0.8.27 tag did not produce published release artifacts because its release-only
version check expected the previous identity format. This release includes the
repair and the changes listed below.

### Changed

- Verification candidates are stored as typed queue records, and cached gate
  results are bound to the execution-profile digest. A result without a
  captured profile is not reused.
- Routine broker status, resource-blocker, and guarded-execution paths reduce
  repeated liveness and historical-owner work while retaining lease ownership
  and conflict decisions.
- Broker recovery and local coordination workflows keep revision-bound
  evidence, check recorded ownership before acting, and report incomplete
  inspection as unknown.
- Submission planning skips integration patch-history scans when session
  commits have no patch identities, while preserving ancestry and ownership
  classification.

### Fixed

- Cleanup plans use a stable daily retention anchor and exclude their own
  telemetry, so inspection does not immediately invalidate the reviewed plan.
  Existing schema-3 cleanup journals retain their original resume semantics.
- Release identity checks validate the exact tagged commit in the current version
  output, and release checkouts do not persist credentials that mark builds dirty.

## [0.8.27] - 2026-10-21

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0827).

By default, each repository's broker database advances from schema 49 to 52 on
its first writable open. Broker status and guarded coordination paths do less
repeated inspection, and verification and gate records retain the identity
needed to avoid reusing stale state.

### Changed

- Verification candidates are stored as typed queue records, and cached gate
  results are bound to the execution-profile digest. A result without a
  captured profile is not reused.
- Routine broker status, resource-blocker, and guarded-execution paths reduce
  repeated liveness and historical-owner work while retaining lease ownership
  and conflict decisions.
- Broker recovery and local coordination workflows keep revision-bound
  evidence, check recorded ownership before acting, and report incomplete
  inspection as unknown.
- Submission planning skips integration patch-history scans when session
  commits have no patch identities, while preserving ancestry and ownership
  classification.

## [0.8.26] - 2026-10-08

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0826).

`submit` now refuses a session whose checkout has drifted from its recorded branch. Lock and lease waits can be seen from other terminals, `unblock` suggests the step that actually clears a lease, and the broker-open cleanup sweep stays within its deadline. A dependency security advisory is fixed. The broker database stays at schema 49.

### Changed

- **Breaking:** `broker submit` refuses a session whose checkout no longer matches its recorded branch, for example a detached HEAD left by a stuck rebase or a different branch checked out. It checks again after planning, and a refusal leaves no queue entry. Sessions adopted on a detached checkout are unaffected. `status --refresh` reports such a session with the blocked advice `session.checkout-drift`, and keeps counting its dirty files (#645, issue #640).
- **Breaking:** a `finish` timeout now reads `finish timed out after <n>s waiting on Git (…)`, not `… while checking Git`. It reads the session back and either gives the retry command (`finish --timeout <2x>`) or says the session is already closed and must not be retried (#641, issue #494).
- `status --json` and `status --summary --json` gain an optional `waiters[]` array listing current waits on the coordinated write lock, gate owner locks and `leases wait`: `pid`, `session_id`, `kind`, `resource`, `holder`, `waited_ms` and `alive`. The human view adds a `Waiting on locks and leases:` section. The next waiter prunes the records of dead waiters (#641, issue #494).
- `broker unblock` names the command that actually clears a stale session's lease: `git status` for uncommitted changes, `submit` (or `representation scan`) for undelivered commits, `push` for commits on no remote, and `finish` only when nothing is pending. The `status` listing derives that advice once per session, within 2 s (#646, issue #637).
- The internal pilot materials describe internal dogfooding, with a dated snapshot and a follow-up report on conflict recovery and the install, update, rollback and uninstall lifecycle (#639, #647, issue #126).

### Fixed

- Auto-cleanup removal in the sweep run on every broker open now honours the selection deadline. The first selected checkout is still removed, the rest are deferred to a later pass, and every safety check is re-read under the lock (#642, issue #460).

### Security

- `noyalib` moves from 0.0.24 to 0.0.53, fixing RUSTSEC-2026-0333 (parser resource budgets not enforced on typed deserialization). The broker uses the default budgets, so its behaviour is unchanged (#644).

## [0.8.25] - 2026-10-08

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0825).

A verified queue entry can ship on its own, Windows x64 gets a native release, Linux arm64 is validated natively, and the broker bounds its slow paths: `status` inspection, `finish` and lock waits stop at a deadline and say what they did not check, instead of hanging. The broker database stays at schema 49.

### Changed

- **Breaking:** the `status` and `status doctor` inspection budget drops from 15 s to 7 s, and `AETHYME_STATUS_INSPECTION_BUDGET_MS` can only lower it, never raise it. `broker advanced worktrees` and `gc plan` stop at the same shared budget and report what they did not reach: a checkout not inspected is a `state: "not_inspected"` row counted as work at risk, `deferred_checks` names the cut checks, and a partial `gc plan` reports `budget_verdict: "unknown"` (#625, issue #460).
- **Breaking:** `broker finish` applies one deadline (default 10 s, `--timeout <1..86400>`) to the Git checks that decide whether to close, and reports the timed-out check instead of waiting indefinitely; the session is kept for explicit recovery. Opening the broker runs before the deadline and worktree removal after close runs without it. Lock waits print the holder and elapsed time, and `status --json` reports pending lease-release requests and unresolved coordinated operations (#616, issue #494).
- `broker ship plan|execute --entry <id> --only` ships one verified queue entry ahead of unshipped entries before it, when git proves it independent of them (#619, issue #369).
- Windows x64 ships as a native release archive beside the signed manifest, with its own checksum and signature. Broker commands refuse explicitly on Windows; macOS and Linux are unchanged (#629, issue #357).
- The Linux arm64 release is validated natively: install, version, upgrade and uninstall (#627, issue #356).
- `broker gc storage` reports which session owns each broker branch; the default and integration branches are reported as `shared` and never removable (#630, issue #335).
- The graph indexes explicit non-code relationships (Markdown links to source, include directives, config references), with qualified names, per-language test edges and a per-file cap (#623, issue #213).
- Reproducible product latency benchmarks against a pinned fixture, with a checked-in baseline (#615, issue #386).
- `broker.rs`, `store.rs` and `operations.rs` are split into modules, as pure moves (#633, #634, #635, issue #382).

### Fixed

- `broker push` runs Git, and so the repository's pre-push hook, from the session worktree instead of the main checkout, and surfaces the hook's output when a push is refused (#626, issue #621).
- `operations reconcile` and `unblock` accept host operation IDs and refuse one that belongs to another repository (exit 3), and a failed gate reports the tail of its log (#628, issue #284).
- The cleanup runbook and the pilot guide match the shipped behaviour (#631, #632).

## [0.8.24] - 2026-10-08

Agents can subscribe to a pull request or to every pull request of a repository, and receive a review prompt when one opens. A brokered push can run the review rules for its own pull request. Disposable checkouts are removed automatically. Tests run the same way locally and in CI. The broker database moves to schema 49 additively: 0.8.23 still opens it.

### Changed

- `aethyme broker advanced watch repo start|list|show|events|pause|resume|stop` subscribes a session to every pull request of a repository (`--include-drafts`, `--include-existing`, `--exclude-authors`, `--auto-watch`). Each new, ready or reopened pull request emits one event. `deliveries subscribe --repo-watch <id> --policy review|notify` sends the subscribed agent a review prompt rendered from `[watch.prompts.review]`, with pull-request text quoted as untrusted data (#611, issue #606).
- `[review] run_on_push = true` (off by default) runs the review rules for a session's open, non-draft pull request right after `broker push`. The run is bounded by `run_on_push_budget_secs`, best-effort, and reported as `review_run` in `push --json`. Rule comment templates gain `{{head}}`, the full head SHA (#612, issue #610).
- The unattended sweep removes a broker session checkout automatically when every owning session is terminal, no process, lease or gate uses it, it holds no local changes or valuable ignored files, and its commits are contained in the remote default branch. Each removal is journaled with its proof. A worktree with no recorded owner, the main checkout and `[cleanup] keep` pins are never removed. `[cleanup] auto_remove = false` turns it off (#605, issue #588).
- Coordinated operations record which agent process ran them (pid, start time, program name, session). This is evidence, never authorization, and is shown by `operations show` (#613, issue #393).
- `packages/aethyme/scripts/test-like-ci.sh` runs the test suite exactly as CI does (`--changed` or `--full`), and CI calls it (#600, issue #598).
- The graph indexes TypeScript call edges (#608, issue #213).

### Fixed

- The `gh` guard lets `pr close <N>` and `pr reopen <N>` through without acknowledgement (#607).
- `broker submit` refuses an unsupported trailing subcommand such as `status` before doing anything, with exit 2 (#609, issue #595).
- `quality fix` no longer silently overwrites one fixer's change with another's on the same file (#614, issue #381).
- The GC confirmation test and the gate-label test no longer flake (#601).
- The broker opens a database that an unmerged development build migrated under a reused version number: migrations from v48 on are idempotent, and a repair pass recreates any v48+ table or column that is missing (#617).

## [0.8.23] - 2026-10-07

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0823).

The broker measures pull request size and risk and can act on it through configuration, protects the default and integration branches, guards `gh` branch writes across sessions, and gains a cross-store doctor, lease liveness and handoffs, and a verified self-update.

### Changed

- **Breaking:** brokered `gh` commands that can write or delete a branch ref are guarded. Only an exact allowlist of non-ref writes runs without acknowledgement: issue and PR comments, reviews, labels and issue edits. `pr merge` is checked against session ownership, with `--match-head-commit` injected. Anything else that may write a ref is refused unless `--destructive --ref-write-acknowledged` is given. `gh` runs from a trusted directory with a fixed `PATH` and environment (#566, issue #393).
- **Breaking:** `main` (resolved from `origin/HEAD`) and `aethyme/integration` can no longer be deleted, force-updated or rebased through `broker advanced git|gh`. Plain pushes and merges into them still run. `start --adopt` refuses the repository's main checkout unless `--allow-main-checkout` is given (#594, issue #284).
- The broker measures each pull request's size, risk signals and tier (`trivial`, `normal`, `large`; risk `none` through `critical`) in `review plan`. With `[review.projection]` enabled it flags them with `aethyme/size:*` and `aethyme/risk:*` labels and does nothing else by default. `[review] pr_size` sets the size thresholds, and `broker push --pr` warns above them (#589, issues #584 and #239).
- Review rules can condition on tier, size and signals, and can act deterministically:
  - `waive`: a waiver bound to the head, with guardrails the broker enforces;
  - `labels`;
  - `comment`: a pull request comment rendered from a `[review.comments.<name>]` template with broker-computed variables only, edited in place and never re-posted unchanged.
  (#593, #597, issues #584 and #596).
- `broker status doctor plan|apply --confirm <digest>` reports debris across stores and removes only resources whose owner is verifiably dead. Gate commands receive `AETHYME_GATE_RUN_ID` and `AETHYME_GATE_LABELS` to label what they start (#583, issues #287 and #286).
- Lease liveness is bound to the holder process (`active`, `idle`, `stale`, `released`, `unknown`), with a configurable stale grace period. A verified `finish` releases its leases. `leases request-release`, `ack`, `decline` and `wait` coordinate handoffs, with an automatic grant only against a stale holder (#576, #578, #579, issues #360, #358, #359 and #191).
- `aethyme self-update` installs the verified release pair atomically. `broker status` raises `install.replaced` when the installed build changes under a session (#577, issues #293 and #251).
- In verify-only repositories, `aethyme/integration` is a disposable verify base. It is refreshed automatically when it holds nothing of its own, and `broker ship` refuses there (#575, #581, issues #352 and #369). `integration reconcile` accepts `tracked_elsewhere` for work preserved on a pushed branch or an open pull request (#582, issue #464).
- A sound `Stale` graph-integrity verdict advises on submit and on brokered merges, and never blocks (#574, issues #280 and #292).
- Brokered pushes tell hooks their shape through `AETHYME_PUSH_KIND` and `AETHYME_PUSH_REFS`, and a delete-only push skips the broker's own dry run (#573, issue #264).
- Broker commands fast-forward a clean main checkout that is strictly behind. They never do so under a live session or an in-progress Git operation (#591, issue #232).
- `gates affected` records per-phase timings in its metric. `--timings` prints them as JSON (#585, issue #481).

### Fixed

- Every "already landed" verdict compares patches with `git patch-id --verbatim`, so a whitespace-only difference is no longer taken as landed (#586).
- Landing proofs try ancestry against every delivery target before any content or patch search (#592, issue #588).
- Review projection writes (comment edits) use the exact repository path the ref guard allows (#599).
- The nightly workflow publishes workspace coverage (#587, issue #380).

## [0.8.22] - 2026-10-06

Broker status, worktree listings and brokered reads return within a bounded time on large hosts. A provider's definitive refusal no longer write-blocks the repository. A re-adopted worktree keeps the commits it still owes. The Homebrew tap is published from a reusable, retryable workflow.

### Changed

- `broker status --refresh` and `status doctor` run per-session inspection in parallel and batch drift ancestry and patch ids into a few Git calls, under one 15 s inspection budget (`AETHYME_STATUS_INSPECTION_BUDGET_MS`). Anything the budget cut is named in `deferred_checks` and reported as unknown, never as clean. On a 65-session repository `status --refresh` went from 150 s to 44 s (#570, issue #460).
- `broker advanced worktrees --json` sizes checkouts from the recorded size cache or under a 10 s budget, and inspects checkouts on up to 8 threads; rows not measured in time report `size: "unmeasured"` and are counted in `unmeasured_count`. `--measure` walks every checkout as before. 225 worktrees went from 187 s to 24 s (#563, issue #559).
- Brokered read-only `gh` and `git` operations without a caller budget stop after 90 s (`AETHYME_BROKER_READ_BUDGET_SECS`) and exit 6 with the time split between preparation and provider wait; nothing needs reconciling. Long-running reads (`run watch`, `--watch`, `run view --log`, downloads, `api --paginate`) keep an unbounded wait (#562, issue #555).
- Re-adopting a worktree after `broker close` carries the closed session's ownership boundary forward while its commits are still pending, so `submit` replays them; `adopt --json` reports `carried_ownership`. `finish` on such a session now refuses and recommends `submit` (#557, issue #294).
- Load-admitted gates scale their deadline by the one-minute load per CPU at start (1x floor, capped by the admission threshold and 4x), and timeout diagnostics record the load at start and at timeout (#552, issue #413).
- `aethyme explore` warns when it reads a checkout that is behind `aethyme/integration` or the upstream branch: a stderr line, a `source_staleness` object in JSON output, and `safe_to_use_as_answer: false`. Generated agent guidance says to read code in a session worktree (#571, issue #232).
- Empty delivery polls no longer write command metrics or events, and the PR monitor starts the delivery adapter only when the outbox has work (#561, issue #417).
- `broker start` and `adopt` without a tab name skip the Chau7 tab lookup and report `tab_rename.status: not_applicable` (#550, issue #496).
- Stable releases publish the Homebrew formula through `homebrew-tap.yml`: it verifies the signed manifest, writes once with the formula's current blob SHA as precondition, and reads the result back byte for byte. Re-run it with `workflow_dispatch` to retry; pull requests that touch the path rehearse it read-only (#553, issue #253).
- `quality inspect` skips the standard temp roots in `relative-links` and files marked `linguist-generated` in `.gitattributes`; `--include-generated` brings them back. This repository's findings went from 56 false positives to 0 (#565, issue #384).

### Fixed

- A definitive GraphQL refusal of `gh pr merge` or `pr update-branch` that names the command's own mutation (for example auto-merge disabled) is recorded as a failure, not as an unknown outcome that write-blocks the repository (#558, issue #549).
- A killed coordinated write's two blockers name each other, carry an inspect command, and list the ordered recovery (inspect the remote, record the outcome, reconcile the lease); clearing one half reports what still blocks (#558, issue #286).
- A refused local pre-push hook or failed coordinated Git operation keeps a bounded, redacted stderr tail in its operation record (#551, issue #415).
- `aethyme-enhance` has its first integration suite, covering deploy idempotence, file modes, overrides, hand edits, policy rendering and the symlink guard (#568, issue #381).

## [0.8.21] - 2026-10-06

Broker sessions belong to the agent process that holds them. Health checks finish on large hosts. Gates that run out of disk space or inodes are reported as host faults.

### Changed

- A broker session is bound to the agent process that started or adopted it. A second process that names the session is refused with the current holder's identity, unless it passes `--take-over`, which is recorded as an event. `broker status --json` warns with `session.foreign-process` when another process works inside a session's worktree. Agents that used to re-enter a session another process holds now need `--take-over` (#542, issue #393).
- A destructive `broker advanced git` write to another live session's `agent/*` branch is refused unless it names that session with `--cross-session <id>` (#531, issue #393).
- Before running gates, `broker submit` checks that the pending commits declare a contract decision whenever the diff touches a tracked cross-process symbol, so a missing `Contract decision:` line no longer surfaces only after a full gate run. The decision must be in a commit message; the gate never sees a PR body (#536, issue #418).
- The cost-1 contract gate is admitted by host load, so a loaded machine queues it instead of timing it out (#529, issue #413).
- Promotion commit subjects are capped at 72 characters. A multi-line task contributes only its first line (#528, issue #493).
- `aethyme deploy` installs the `aethyme-explore` wrapper for Claude as well as Codex (#533, issue #385).

### Fixed

- `broker status doctor`, `certify` and the verify loop no longer run for more than 10 minutes on hosts with hundreds of retained worktrees. Cleanup eligibility and the build-artifact scan each get a 10 s budget; worktrees not judged in time are listed and counted (`eligibility_not_inspected_count`, `artifact_worktrees_not_scanned`) instead of blocking the report (#530, #535, issue #460).
- A gate that runs out of disk space or inodes after it starts is reported as a host fault and is not cached as a test failure (#539, #545, issues #288 and #471).
- Disk headroom accounts for inodes as well as bytes. Filesystems that report no inode counts (btrfs, unlimited tmpfs) are treated as unknown and never refuse gates (#545, issue #471).
- `broker submit` checks the provenance of a long branch in one batched pass: 1,500 commits went from 140 s to 0.35 s (#543, issue #463).
- `broker gc storage attribute [--apply]` attributes unmarked worktree roots to their repository from Git evidence (#541, issue #257).
- `aethyme deploy` verifies `.aethyme/` path references in generated files against the tree (#534, issue #289).
- `broker advanced leases plan` honours lease ignore rules, and lease records report their scope (`exact` or `recursive_directory`) (#538, issue #366).
- Gate selection (`gates affected`) no longer waits on the graph-integrity lock (#540, issue #481).

## [0.8.20] - 2026-10-05

Cleanup explains a shallow clone instead of failing with an empty error.

### Fixed

- In a shallow clone, `broker finish cleanup` could not relate a session older than the shallow boundary to its delivery target, and reported `inspection_failed` with an empty `git merge-base … failed:` message. The reason now says the repository is a shallow clone and gives the `broker advanced git … fetch --shallow-since=<date>` (or `--unshallow`) command to deepen it. The disposition is unchanged and no safety check is loosened (#526, issue #525).

## [0.8.19] - 2026-10-05

Cleared gate failures are kept as history instead of deleted, and broker runtime files are ignored from one catalog.

The broker database schema advances from 46 to 47, and the minimum compatible schema rises from 42 to 47. This is not a compatible migration: once 0.8.19 opens a repository's broker database, older binaries refuse that repository, because they ignore `cleared_at` and would serve a cleared failure as a cached verdict. Upgrade every installed `aethyme`, including tree-built gate binaries, before using 0.8.19 on a shared repository; rolling back below 0.8.19 is not supported.

### Changed

- `broker unblock gatecache:` marks failing gate results cleared, with a time and reason, instead of deleting them. Cleared results no longer feed cache lookups, blockers or recommendations, and a clear stops the cache from falling back to an older pass for that tree, so the next run executes the gate. Gate result ids are never reused. `broker.blocker.cleared` events keep the `removed_gate_result_ids` key, which now names the retained, cleared rows (#523).
- Broker runtime paths come from one catalog, which feeds the generated `.gitignore` block, `ship`'s untracked-file check and the paths `aethyme deploy` reports. The block now also ignores `gc.lock`, `gc-journal.json`, `worktree-sizes.json`, `reviews/` and `locks/` (#521).

### Fixed

- Scaffold refuses, and certify warns about, a `.gitignore` whose `# aethyme-broker:begin` marker has no end marker, instead of appending a second block that a later run would replace together with the rules in between (#521).

## [0.8.18] - 2026-10-04

A contract decision accepted on a pull request is accepted on its merge commit too, and gates on an overloaded host defer instead of failing.

### Added

- `aethyme broker check-contract` takes `--commit-messages`, which reads the `<base>..HEAD` commit messages itself, and `--merged-pr`, which asks GitHub for the pull requests that merged HEAD and reads their bodies. The lookup runs only when no other source declares a decision that passes (#517).

### Changed

- A load-admitted gate whose host stays above its load threshold for the whole `resource_wait_seconds` is deferred as `error/resource_contention`, with the measured wait, load, CPU count, threshold and retry guidance, before it spawns or takes owner locks or resource leases. The result is not cached, so a later retry runs. A gate that starts and fails an assertion is still a test failure (#499).

### Fixed

- The `cross-process-contract` gate on the default branch reads the merged pull request's contract decision, so a pull request that declared its decision only in its body no longer turns the branch red after merging. The pull request check also reads commit messages, so both lanes judge the same sources. A change with no decision anywhere still fails, and a lookup that cannot run is reported with its cause and never counts as a pass. Every refusal lists where it looked (#517).

## [0.8.17] - 2026-10-04

Coordinated writes that GitHub refuses no longer block the repository, and sync, read-only refusals and doctor say what happened and what to do next.

### Added

- `aethyme broker doctor` reports `integration.leftover-work` in verify-only repositories, using the same detection, evidence and `broker advanced integration reconcile --dry-run` next action as `broker status`. It reads refs only and never makes doctor unhealthy (#513).

### Changed

- `broker sync` says what it merged into what (or what it rebased onto), that nothing was pushed, and to run `aethyme broker push` next. JSON gains `next_action` (#514).
- Commands refused in read-only compatibility mode, such as `broker status --refresh`, name the repository's compatibility state and the remediation instead of only the mode (#512).

### Fixed

- A coordinated `gh` write that GitHub definitively refuses (exit status 1 for a single-mutation command with an HTTP 4xx rejection or a known refusal such as "Cannot update PR branch due to conflicts") is recorded as failed and no longer write-blocks the repository. Killed processes, network errors, 5xx, 408 and 429 stay `outcome_unknown` (#515).
- The graph-task skill no longer repeats its task, context-pack and verification sections (#510).

## [0.8.16] - 2026-10-03

Prompt status, earlier warnings about disk, hooks and leftover integration work, safer coordinated operations, and tools for landing and auditing work.

### Added

- `aethyme repo branches audit` classifies every local branch as on the remote, contained in a remote branch, merged through a pull request (by patch equivalence, even when SHAs differ) or local-only, lists the commits that exist only locally, and inventories worktrees. It is read-only: remote state comes from `git ls-remote` and no refs move (#502).
- `broker advanced ownership claim|release|list` names who is driving work that has no file path, such as a release. `broker status` lists active claims with the holder's last coordinated operation; a claim is refused only while another session holding it is working (#507).
- `broker advanced merge-chain` lands pull requests in order: it marks drafts ready, updates branches, waits for the latest run of every check on the exact head, merges with `--match-head-commit`, and requires the base branch's runs on the merge commit to succeed before continuing. It stops with a next action on the first failure and resumes by skipping merged pull requests (#508).
- `broker push --pr` fills the repository's pull request template from the commits' Problem, Decision, Rationale and Validation sections, and notes when the repository's CI skips draft pull requests (#506).

### Changed

- Routine `broker status` reports recorded observations and names the checks it did not inspect instead of scanning dirty worktrees and cleanup history; `--summary` reads only the ledger and `--refresh` keeps the full audit. JSON exposes freshness, incomplete inventory and phase timings (#500).
- `broker status` warns with `host.disk-low` below twice the gate headroom threshold and points to `gc reclaim plan`. Reclaim keeps build output in worktrees that have open files or a git index changed in the last three hours (#504).
- `broker status` reports `integration.leftover-work` in verify-only repositories when the integration branch holds commits the published branch lacks, with `broker advanced integration reconcile --dry-run` as the next action (#503).
- `aethyme doctor` reports a `core.hooksPath` that makes git skip every hook (missing, not a directory, or without executable hooks while the repository ships a hooks directory), and `aethyme deploy` warns about it. Neither changes git configuration (#505).

### Fixed

- Coordinated `git` and `gh` operations refuse a `--repo` that is not one of the session worktree's remotes. Session ids are numbered per repository, so an id resolved in another checkout no longer authorizes an operation there. Success output names the session worktree (#501).
- Refusals for a missing `--destructive` or `--repo` print the corrected command (#506).

## [0.8.15] - 2026-10-03

Faster broker commands, visible submit progress, local throughput reports, and more reliable code graphs.

### Added

- `broker advanced insights` reports the session funnel, recorded active time alongside elapsed time, PR lifetimes, gate reliability and coordination counts, without identifying people. JSON output is versioned; percentiles require at least five samples (#487).
- `broker submit` reports its phase, wait position, owner-lock holder and last progress on stderr. `broker status` exposes in-flight submits and warns about dead processes or five minutes without progress (#491).
- `aethyme init` drafts `.aethyme/prepare.toml` for supported Node and uv projects without executing or overwriting it. Shared preparation caches can have a size budget; `gates doctor` detects fixed ports and shared resource names (#461).
- JavaScript files are indexed and Rust calls produce caller/callee edges, including `crate::` calls. Graph navigation accepts `--repo`, and `--json`/`--json-output` are interchangeable (#459).

### Changed

- Opening the broker spends at most 250 ms on artifact cleanup. Interrupted passes resume at most once every ten minutes; `broker finish` and the new `broker gc sweep` perform larger sweeps. Remembered paths must stay inside the worktree, contain no parent traversal or symlinks, and name an allowed build artifact (#490).
- Overlap refresh classifies each participating session once, skips inactive sessions in routine refreshes while retaining their leases, and defers unfinished pair classification after a ten-second budget checked between operations. Submit still checks its overlaps with inactive holders (#492).
- Lease claims inform rather than block; only an explicitly leased path held by a session actively doing work can refuse a claim (#489).
- Cleanup representation reads batch Git object queries instead of forking once per path, while preserving comparisons of files, trees and submodule pointers (#467).
- Graph freshness compares recorded commits with HEAD instead of file timestamps. The derived graph-store schema advances to 9 for line-qualified node IDs, so older stores need rebuilding. Unknown flags and missing targets are reported as errors; usage errors exit 2 and an unhealthy text-mode `graph status` exits 1 (#459).

### Fixed

- Failed broker command events retain a redacted error message so diagnostics explain the failure without exposing secrets (#488).
- Partly deleted build caches no longer gain another idle-protection period, and emptied `.pnpm-store` directories are removed (#490).
- Insights per-gate percentiles use the complete sample set; negative clock deltas are clamped consistently, and long idle periods are not counted as active work (#487).
- Deleted source fragments are pruned within the graph directory only; same-named symbols on different lines retain distinct projected IDs. `graph impact` works with read-only broker snapshots (#459).
- Preparation cache rotation leaves the current run's cache available; Docker image tags and unrelated environment variables are no longer reported as fixed ports (#461).

## [0.8.14] - 2026-10-01

Fresher coordination notes, quieter verify-only status, faster per-session reclaim.

### Changed

- The edit-time hook note now reflects each other live session's current change to the file being edited, read from its worktree, instead of the lease data recorded by the last broker command; a reverted change no longer warns. Results are cached per file stamp (#485).
- In `verify-only` repositories, `broker status` and `status doctor` no longer show advice about the integration branch moving: `integration.may-move`, the integration drift rows, `main.external-writes` and the `wait-stable` hints. `integration.unpublished-work` still reports commits left on integration from before the switch. `auto` and `manual` repositories are unchanged (#483).
- `gc reclaim plan --session` and `apply --session` scan only that session's worktree, so a session plan costs time in proportion to that worktree rather than to the whole worktree root, and `apply` no longer re-measures directory sizes. A confirmation digest now binds its scope: a session plan's digest is refused for a whole-root apply and the reverse. A session whose worktree is gone gets an empty plan instead of an error (#484).

### Fixed

- Tests no longer depend on the host's free disk space: every headroom decision reads one probe, which tests can simulate only for repositories under the system temp directory. Production headroom checks are unchanged (#482).

## [0.8.13] - 2026-10-01

Agents start on current main, catch up with one command, and coordinate instead of blocking each other.

### Added

- `broker start` fetches the default branch before choosing its base, so a new worktree starts from the remote tip; when the remote is unreachable it uses the cached copy and reports its age. `--reuse` and `--adopt` report how far the worktree has drifted (#473).
- `aethyme broker sync --session <id>` brings a session up to the latest default branch when it is safe: clean tree, no simulated conflicts. It rebases an unpublished branch, merges into a published one, and on a conflict changes nothing and lists the paths (#473).
- `broker start`, `--reuse` and `--adopt` show a short working agreement, configurable per repository with `[session] guidance` in `.aethyme/config.toml` (`[]` turns it off). The default asks for small targeted commits with explicit staging, one concern per PR, reuse of existing shared components, tests for behaviour changes, and nothing unintended committed or pushed (#474).
- Through the agent hook, an agent about to edit a file another live session is changing gets a short note naming that session, its task, the lines it changed, whether Git would conflict, and the command to message it. Broker notes are delivered into the recipient's context at its next turn boundary (#477).
- `start`, `adopt` and `push` warn when another live session works on the same branch or pull request; `status` adds `session.duplicate-work` and `session.mid-merge` rows (#475).

### Changed

- In `verify-only` repositories, lease overlaps never refuse `broker submit`; conflicting overlaps are reported as warnings with a coordination hint. `auto` and `manual` repositories still refuse a real conflict with an actively working session (#475).
- `[leases]` rules are read from the configuration committed on the default branch, like `[promote]` and `[delivery]`, and an `ignore` entry containing `/` is an exact repository-relative path (#475).
- CI runs the full workspace suite with cargo-nextest on pull requests and on main, with caches warmed from main (#466). The repository's `cargo-test` gate runs the whole workspace suite again, not only the example tests (#469).

### Fixed

- Worktree lock failures on a configured worktree root are reported instead of discarded (#472).
- The build is restored after two changes added conflicting `GitRepo::operation_in_progress` methods (#476).

## [0.8.12] - 2026-10-01

Verify-only repositories stop using the integration branch, and worktrees can live on another drive.

### Added

- In `[promote] mode = "verify-only"` repositories, `broker submit` verifies against the fetched default branch instead of the integration branch, and `broker push` fetches the default branch and reports how far the session is behind it and whether merging it would conflict, with the command to catch up. `broker status` adds a per-session `session.behind-main` row and drops integration advice that no longer applies there. `auto` and `manual` repositories keep the integration branch (#462).
- `[worktrees] root` in `.aethyme/config.toml` places new session worktrees under another directory, such as an external drive, read from the committed default branch like `[promote]`. The configured path is used only when it already exists, is writable, is outside the repository and has at least `worktrees.min_free_bytes` free (default: the 8 GiB a gate needs); otherwise worktrees go to the usual location and `broker start` reports why. Worktrees on an unplugged drive are reported unavailable and are never retired or removed (#465).
- `broker status` reports a `host.gate-headroom` row when the volume where gates run is below the gate headroom, even when nothing is retained, and points to `gc plan` (#457).

### Changed

- `broker submit` defers instead of rejecting when a gate could not judge the change because of a host fault the broker observed itself (refused resources, a command that never started, its own timeout kill, a full disk). A deferred submit has its own status advice with recovery commands and exits with the environment exit code; a second timeout on the same tree is a verdict (#452).
- Merge verification uses a pool of stable verification checkouts so independent submissions verify concurrently, and integration only moves by compare-and-swap from the verified base (#456).
- `broker status` no longer starts one process per session to check liveness on macOS, and reads all session branch tips in one Git call: about half the wall time with nine live sessions (#455).

## [0.8.11] - 2026-09-30

Push-early delivery, safer worktree cleanup, and quieter multi-agent coordination.

v0.8.10 was tagged but never published: its release stopped at the gate check before building. Everything listed here ships for the first time in v0.8.11.

### Added

- `aethyme broker push --session <id> [--pr]` publishes a session's own `agent/*` branch while it works, and with `--pr` opens a draft pull request. It pushes nothing else, and rewrites a branch only under a lease on the commit it last pushed. A repository enables it with `[delivery] push_session_branches = true`, read from the committed configuration on the default branch (#433).
- `broker status` and `doctor` report unpushed session commits and their age, and warn when the local integration branch runs ahead of upstream. With the push lane enabled, `broker finish` refuses to close a session that still holds unpushed commits unless it is given `--abandon --reason` (#434).
- Sessions are warned when their changes overlap an open pull request, in `push`, `status` and `start` (#441), and are told which session should land a shared edit first when two live sessions change the same lines (#442).
- `broker finish cleanup resolve --archive` archives a closed dirty or pending worktree, verifies the archive, then removes the worktree (#429). Archives expire through a reviewed `gc plan` after `retention.recovery_archive_days` (default 30) (#439).
- Sessions take an optional `--short-name`, derived from the task when omitted, and matching Chau7 tabs are titled `<id> - <short name>` (#431).
- `broker gc reclaim plan|apply --session <id>` scopes a reclaim to one session (#427).
- `retention.idle_session_artifact_hours` (default 24) lets the automatic sweep reclaim ignored build output from open sessions whose agent has gone quiet (#428).
- Generated agent instructions describe the push lane when a repository enables it (#432).

### Changed

- Lease overlaps are reported once per session pair, ranked by whether Git would actually conflict, and `broker submit` blocks only on a conflicting overlap with an actively working session (#440).
- `broker gc reclaim apply` deletes the reviewed paths that are still reclaimable instead of refusing the whole plan when anything else changed (#427).
- Garbage collection and reclaim share one artifact catalog, which now includes `.venv` (with `pyvenv.cfg`) and `.pnpm-store` (#425).
- The representation scan, cleanup audit and cleanup plan share one landed-work check that recognizes rebased and squash-merged work (#426, fixes #408).
- `gc storage` counts fully cleaned worktrees as retired rather than missing, and proposes empty worktree roots for reviewed removal (#424).
- `broker push`, `finish` and `submit` count an untracked folder as one uncommitted entry (#443).
- Promotion commits pass the repository's own commit-message lint, and every cargo gate and CI job runs with `--locked` (#435).
- Dependencies: `thiserror` 2.0.21 (#444).

### Fixed

- `broker start` no longer cuts sessions from an integration branch that has fallen behind the fetched default branch, and never from integration under `[promote] mode = "verify-only"`; it warns and reports the reconcile command instead. `[promote]` is read from the configuration committed on the default branch, like the push setting (#450).
- `AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS` can no longer record gate trust for a real checkout (#435).
- Concurrent gate workers always receive distinct broker database directories (#438, fixes #430).
- Release-manifest tests that were compiled but never run now run (#435).
- The help snapshot suite covers every public command and fails on orphaned snapshots (#437).

## [0.8.9] - 2026-09-29

Broker storage safety and clearer gate results.

### Added

- Storage inventory recognizes a Git worktree placed directly in the broker worktree container, reports its branch, and excludes its contents from reconciliation and removal candidates even when it contains an ownership marker (#335).

### Changed

- The workspace cargo-test gate now completes all tests and reports all failures in one run.
- Garbage collection does not offer tracked or unrecorded build output for reclamation.
- Graph status describes a missing graph store as the repository posture.


## [0.8.8] - 2026-09-28

**Breaking:** see [UPGRADING.md](UPGRADING.md#v088).

The broker and top-level CLI now require the documented primary command
spellings.

### Changed

- Removed deprecated broker and top-level CLI aliases scheduled for removal
  in #387. Retired spellings now exit with status 2 and show their
  replacement. Existing v0.8.7 readiness recovery journals remain recoverable,
  and Playground setup uses `deploy --generated-only` to preserve
  discoverability-only behavior.

## [0.8.7] - 2026-09-28

Broker recovery, worktree lifecycle, and gate reliability improvements.

The broker database schema advances from 42 to 43. The migration is additive
and compatible with v0.8.2 and later. v0.8.0 and v0.8.1 refuse schema 43;
upgrade those binaries before using this repository.

### Added

- Gate results now record machine load, CPU count, and free disk at admission,
  so duration reports can distinguish slow gates from host contention (#376).
- Cleanup inventory compares Git worktrees, broker ownership, and filesystem
  state, and uses content-aware delivery proof before reclaiming eligible
  closed-session checkouts or regenerable build output (#375, #388, #397;
  broader lifecycle work remains in #335).
- Console inventory explains each console's port, process identity, and
  recorded checkout existence. Stop/reap requires proven process identity and
  checkout ownership (#374 §3, #389; related context: #169 and #335).

### Changed

- A clean local main that is behind integration now gets an exact reviewed
  fast-forward plan; summary status uses a baseline consistent with detailed
  integration status (#374 §§1–2, #395, #396).
- Expensive gate admission accounts for host load instead of timing out while
  the machine is saturated (#399).
- Repository callers use the six-verb broker surface. Deprecated aliases
  continue to work with warnings; removal is scheduled for v0.8.8 (#387).

### Fixed

- Gate database resolution now fails closed around shared state, and workspace
  members are derived from the tree under test so a passing result cannot
  silently certify a different checkout (#392, #398).

These changes resolve the three recovery cases in #374. They do not complete
the broader worktree lifecycle tracked in #335.

## [0.8.6] - 2026-09-26

Build and disk maintenance, plus reliability fixes. No schema bump (42), so
rollback to 0.8.5 is unrestricted. Old broker spellings still work and warn;
their removal moves from v0.8.6 to v0.8.8, so that the remaining callers
(generated guidance templates, adapters, one CI step, docs) can migrate
first.

### Added

- `broker gc` now sees and reclaims this repository's gate cache under
  `~/Library/Caches/Aethyme/gates/<repo>`, which could hold 4–11 GiB while
  gates refused to start below 8 GiB free. Reclaim is scoped to this
  repository only. Held entries (a lease, a live pidfile, an owner lock, or a
  missing lease registry) are never proposed. The active cache is kept unless
  `--include-active-gate-cache` is given. Older entries are held to
  `gate_cache_bytes_budget` (default 4 GiB), and apply re-verifies each entry
  against the plan. `gc plan --json` gains `gate_cache`, `gate_caches` and
  `estimated_build_output_reclaimable_bytes` (#371, closes #295).

### Changed

- Dev and test builds keep file:line debug info for workspace crates and
  drop it for dependencies. `docs/guides/build-performance.md` documents an
  optional machine-level sccache setup (#365).
- The repository's cargo-test gate may run for 2400 s, up from 1200 s. This
  policy change requires `aethyme broker advanced trust` once per machine
  (#370).
- The disk-headroom refusal names the gate cache: what is kept as active and
  what is reclaimable (#371).

### Fixed

- Upgrade proposals, graph refresh proposals and enrollment clone local
  repositories with `--no-local`. The previous file-level copy could fail
  when the source repository repacked during the clone (#368).
- `broker finish` counts commits already on the default branch as delivered
  when integration lags (#364).
- Four broker tests that failed under machine load are now deterministic
  (#367).

## [0.8.5] - 2026-09-26

Phase 5 of the recovery plan: more platforms, signed installs by default, a
Homebrew tap that updates with each release, and the first broker benchmark.
No schema bump (42); rollback to 0.8.4 is unrestricted.

### Added

- Release archives for `aarch64-unknown-linux-gnu` (built on a native arm64
  runner) and `x86_64-unknown-linux-musl` (static, for Alpine and other
  non-glibc systems). `install.sh` detects both. `aethyme update` picks the
  archive that matches the running binary. The Homebrew formula gains a Linux
  arm64 block (#350).
- The Homebrew tap updates automatically: a final release job commits the
  release's own formula to `schiste/homebrew-tap` for every stable tag (#350).
- `install.sh --require-signature` and `--no-verify-signature` (#350).
- `aethyme broker advanced worktrees --json` reports Git worktree registration
  state (detached, bare, locked, prunable), read from `git worktree list
  --porcelain -z`. Paths that aren't valid UTF-8 are preserved (#347).
- A benchmark harness under `packages/aethyme-eval/benchmarks/broker/`, with
  the results of its first run in `docs/reports/broker-benchmark-2026-09.md`.
  The run used four agents across git worktrees + PRs, native agent worktrees
  and the broker. Only the broker kept main green throughout (#353, #355).

### Changed

- `install.sh` verifies the cosign signature on the release manifest whenever
  `cosign` is installed, using the same identity and issuer the release signs
  with. Without cosign it prints a note and continues (#350).

### Documentation

- A scoping document for a Windows port. Windows is supported through WSL2 only
  for now, and the README says so (#351).
- An Explore re-baseline report, including where results got worse, and a
  pilot kit for early teams (#349).

## [0.8.4] - 2026-09-25

Phase 4 of the recovery plan: a smaller command surface and shorter agent
guidance, plus two Explore improvements. No schema bump (42); rollback to 0.8.3
is unrestricted. Old broker spellings keep working until v0.8.6 and print a
warning on stderr.

### Added

- `aethyme explore --request "<q>" --format brief` answers with one call: a
  summary and the top 2 verified source spans. `--repo` defaults to the
  current directory, and answer-json remains the default format (#345).
- `aethyme broker advanced <verb>` holds every non-core broker verb: `leases`,
  `git`, `gh`, `ship`, `review`, `operations`, `exec` and the rest (#344).
- A single `UPGRADING.md` at the repository root, with a section for each
  breaking release. Release notes are now rendered from this changelog plus
  that section (#342).

### Changed

- `aethyme broker --help` lists six verbs: `start`, `status`, `submit`,
  `finish`, `unblock` and `gc`. Old spellings still work until v0.8.6, with
  one deprecation warning on stderr (never on stdout). For example,
  `adopt --reuse` becomes `start --reuse`, `close` becomes `finish close`,
  `promote` becomes `submit promote` and `blockers` becomes `unblock`. Other
  renames: `enhance deploy` becomes `deploy`, `broker init` becomes
  `aethyme init`, and `aethyme readiness` becomes `broker status readiness`.
  `--json` output shapes are unchanged (#344).
- `--help` and `-h` work on every command, and they no longer write anything.
  Before, `graph materialize --help` rebuilt the graph store (#344).
- The generated CLAUDE.md and AGENTS.md policy block is at most 1,000 tokens.
  This repository's block went from about 1,900 to about 580. The detail moved
  to the skill's `references/policy.md` (#345, #346).
- The SessionStart hook prints the session's state and the single next
  command, instead of policy text (#345).
- Explore's default graph-free answer-json is about 31% smaller: per-hint
  diagnostics and scoring moved to `--show-observability` and
  `--detail standard` or `--detail full`. Ranking is unchanged, and agents
  using it cost less per question (#339).
- Graph-free ranking averages BM25F field lengths over every searched file, so
  large hub files are properly length-penalised (#340).
- `ai-ready` and `quality inspect` JSON emits non-ASCII text as UTF-8 instead
  of `\u` escapes. Autofix diffs are now standard unified diffs. `ai-ready`
  and `autofix` are hidden from top-level help but still work (#343, #344).

### Removed

- The Python-parity layer in aethyme-quality: `difflib` and `pystr`
  emulation, and the byte-parity tests. That is 801 lines net (#343).

## [0.8.3] - 2026-09-24

Phase 3 of the recovery plan: Explore works without a graph. No schema bump
(42); rollback to 0.8.2 is unrestricted.

### Added

- Graph-free Explore. With no graph store (the default in every repository),
  Explore searches file contents within a 2 s budget and returns ranked hits
  with line spans, backed by an on-demand symbol index cached under the host
  cache (`symbol-index/v1/`). Before, it matched path names in the first 128
  files (#331).
- `observability.source_fallback` reports the search: completeness, budget,
  scoring details, and the verdict of the content-evidence answer rule in
  `answer_safety` (#331, #332).

### Changed

- Graph-free ranking uses BM25F over file-name, directory, definition-name,
  code and comment fields, with question stopwords and light stemming. A hit's
  reported spans are the definitions that cover the most of the request (#337).
- Graph-free results are navigation only: `safe_to_use_as_answer` is always
  false and `answer[]` is empty. The answer rule is still evaluated and
  reported, but it does not promote hits until its measured precision reaches
  95%; it was 4 of 6 on the held-out set (#332, #334).

### Fixed

- The TypeScript and JavaScript indexer now indexes `export`-wrapped
  declarations, which is most symbols in typical TypeScript (#330).

## [0.8.2] - 2026-09-24

**Breaking:** see [UPGRADING.md](UPGRADING.md#v082).

Phase 2 of the recovery plan: broker hardening. No schema bump (42); rollback
to 0.8.1 is unrestricted.

### Added

- `aethyme broker trust [--repo] [--json]` and `trust status`. A repository's
  gate and prepare commands run only once their policy is trusted on this
  machine. A new repository, or a changed gate or prepare policy, needs
  `broker trust`, which works only from an interactive terminal. Repositories
  with gate history on this machine are trusted automatically. An untrusted
  policy is refused with exit 3 and the command to run.
- `aethyme broker blockers [--json]` lists everything blocking a repository in
  one id namespace, and `aethyme broker unblock <id>` clears one through the
  matching recovery path. It covers unknown-outcome operations (including
  host operations by their hex id, #276), cached failing gate verdicts
  (previously a hand-written SQL delete, #281), stale pidfiles, stale leases
  and quarantined resources. `broker status --json` gains a `blockers` array.
- `[review.trigger] include_forks`. Fork pull requests are skipped by default.

### Changed

- Flags a broker subcommand does not read are refused with exit 2 and a message
  saying where the flag applies, instead of being silently ignored (#285
  class). `broker cleanup <id> --dry-run` used to ignore `--dry-run` and clean
  the session; it is now refused.
- The broker database records `min_compatible_schema`. From this release on, an
  older binary opens a newer database without migrating when every newer
  migration was declared compatible, instead of refusing (#293).
- Reviewers in the review lane run without push-capable credentials. The broker
  fetches the diff and posts the review.
- Git subprocesses run by the broker time out after 10 minutes
  (`AETHYME_GIT_TIMEOUT_SECS`) and exit 6.
- Failed state writes that were silently discarded now propagate or print a
  `warning: aethyme could not ...` line.
- The broker CLI is split into modules under `crates/aethyme-broker/src/cli/`.
  There is no user-visible change.

### Fixed

- `enhance deploy` refuses to write or chmod through symlinks.
- The operation journal redacts credential-bearing `-c` values and
  `Authorization` headers.
- A gate's process group is signalled only after checking the pid, pgid and
  start time, so a reused PID is never signalled. Pidfiles are written
  atomically.

## [0.8.1] - 2026-09-23

**Breaking:** see [UPGRADING.md](UPGRADING.md#v081).

Fixes two regressions from v0.8.0 seen in real use. No schema change (42);
rollback to 0.8.0 is unrestricted.

### Fixed

- `broker git -- git ...` and `broker gh -- gh ...` are refused plainly, naming
  the fix (drop the repeated program name). In v0.8.0 the unrecognized-command
  refusal told the caller to declare `--effect write`; the failed write then
  left an unknown outcome that write-blocked the whole repository. The
  unrecognized-command refusal now says to check the command name first.
- A submission whose failing gates could not run on the host (low disk, locks,
  a missing tool: `resource_contention` or `environment`) exits 6 instead of 4,
  prints that the code was not judged, and status/finish advice says to free
  the resource and resubmit without changing code.

## [0.8.0] - 2026-09-23

**Breaking:** see [UPGRADING.md](UPGRADING.md#v080).

A correctness and integrity release from the 2026-09-23 audit. It changes
exit codes and what a submission is judged by, hence the minor bump. The broker
database schema is unchanged (42), so rollback is unrestricted.

### Changed

- `aethyme broker` exit codes now name the outcome: 3 refused, 4 verification
  failed, 5 outcome unknown, 6 environment; 1 stays an unclassified failure.
  `broker submit --json` no longer exits 0 for rejected (4) or conflicted (3)
  entries. See "Exit codes" in `docs/reference/cli.md`.
- `broker submit` reads `.aethyme/gates.toml` and the `[graph]` policy from the
  integration base the change lands on, not from the submitted tree. A session
  can no longer weaken or delete the gates that judge it; a policy change it
  makes applies after it lands and emits `merge.policy_deferred`.
- An unrecognized git/gh command can no longer be declared `--effect read`;
  declare `write` or `destructive`. Inline `-c`/`--config-env` keys that run
  programs or define aliases, and `--exec-path`, are refused for coordinated
  git.
- `+refspec` pushes, bundled short flags (`-fdx`, `-uf`, `-Df`), `git push -d`,
  `update-ref -d`, forced `send-pack`, `reset --hard/--merge/--keep`, `clean
  -f/-d/-x` and `gh api -XDELETE` now classify as destructive.
- The pre-push hook verifies coordination in the operation journal. Exporting
  `AETHYME_BROKER_OPERATION_ID` and `AETHYME_BROKER_SESSION_ID` no longer
  unlocks a push to a protected branch.
- Explore's auth ranking layer is removed. It was tuned to one evaluation
  playground and broke the project's eval rule. Explore ranks auth questions
  generically until a content search replaces it. Eval results from
  2026-07-28 to 2026-09-23 are marked contaminated.

### Fixed

- `broker adopt` records declared and task-derived scope (#285), and
  `broker start --json` records scope too. `--claim` is refused on
  subcommands that cannot honor it instead of being silently discarded.
- The promote commit carries `Contract justification:` alongside the contract
  decision, so a justified `none` can pass the contract gate.

### Repository

- This repository no longer commits `.aethyme/graph` fragments.

## [0.7.25] - 2026-09-22

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0725).

### Added

- Sessions can declare the targets they will work on, and collisions between
  live sessions are reported before any edit exists. `broker start` and
  `broker adopt` take a repeatable `--claim kind:value[=operation]`, targets are
  also derived from the session task where the repository has graph state, and
  `broker status --json` carries `scope_overlaps` beside the existing path
  `overlaps`. Severity is decided by the pair of intents rather than by the
  shared target, so two sessions extending one interface stays quiet while a
  rewrite underneath an extension does not. A target named without an operation
  records as `unknown` and reports at medium rather than being assumed safe.
  A session's declared targets stop pairing once it finishes, by the same
  liveness rule leases apply.
- A `none` cross-process contract decision is accepted when the pull request
  body carries a `Contract justification:` line of at least 24 characters. The
  finding is still printed, with the justification beside it.

### Fixed

- Tracked cross-process symbols match on word boundaries instead of as
  substrings, so a tracked name inside a longer identifier is no longer read as
  a removal.
- `broker git` refuses a `HEAD`-family push source when the caller is outside
  the session worktree. Coordinated commands run in the session worktree, so
  such a refspec resolved there rather than where the operator stood and
  published an unintended commit while reporting success. Every successful push
  now also states what it sent.
- A submission that does not promote reports why, naming the gate, its failure
  class and its log. A gate that refused before spawning previously reached the
  caller as a bare "did not promote", indistinguishable from a gate that ran and
  failed the change.
- `broker integration reconcile` skips terminal queue entries whose commit is no
  longer reachable. A never-merged entry loses its commit to `git gc`
  eventually, which is its expected end state; reading its parents aborted the
  whole pass and deferred post-merge cleanup after every merge. A pending entry
  with a missing commit still fails loudly.
- Graph verification no longer refuses on the recorded engine version. The
  comparison was against whichever binary happened to be installed, and a
  version string cannot distinguish two builds; the tree comparison still
  detects a genuinely different verifying binary.
- `aethyme --version` reports the commit it was built from. The build scripts
  watched `.git/HEAD`, which does not change when a branch advances, so a pull,
  fast-forward or local commit produced a binary carrying stale provenance.

### Changed

- Broker database schema 41 to 42, adding `session_scopes`. Migrations are
  append-only and there is no downgrade: an older binary refuses a migrated
  database rather than guessing, so the CLI and engine pair must be installed
  together.

## [0.7.24] - 2026-09-21

### Changed

- The autonomous artifact sweep now reclaims the shared preparation cache. It
  previously ran on its daily cadence and reclaimed nothing, because the cache
  was reachable only from `aethyme broker storage`, a command an operator had
  to type. An entry is removed only when no checkout on the host would compute
  its key, which is the same rule the manual lane applies.
- The sweep's effort now follows free space. Below the headroom a gate
  requires to start, its budget widens and its interval shortens to an hour;
  above it, the previous light touch is unchanged. Unknown free space is
  treated as routine rather than urgent.
- `aethyme broker status` reports host free space beside retained bytes, and
  says that the retained-bytes budget is per repository while the volume is
  shared.

### Added

- Graph enrollment, refresh, and the committed Aethyme graph artifacts.

## [0.7.23] - 2026-09-19

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0723).

### Fixed

- Bind preparation-cache deletion to reviewed plans and preserve caches when
  liveness is uncertain.
- Isolate gate databases from shared operator state, and isolate deployment
  test fixtures from the enclosing gate's database override.
- Verify installer archive digests against the release manifest and reject
  ambiguous artifacts. Manifest parsing now requires jq.
- Validate readiness review configuration through the runtime policy loaders.
- Keep operation liveness honest about unknown state and retain heartbeats
  through reconciliation.

### Added

- Bounded source-navigation hints when the optional graph is unavailable.
- Persistent coordinated-operation heartbeats and progress diagnostics.
- Shared preparation-cache inventory and reclaim.
- Privacy-safe pilot cost snapshots, comparisons, and operational recovery
  and external-pilot guides.

### Changed

- Run workspace tests on PRs, combined gates on main, and the duplicate
  binary-only workflow manually. Add narrow script and workflow contracts.
- Retain broker schema 41; no database migration from v0.7.22.

## [0.7.22] - 2026-09-18

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0722).

### Changed

- The broker database moves from schema 39 to 41. Migration 40 rebuilds
  `gate_results` so it can record a build failure separately from a test
  failure; migration 41 adds nullable `repository_name`, `tab_name` and
  `ai_provider` columns to `sessions`. Both run when the first v0.7.22 binary
  opens a database, and older binaries then refuse that database. See the
  upgrade guide before installing alongside live sessions.

### Added

- Broker sessions record the repository, tab and AI provider they belong to, so
  a session can be identified by where it is running rather than only by its
  worktree path.
- `aethyme explore` and the semantic gate report can answer from an Imports
  graph-impact mode, traversing bounded incoming Imports edges where Calls has
  no answer.
- `aethyme broker storage` inventories enrolled primary checkouts, reporting
  recognised regenerable artifacts, Git cleanliness, and tracked-file
  protection separately from host storage roots.
- Gate failures are classified as `build_failure` when the command never
  compiled, distinguishing a broken build from a failing assertion. Detection
  is cargo-scoped and sits behind the resource and environment checks, so a
  build stopped by a full disk is still recorded as contention.

### Fixed

- A delivery whose target never returns is dead-lettered after a bounded number
  of attempts instead of being deferred forever. Deferral still absorbs a
  target that is briefly away.
- A failing gate log is kept under a unique name instead of being overwritten
  by a later run on the same tree, so a failure followed by a pass on unchanged
  source leaves both records. This now covers the host-resource and
  managed-cache failure paths as well.
- `aethyme broker storage apply` requires a primary checkout's build directory
  to have stopped changing before removing it. A running build leaves the
  checkout clean, because build output is Git-ignored, so cleanliness alone
  could license deleting a build out from under itself.
- `aethyme broker storage` reports every enrolled checkout on the host but only
  removes artifacts from the checkout it was invoked in; a confirmation digest
  speaks for one repository.
- Promotion no longer overwrites a session representation that already records
  a pull request merge, which had downgraded a session that landed on the
  default branch to unproven provenance and blocked its cleanup.
- Prepared admissions record structured wait evidence and are reaped
  independently when the holding client process is gone.
- Dead quarantined resource holders are diagnosed and their capacity reclaimed.

## [0.7.21] - 2026-09-16

### Fixed

- Cleanup can release a session whose branch was squash-merged. The recorded
  representation search now prefers the remote-tracking default branch, which
  the ship lane never moves, and reachability from a remote ref is accepted as
  durability under a freshness check that treats not knowing as unproven.
- A worktree detached from its session branch is released when both the
  worktree head and the branch tip are proven durable, instead of being refused
  without asking whether either is lost.
- The autonomous artifact sweep records how far it got, so an interrupted pass
  advances the next one instead of re-scanning the head of the session list.
- Storage exhaustion during a gate is classified as resource contention rather
  than a test failure, so a full disk no longer records a conclusive verdict
  that the tree-hash cache replays on every resubmission.
- `--queue-timeout` and `--no-wait` bound the whole Git admission, including the
  preparation that contacts the network, and exhaustion reports a distinct
  admission timeout instead of naming a lock holder that never existed.
- `broker status` counts the integration lead against the published default
  branch rather than the checked-out branch, and names the reference it used.

## [0.7.20] - 2026-09-16

### Fixed

- Release validation now parses tagged version banners that include build
  metadata, keeping the release contract aligned with the production binary
  output.

This is a maintenance release with no runtime or broker-schema changes from
v0.7.19.

## [0.7.19] - 2026-09-16

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0719).

### Added

- Sessions now reach a terminal state on their own. A session with no evidence
  of a working agent for `session_abandoned_after_hours` (default 72, `0` to
  disable) is closed by the broker, which releases its leases and makes a
  broker-owned worktree a cleanup candidate.

  `stale` was only ever a label. Nothing moved a stale session anywhere, so it
  pinned its worktree, its branch and its leases indefinitely — on the dogfood
  machine, 16 of 19 sessions sat stale, one worktree of 38 was eligible for
  cleanup, and essentially all retained bytes reported as blocked (#176). The
  warning was accurate and no policy could act on it.

  A session whose process is alive is never abandoned, however long it has been
  quiet: a long-thinking agent is indistinguishable from a departed one on every
  signal except the process itself. Abandonment grants *candidacy*, never
  eligibility — dirty trees, unpromoted commits and unproven provenance block
  removal exactly as before, so this reclaims disk that nothing was using and
  cannot reclaim work. Each transition appends `session.abandoned` carrying the
  observed idle time and the threshold that was applied.

- `broker status` and `broker gc plan` now report the directories under a
  worktree root that no session claims. Status counts them; `gc plan` also
  sizes them, and both carry them as `reconciliation` alongside the existing
  totals.

  Every cleanup lane starts from a session row, so a directory with no row was
  invisible to all of them — not retained, not reclaimable, not blocked, simply
  missing from the arithmetic. On the dogfood machine 54 directories sat under a
  root the broker described as holding 38 (#176), and no policy could reach the
  difference because nothing reported that it existed.

  Report only. `gc apply` never removes an unclaimed directory, no retention
  window reaches one, and the sweep is excluded from the plan's authorization
  digest so a directory appearing on disk cannot revoke a confirmation an
  operator already gave. The broker did not create these directories and cannot
  reason about their contents; naming them for a human is the whole remedy.

- `retained_bytes_budget` now drives reclamation instead of only reporting on
  it. Over budget, `gc plan` orders its candidates largest first; within budget
  it orders them oldest first. The applied rule ships as `reclaim_order`.

  `gc apply --budget-ms` drains the plan in order and stops at its deadline, so
  the order is what a bounded sweep actually reclaims. Candidates used to be
  listed in session-id order — insertion order wearing a policy's clothes — and
  a sweep that got through three of forty spent its deadline on whichever three
  happened to be oldest by row id (#176). Build caches follow the same rule,
  ranked on their recorded idle days. Orphaned worktree roots are always ordered
  largest first: their owning repository is gone, so no age is recoverable and
  size is the only fact left.

  The plan and `broker status` also now report `retained_bytes_deficit` — the
  bytes above the budget — and `clears_retained_bytes_budget`, which says
  whether applying everything would get back under it. A budget that reclamation
  cannot satisfy, 3.2 MB reclaimable against 37.4 GB retained, used to read
  exactly like a backlog somebody could work off; the status warning now says
  which it is rather than recommending cleanup that provably would not help.

  Both reporting fields stay out of the plan's authorization digest, like the
  other byte totals. The ordering does change the digest, because it changes the
  candidate lists: a plan that crosses the budget threshold has to be
  re-confirmed.

- `aethyme broker review waive` excuses one review dimension at one head, on the
  record:

  ```bash
  aethyme broker review waive --repo owner/repo --pr 412 \
      --type code --head 9f3c1a2b4d5e --reason "reverting the outage; reviewed offline"
  ```

  Until now the only way to unblock a dimension that would not clear was
  `review state --state satisfied`, which writes "a review happened" into the
  one record anyone ever consults. Nobody could later tell a security review
  that passed from a security review somebody waived at 2am, and the whole value
  of the ledger is that distinction.

  A waiver is bound to repository, pull request, **dimension** and **head
  commit** together. Waiving `code` leaves `security` exactly as blocking as it
  was; a new head has no waiver, so it expires by construction rather than by
  anyone remembering to withdraw it. `--reason` is required and is stored as
  text rather than as the SHA-256 digest the broker's coordinated operations
  use, because it is addressed to whoever later asks why this dimension is
  green; the author comes from `--agent` or `AETHYME_AGENT`, and an
  unidentified operator is recorded as one rather than refused.

  The pull request comment renders `⊘ <type> — waived` with the author and
  reason, never a tick. Two refusals are deliberate: `review state --state
  waived` does not work, because that command takes no author and no reason, and
  a review already `satisfied` cannot be waived, because that would overwrite
  the evidence that a review actually ran.

- Review freshness is declared per dimension. A `[review.trigger.schedule.<type>]`
  table may set `freshness = "head"` (the default, and what every dimension has
  always done) or `"head_and_base"`, which additionally invalidates a completed
  review when the base commit moves:

  ```toml
  [review.trigger.schedule.security]
  freshness = "head"            # valid until the head SHA changes

  [review.trigger.schedule.code]
  freshness = "head_and_base"   # a moving base changes what the diff means
  ```

  One staleness rule applied to every dimension is what forces an operator to
  reach for a blanket override, which waives every dimension rather than the one
  that is actually stale -- so clearing a stale code review silently clears
  security too. `Aeptus/mockup`'s PR #619 held a completed security review bound
  to its own current head and was still permanently unmergeable, because the only
  way to clear a timestamp-derived staleness flag was a re-run the provider's
  exhausted quota could not serve.

  Two properties are deliberate. The base is compared as a **commit**, never a
  timestamp: a clock-derived floor is what produced that deadlock. And a base
  that cannot be compared -- no base recorded on the row, or no base resolvable
  now -- counts as *moved*, because the question is whether the base is proven
  unchanged and silence does not prove it.

### Changed

- `broker status`, `doctor`, `certify` and the verify loop no longer walk the
  disk. Sizing a directory has no shortcut — the filesystem stores no subtree
  total — so every byte figure costs a full recursive walk, and status reached
  that walk through the same code as `gc plan`. On the dogfood machine that
  shared path covered about 96 GB and took over five minutes (#176), on the
  command every session is told to run first.

  The expensive walk now belongs to `gc plan`, `gc apply` and `cleanup --apply`
  alone, and records each measured size to `.aethyme/worktree-sizes.json`. The
  routine commands read those records. Counts, dispositions, git state and
  provenance are exact as before; only byte totals come from records, because
  only they were ever expensive. Records warm themselves: each routine check
  spends up to `routine_size_budget_ms` (default 200, max 250) measuring one
  directory that has never been sized or whose record is older than
  `size_record_ttl_hours` (default 24), so a machine nobody audits converges on
  knowing its own size. A directory too large for that budget stays unmeasured
  rather than being recorded from a partial walk.

  Plans now report `unmeasured_directory_count`, `sizes_measured_at_ms` and
  `budget_verdict`. When something was never sized the byte totals are a floor,
  and the verdict is one-sided on purpose: `over` is trustworthy because
  unmeasured bytes could only add to the total, while a floor *under* the budget
  reports `unknown` rather than passing. A floor read as a total is how a budget
  comes to read as satisfied because nobody looked. `over_retained_bytes_budget`
  is raised only by `over`, so the cheap path raises no false alarms either.

  A routine plan carries no authorization digest: a plan that does not know how
  big things are must not be able to authorize removing them. `gc apply` still
  requires a digest from the full audit.
- Broker database schema v37 adds `waived` to the `review_requests.state` CHECK
  constraint, which SQLite can only do by rebuilding the table. A reader that
  matches the six previous states exhaustively must handle a seventh; treating
  an unknown state as *settled* is correct, and treating it as *reviewed* is the
  defect this state exists to prevent. **Every `aethyme` binary on a machine
  must be reinstalled to open a v37 database**; an older binary is locked out
  until it is.
- Broker database schema v36 adds a nullable `base_commit` to `review_requests`
  and to `pull_request_observations`. The column is deliberately left null on
  existing rows: backfilling it from today's branch tip would assert a
  comparison nobody made, and `head_and_base` would then silently pass stale
  reviews. **Every `aethyme` binary on a machine must be reinstalled to open a
  v36 database**; an older binary is locked out until it is.

- Review routing: a route may declare `on_refusal`, a one-hop escape taken when
  the previous attempt at that dimension came back refused. A spent provider
  quota is the one refusal a retry cannot clear -- it is what left ten
  `Aeptus/mockup` pull requests blocked on a dimension whose only exit was the
  re-run that was unavailable -- so a repository can now say "when the provider
  is spent, this dimension is worth paying a local agent for":

  ```toml
  [review.routing.route.code]
  backend = "provider_comment"
  mention = "codex"

  [review.routing.route.code.on_refusal.quota_exhausted]
  backend = "chau7"
  max_concurrent = 1
  ```

  Declaring nothing changes nothing, so an existing policy routes exactly as
  before. The edge is keyed by the classification #173 records, is never taken
  on a first attempt, and reverts as soon as any later attempt exists -- so one
  quota refusal cannot pin a dimension to the expensive backend for the life of
  the pull request. A fallback has no fallback of its own (a nested
  `on_refusal` is a parse error, not an ignored key), carries its own slot
  budget because an agent does not cost what a comment costs, and may not name
  the backend it is escaping: that is refused when the policy loads rather than
  discovered as a second refusal.

### Fixed

- The exact-tree verification slot -- the disposable checkout that merge
  simulation, graph-integrity verification, and the gate-doctor probe run in --
  now lives outside the repository it verifies, beside the broker worktree root
  in host state (`AETHYME_HOST_STATE_DIR`), or under the system temporary
  directory for a repository that is itself ephemeral.

  A slot is a checkout, and nesting it at `.aethyme/run/<namespace>` put it
  inside the tree under test. Anything discovering a workspace by walking
  upward -- the ordinary idiom -- then resolved to the *enclosing* checkout
  whenever the slot was absent or half-built, and answered confidently with the
  wrong tree instead of failing (#149). Outside the repository that same walk
  terminates with nothing, which is loud. Gate isolation no longer depends on a
  directory inside the repository being present.

  A sandbox that confines writes to the invoking checkout has nowhere else to
  put it. There the slot still falls back to `.aethyme/run/<namespace>`, but the
  placement is recorded and `broker gates doctor` reports it as
  `nested_verification_slot`, naming where the slot was wanted, why each
  location outside the repository was refused, and the discovery hazard the
  fallback carries.

- `broker gh` now answers "was the issue created?" for a `gh issue create` or
  `gh pr create` that exits non-zero, instead of recording `outcome_unknown` and
  leaving an operator to look. Before the command runs the broker records the
  highest number the collection has assigned. A resource URL under the asserted
  repository on the command's own output is conclusive; failing that, the
  collection is listed and an entry above the watermark whose title (issue) or
  head branch (pull request) matches proves creation, while a listing that
  reaches back past the watermark without a match proves none. The verdict and
  its evidence are journaled, shown by `operations show`, and stated in words on
  the command's output.

  Evidence that cannot see far enough still refuses to guess: an unreadable
  watermark or listing, a full page that never reaches the watermark, and a
  create with no resolvable identity — an untitled issue, a pull request with no
  `--head` on a detached HEAD — all remain `outcome_unknown`. A wrong "failed"
  is what would make a blind retry look safe, and a duplicate issue is exactly
  the cost of getting it wrong (#184).

- A labelled `gh issue`/`pr` `create` or `edit` naming a label the repository
  does not define is now refused before anything is journaled, queued, or sent,
  naming the unknown label and the ones that exist. `gh` resolves labels before
  it creates anything, so the whole command failed with `could not add label`
  after the payload was written — costing a full round trip and leaving the
  operator to work out whether the issue existed (#184). The refusal precedes
  the journal, so there is no operation to reconcile and the answer is
  unambiguous: nothing was created.

  Names match without regard to case, as GitHub matches them. A vocabulary that
  cannot be read — offline, unauthenticated, rate-limited — does not refuse the
  write: that machine may be one where the write works, and turning an
  unreadable vocabulary into "unknown label" would refuse valid commands for a
  reason unrelated to labels. The command then behaves exactly as it did before,
  and the create reconciliation above says what happened.

- One repository now has one gate coordination key, whatever checkout asks for
  it. The key is resolved from the main checkout instead of from the calling
  worktree, so a session and the primary checkout agree.

  Two configurations disagreed before. A repository with no `origin` fell back
  to the calling worktree's directory name, and broker worktrees are named
  after their task, so no two sessions ever matched. A repository whose
  `origin` is a *relative* local path (`../upstream`) resolved that path
  against the calling checkout's root, which differs per worktree for the same
  reason. In both, a gate declaring an `ExclusiveKey` or a `Capacity` pool to
  stop two workers touching one piece of external state got a per-worktree
  pool: every worker acquired, every worker ran, and nothing reported that the
  guard had become a no-op (#170). Managed gate caches fragmented the same way,
  each worktree building its own from cold. An absolute or network `origin` was
  always worktree-independent and is unaffected.

- The origin-less fallback now hashes the main checkout's absolute path rather
  than its bare directory name, so two unrelated repositories both called `app`
  stop sharing one key, one managed gate cache and one exclusive pool — as did
  every repository whose directory name was not valid UTF-8, which all
  collided on the literal `repository`.

  Upgrade note: in a repository with no `origin`, or with a relative local
  `origin`, existing gate resource leases and managed gate caches are keyed
  under the old value and are abandoned rather than migrated. Leases expire on
  their configured TTL and each managed cache is rebuilt once. Repositories
  with an absolute or network `origin` — the usual case — keep their keys and
  their caches.

- `broker gates doctor` reports a gate that coordinates under a path-derived
  key. The fallback is correct now but it is still not the cross-clone identity
  a declared pool reads as, and it rotates if the checkout is moved or renamed.
  The notice names only gates that actually declare a resource pool or a
  managed cache, since a gate that declares neither loses nothing to it.

## [0.7.18] - 2026-09-12

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0718).

### Added

- `scripts/adapters/codex-luna-review.sh` performs this repository's routed
  reviews in Codex Luna shells. The broker decides which review and which
  workspace, `chau7-review-adapter.py` performs the Chau7 half, and this script
  is the only file that names a model -- so changing reviewer is an edit in one
  place. It runs the agent unattended (`--approve-for-me`, because a reviewer
  blocked on an approval prompt holds its slot until the staleness window
  reclaims it), confines it to its throwaway checkout with network reach for
  `gh` and the broker, and strips the machine's `git` wrappers from the PATH it
  hands on, so a reviewer typing `git diff` does not review bytes nobody wrote.
- Aethyme's own `.aethyme/config.toml` now enables all three review tables: the
  repository reviews its own pull requests, routing both `code` and `security`
  to a Codex Luna shell. `review run` reads policy from the main checkout, not
  from the branch under review, so a pull request cannot alter the rules that
  judge it.
- Review routing: three opt-in tables in `.aethyme/config.toml` that decide
  which reviews a change needs (`[review.trigger]`), who performs them
  (`[review.routing]` -- a Chau7 agent, a provider review bot, or a plain
  record), and what the pull request shows about it (`[review.projection]` --
  one Aethyme-owned comment plus namespaced labels). Every table is off by
  default at every level, so a repository that has not opted in gets pull
  requests byte-for-byte identical to before.
- Authors classify a change with commit trailers -- `Area:`, `Surface:`,
  `Risk:`, `Review:` -- merged across the commits in the change. A declaration
  can add a review and can never remove one, so an unverified trailer costs at
  most an unnecessary review.
- `aethyme broker review plan [--base <ref>] [--pr <number>]` prints the whole
  decision for a change: eligible dimensions and why, what would be scheduled
  or deferred, which backend would perform each, and the comment and labels
  that would appear. It takes no session, performs nothing, and names both the
  tree it read policy from and the tree it read the change from. Full guide in
  `packages/aethyme/docs/guides/review-routing.md`.
- `aethyme broker review run --session <id> --repo <owner/name> --pr <number>`
  performs what `review plan` describes. It replaces the plan's three
  assumptions with facts -- a new `review_requests` ledger for what has already
  been spent and what is in flight, an optional Chau7 tab snapshot
  (`--tabs-file`) for live workspaces, and read-only `gh pr view` for the pull
  request's current labels and Aethyme comment -- then records each review
  before it is asked for, performs the GitHub writes through the coordinated
  operation lane, and prints the Chau7 spawns for an adapter to start. A unique
  index on `(repository, pr_number, review_type, head_commit)` makes an
  interrupted run re-runnable without a second reviewer landing on the same
  head. `--dry-run` plans against those facts and stops, needing no session.
- `aethyme broker review ledger --repo <owner/name> [--pr <number>]` prints the
  review ledger -- which reviews the router asked for, for which head, through
  which backend, and how each ended. It is read-only, needs no session, and is
  the answer to "why was there no review on this pull request". `aethyme broker
  review state --repo <owner/name> --pr <number> --type <review-type> --state
  <state>` is the other half: whoever performs a review closes its row, which
  is what drains the router's concurrency budget. `--head <sha>` reports on a
  superseded commit; `--note <text>` records why.
- The review ledger distinguishes `recorded` -- the `record` backend's complete
  outcome, where the policy deliberately performs nothing -- from `abandoned`,
  which now means only that nobody was ever asked. `abandoned` is the single
  state the router may ask about again, and a GitHub write that fails now
  returns its review to it, so one failed `gh` call no longer settles a
  dimension permanently for that commit. Existing rows migrate automatically
  (broker schema v34).
- Review concurrency (`max_concurrent`) is counted across the repository rather
  than within one pull request, which is what the setting always documented.
- New `stale_after_minutes` on a `[review.routing.route.*]` entry (default
  `360`, `0` to disable) bounds how long an unfinished review may hold one of
  those slots. A slot is released by whoever reports the outcome and nothing
  guarantees anyone does -- a closed Chau7 tab, a crashed adapter, an
  uninstalled review bot -- so without it a repository dispatches
  `max_concurrent` reviews of a dimension and then silently stops dispatching
  any. Each `review run` tick first marks any review of that dimension whose row
  has not been touched in the window `abandoned` and asks again; the window runs
  from the last update, so a reviewer that reports progress is left alone.
  Expiries appear under `expired` in the run report.
- `aethyme enhance deploy` now installs an `aethyme-review-rule-maker` skill on
  both agent surfaces. It carries the procedure for writing a repository's
  `[review.*]` policy -- find the guarded paths by searching the repository
  rather than guessing them, write the trigger table alone, replay it against
  real history with `aethyme broker review plan --base`, then turn on
  projection and routing in that order -- plus a `references/review-rules.md`
  field grammar loaded only when needed. It is embedded in the binary, so an
  installed Aethyme deploys it with no checkout and no network.
- The review trigger now has a real event source. `aethyme broker review run
  --from-provider` reads the pull request from `gh`, compares it against the
  last observation the broker kept of that pull request, and derives which of
  the ten `on` transitions happened -- reopened, ready for review, a dismissed
  review, a retargeted base, an additional or a replacement commit, or no
  transition at all. Additional and replacement commits are told apart with
  `git merge-base --is-ancestor`, so a force-push that rewrites history is not
  reported as more work on top of it. Without the flag the trigger stays
  `pull_request_opened`, which is what every earlier run assumed. The
  observation is written last, after the run has acted on the transition it
  derived: recording it first would let a half-finished tick claim the
  transition was handled and the next one see nothing to do (broker schema
  v35).
- `on = ["merge_queue_entered"]` is now rejected when the config loads, naming
  the rule: no tick can observe a merge queue from the pull request alone, so
  the rule could only ever wait forever. Turning dead configuration into a loud
  load error is the point -- a rule that silently never fires reads exactly
  like a rule that is working.
- `from_fork` and `first_time_contributor` are now read from the pull request
  instead of being assumed false, so rules keyed on them match. `Model:` joins
  the commit trailers, and two new rule conditions read it: `authored_by_model`
  (a model declared itself the author, or none did) and `models` (the declared
  model is one of these). It stays a declaration rather than a detection --
  `true` means somebody said a model wrote this -- so like every other
  condition it may only add a review, never waive one. The case it exists for
  is a repository that reviews with one model and does not want that model
  reviewing its own output.
- `aethyme broker review tick --session <id> --repo <owner/name>` runs
  `review run --from-provider` across a repository's open pull requests, oldest
  first, and prints one report for the sweep. This is the whole scheduler: the
  broker still starts no background poller, so a cron entry, a CI step, a hook,
  or a person runs this one bounded pass. A pull request that fails is recorded
  in the report and skipped rather than ending the sweep.
- `scripts/adapters/chau7-review-adapter.py` is the missing half of the Chau7
  route: it runs a tick, and for each handoff prepares the review worktree
  through `broker git`, opens a Chau7 tab in it, and starts the reviewing
  agent. A review that starts is moved to `running`; one that cannot start is
  moved to `abandoned`, the single state the router will ask about again; an
  adapter that dies in between leaves the row `requested` for
  `stale_after_minutes` to reclaim. The broker still never talks to Chau7 --
  it decides, and the caller holding the transport performs.

### Fixed

- The broker now runs a `git` it has proven does not rewrite porcelain output,
  instead of whatever PATH resolves. A wrapper script that decorated every
  command with six bytes made `git status --porcelain` non-empty on a
  spotlessly clean checkout, so `is_dirty` answered "dirty" for every worktree
  on the machine, permanently -- `broker cleanup --all-cleaned` reported 0 of
  11 eligible and 0 bytes reclaimable where 11 and 4.2 GiB were actually free
  (#176). Candidates on PATH are probed against a known-empty repository and
  the first honest one is used, so a wrapped machine repairs itself with no
  operator action; `certify.git-output` now reports that same probe rather than
  a second copy of it.
- `GitRepo::is_dirty` asks whether there are any dirty *paths* instead of
  whether the output was non-empty. It was the last porcelain reader in the git
  module that skipped the validated status grammar added in #43 -- the grammar
  that already discarded exactly this decoration everywhere else.
- When no `git` on PATH survives the probe, the dirtiness predicates refuse
  with an error naming the wrapper rather than returning a verdict. The refusal
  is deliberately narrow: only the answers that authorize removing a worktree
  or block a push, because a wrapper that *swallows* a status line makes a
  dirty tree read clean, and that mistake costs work rather than disk. A probe
  that could not run is not proof of a wrapper and does not refuse.
- A routed review now closes its own tab and its own ledger row when it
  finishes. A reviewer's shell is interactive and never exits, so until now
  nothing ever closed one: a finished review held its workspace,
  `dispatch_review` refuses to spawn into an occupied workspace, and the
  dimension stayed blocked until `stale_after_minutes` expired the row as
  `abandoned` -- recording a review that ran and posted as one where nobody
  looked. Teardown runs before dispatch within a tick, because a tick may both
  reclaim a dimension's workspace and dispatch a new review into it, which is
  what the first tick after a push to an already-reviewed pull request does.
  Known limitation: a tab is matched to its workspace by an exact `cwd`
  comparison, so a reviewer that runs `cd` out of its workspace -- ordinary
  behaviour for an agent reading a repository -- is never matched and never
  closed. See the upgrade guide's known issues.
- `review state --head` accepts an abbreviated commit. `review ledger` prints
  twelve characters and this command required forty, so the one spelling of the
  head an operator could see was the one spelling it refused -- while its
  refusal named `review ledger` as where to look. A short head now resolves
  against the heads recorded for that pull request and dimension, the way `git`
  resolves a short commit, case-insensitively. Two candidates under one prefix
  are refused by name rather than settled by recency: the reporter cannot say
  which review it performed, and taking the newer one would file a verdict
  against a commit nobody read. A prefix that is not a commit -- empty, shorter
  than git's four-character floor, or not hexadecimal -- is refused as a usage
  error before it is matched against anything, so `--head "$HEAD"` with `HEAD`
  unset cannot resolve to whatever review the dimension happens to hold.

### Changed

- `stale_after_minutes` has a second meaning and repositories should re-tune it.
  It used to buy back only a concurrency slot; now a review that crosses it is
  marked `abandoned`, `abandoned` releases the workspace, and releasing the
  workspace force-closes the reviewer's tab. It is a kill timer on a live
  session. That reverses the asymmetry it was usually set under: too long costs
  a held slot on a genuine hang, which an operator undoes the moment they
  notice; too short kills a working reviewer mid-thought and files its
  dimension as never answered, which nothing detects and nobody undoes. The
  default is unchanged at six hours, and a finished review no longer waits for
  the timer at all.

## [0.7.17] - 2026-09-10

### Added

- `aethyme plugin install|status|remove` installs the agent-surface plugin from
  the CLI that serves its hooks. The plugin ships files and the CLI ships the
  logic behind them, so the two can be installed separately and end up
  mismatched -- which produces no error, only a plugin that silently does
  nothing. Installing through the CLI makes that mismatch impossible.
- `aethyme plugin status` reports whether the `aethyme` on `PATH` -- the copy
  the hooks actually reach, which is not necessarily the one you invoked --
  serves `aethyme hook`, and exits nonzero when an installed plugin sits in
  front of one that does not. It determines this by running the subcommand
  rather than comparing version strings, so a development build tagged from an
  earlier release is not misreported as too old.

### Changed

- The hook shim records the CLI's exit status alongside the event name in its
  fallback telemetry, distinguishing a CLI too old to serve `hook` from a
  current one that deliberately had nothing to say. Both look identical from
  outside.

### Upgrade notes

- No database migration; the schema stays at 32 and rollback is unrestricted.
  Release artifacts and their exact source revision remain recorded in the
  signed `release-manifest.json`.

## [0.7.16] - 2026-09-09

**Breaking:** see [UPGRADING.md](UPGRADING.md#v0716).

### Added

- A session whose work reached the default branch through a provider-side
  squash or rebase merge can now be recorded and closed. `aethyme broker
  representation scan --session <id>` searches the commits the default branch
  gained since the session branched and reports the commit whose content
  matches the session's own changed paths; `representation record --session
  <id> --confirm <digest>` stores it. `broker finish` treats a recorded
  representation as delivery evidence, so the session no longer reports its
  work as unsubmitted forever.

### Fixed

- `broker finish` no longer refuses a session solely because ancestry cannot
  see its work. A squash merge rewrites the commit SHA, so ancestry reports a
  false negative for every pull request merged that way.

### Upgrade notes

- Broker database schema moves from 31 to 32, adding the
  `session_representations` table. The migration is additive and runs on first
  write. Older binaries cannot read a migrated database, so upgrade the router
  and its engine sibling together, as always.

## [0.7.15] - 2026-09-08

### Added

- Pull request review activity is routed back to the session that opened the
  pull request. `aethyme broker watch pr monitoring activate --session <id>`
  opts a session in; while active, a pull request opened through `broker gh`
  starts its watch automatically. Off by default, so no session is interrupted
  without asking.
- `aethyme broker deliveries resolve-tab` and `deliveries dispatch` decide
  which Chau7 tab is running a session and whether it can take a message now.
  Resolution is by worktree identity, and refuses when it cannot pick exactly
  one tab.
- A Chau7 transport adapter and a launchd agent template under
  `packages/aethyme/scripts/adapters/`, so the loop runs on a schedule. The
  broker itself still starts no background poller.
- `broker status` reports unresolved coordinated operations, naming the holder,
  how long it has held, and what is parked behind it.

### Fixed

- A deferred delivery is no longer re-claimed immediately, which previously
  starved every delivery behind it for that adapter.
- Checkout discovery no longer escapes a removed merge-simulation worktree and
  resolves the enclosing repository.
- `main reconcile` usage text lists the resolution flags it accepts, and the
  CLI reference no longer documents a `gates affected --why` flag that was
  never implemented.

## [0.7.14] - 2026-09-07

### Added

- `aethyme broker main reconcile` accepts reviewed dispositions for local-only
  commits it cannot prove represented. `plan --write-resolution-template <path>`
  emits the commits needing a decision, and `--resolution-file <path>` applies
  them to both `plan` and `apply`. `archive_local` accepts that a commit leaves
  the default branch while staying reachable from the preservation ref;
  `replay_through_broker` and `keep_local_and_block_publication` continue to
  refuse. A commit with no entry stays undecided and keeps refusing, and
  `already_represented` remains computed from content rather than assertable.

### Upgrade notes

Broker storage, repository deployment, engine protocol, graph
cache schemas, and the generated agent policy are unchanged from v0.7.13.

## [0.7.13] - 2026-09-07

### Changed

- The release runbook states the enhancement redeploy as an explicit step, with
  the two orderings it depends on: build at the new version before deploying,
  because the policy stamp comes from the compiled version, and stage new files
  before deploying, because the generated freshness digest counts tracked files.

### Upgrade notes

This release changes maintainer documentation only. Broker
storage, repository deployment, engine protocol, graph cache schemas, and the
generated agent policy are unchanged from v0.7.12.

## [0.7.12] - 2026-09-07

### Added

- The generated agent policy carries three practices that only bite at fleet
  scale, each with its reason: do not merge the default branch into a working
  branch to keep it current unless freshness is actually required; a check may
  be required on a pull request only if it is scoped to the diff and finishes in
  minutes; cherry-picking patches an already-certified release rather than
  assembling one from trunk.

### Changed

- A host-state permission failure now distinguishes its two remedies. Granting
  the process access keeps it coordinating with every other session on the host;
  pointing `AETHYME_HOST_STATE_DIR` elsewhere always succeeds but yields a
  private coordination domain that sees no other session's leases, sessions or
  host resources. Sandboxed execution is documented as supported on exactly
  those terms.

### Upgrade notes

Broker storage, repository deployment, engine protocol, and
graph cache schemas are unchanged from v0.7.11. The generated policy changes, so
`aethyme enhance deploy` will rewrite `AGENTS.md` and `CLAUDE.md` in enrolled
repositories.

## [0.7.11] - 2026-09-06

### Added

- `aethyme broker main reconcile plan` and `apply --session <id> --confirm
  <sha256>` reconcile a local default branch that carries commits integration
  does not. Representation is decided by content rather than ancestry, so work
  that landed through a squashed promotion is recognised even though its SHA
  differs. The apply refuses unless every local-only commit is represented and
  no tracked path is dirty, and preserves the pre-move tip first.
- `aethyme broker adopt` reports when paths a session still targets were renamed
  by a later promotion, naming the new path and the promoted entry responsible,
  instead of leaving a replay to fail as an opaque modify/delete conflict.
- Repositories may run a push's local hooks before taking the coordination lock
  with `[coordination] hooks_outside_lock = true`. The hook executes in a dry
  run against exactly the commits the real push will send, the push is re-planned
  under the lock and refused if anything moved, and only then pushed. Off by
  default: it skips every `pre-push` protection on the real push, not only a slow
  gate.

### Fixed

- A submit whose response was lost no longer strands its promotion. When the
  integration tip is a promotion this session produced that no promoted entry
  claims, a retry claims it instead of recording content-empty supersession,
  which previously left the commit unrecorded and refused publication.

### Upgrade notes

Broker storage, repository deployment, engine protocol, and
graph cache schemas are unchanged from v0.7.10.

## [0.7.10] - 2026-09-06

### Fixed

- Readiness remediation no longer captures every other `plan`, `apply`, or
  `recover` subcommand. `gc plan`, `ship plan`, `promotion-record plan`, and
  `checkpoint plan` each reach their own command again. Previously they returned
  a readiness plan **and its digest**, which the matching `apply --confirm`
  would have accepted, applying work the caller never reviewed.
- `gc apply` distinguishes a stale confirmation from a pending interrupted run.
  A stale one directs to a fresh `gc plan`; a pending one names the journal's
  digest and explains that no fresh plan can reproduce it.
- Confirmation mismatches for cleanup, checkpoint, promotion record, exposure
  reconciliation, and integration reconciliation no longer print the expected
  digest as a value to paste, and name the command that re-reviews instead.
  `ship` keeps both SHAs, which are inspectable, and warns that confirming the
  new prefix publishes unreviewed work.
- `ship plan` states whether the selected prefix represents all local work,
  naming excluded local-only commits and uncommitted tracked paths. "Ready"
  describes the prefix, not the repository.
- A refused `ship execute --sync-main` names a bounded, non-destructive recovery
  for every cause, including the `git log` range for what a fast-forward would
  discard and a preservation ref.
- Ship plans report the entries this push newly publishes rather than the whole
  included prefix, most of which is already on the remote.
- A finished session no longer lists its leases as active and unreleased after
  cleanup released them.
- `broker status` surfaces commits the local default branch carries that
  integration does not, so writes that never passed through submit are visible
  immediately rather than at publication time.
- `broker adopt` reports integration drift for every adoption, not only reuse,
  and says when pre-existing commits are not session-owned under the recorded
  baseline. `broker submit` names those commits instead of reporting that
  nothing remains to integrate.
- Repository-quality reporting resolves relative paths more precisely.

### Upgrade notes

Broker storage, repository deployment, engine protocol, and
graph cache schemas are unchanged from v0.7.9.

## [0.7.9] - 2026-09-06

**Breaking:** see [UPGRADING.md](UPGRADING.md#v079).

### Added

- Promote commits now credit everyone in the chain: the human remains the
  commit author, the broker becomes the committer, and the agent that produced
  the change is named in a `Co-Authored-By` trailer. Pass
  `--agent "<Name> <email>"` to `aethyme broker start` or `aethyme broker adopt`,
  or export `AETHYME_AGENT`. An agent that is not identified is omitted rather
  than guessed.

### Changed

- Broker storage writes schema 31. Schemas 1 through 31 are read and migrated.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v079)
before updating. Broker storage moves from schema 30 to schema 31, and the
migration is applied in place the first time a v0.7.9 binary opens the database.
Earlier releases cannot read a migrated database, so upgrade every Aethyme
installation that shares a repository together.

## [0.7.8] - 2026-09-06

**Breaking:** see [UPGRADING.md](UPGRADING.md#v078).

### Added

- `aethyme broker readiness` reports whether a repository is conflict-only,
  agent-ready, or parallel-ready, with `readiness plan`, `readiness apply
  --confirm <sha256>`, and `readiness recover --plan <sha256>` for
  digest-confirmed remediation.
- `aethyme broker gates doctor` diagnoses gate configuration, optionally
  probing each gate.
- `aethyme quality inspect` performs bounded optional repository-quality
  analysis. The former `ai-ready` spelling remains as a deprecated alias.
- `aethyme broker git` and `aethyme broker gh` accept `--no-wait` and
  `--queue-timeout <seconds>` to bound how long a coordinated operation queues
  for the repository write lock instead of waiting indefinitely.
- `aethyme broker queue --active` lists only non-terminal entries, and
  `aethyme broker advisories suppress <id>` silences a delivered advisory.

### Fixed

- Readiness remediation no longer captures every other `plan`, `apply`, or
  `recover` subcommand. `gc plan`, `ship plan`, `promotion-record plan`, and
  `checkpoint plan` each reach their own command again. Previously they
  returned a readiness plan **and its digest**, which the matching
  `apply --confirm` would have accepted, applying work the caller never
  reviewed.
- A host-resource holder that died without releasing no longer pins its pool
  until the lease TTL elapses. A provably absent holder is quarantined
  immediately and its capacity units stop counting toward pool occupancy, while
  namespaces and exclusive keys stay reserved until reconciliation proves
  cleanup.
- Resource conflicts name the owning lease, and say when waiting cannot resolve
  one because the holder process is gone.
- `aethyme broker resources release <lease-id>` explains that the command takes
  the grant JSON written at acquire, and names the `reconcile` invocation that
  reclaims a lease whose grant file is gone.
- A coordinated operation is recorded before it queues for the repository write
  lock, so a waiting command is visible in `operations list` for the whole wait
  rather than only once it acquires the lock. A blocked caller is also told
  which operation holds the lock and for how long.
- An identical coordinated command that is still pending for the same session is
  refused instead of queueing a duplicate mutation behind the first.
- A report whose filing already succeeded can no longer be filed twice after a
  later attempt fails. The duplicate guard now bars on any past success rather
  than on the most recent attempt.
- A lease-claim refusal names the sessions holding the overlapping leases and
  their status, `broker start` warns when the chosen base is behind the default
  branch, and a gate failure reports git-ignored paths the worktree lacks.
- `aethyme broker exec` distinguishes a wrapped command's own failure from an
  ownership violation found by the guard.
- Host-scoped coordination state reports the real cause when it cannot be
  opened, including under sandboxed execution.
- Broker worktree retention reclaims build artifacts, proving each retained path
  is git-ignored before removing it, and accounts for the bytes it reclaims.
- Agent-facing command output is bounded, and read commands can record their
  output size on request.

### Changed

- Broker storage writes schema 30. Schemas 1 through 30 are read and migrated.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v078)
before updating. Broker storage moves from schema 28 to schema 30, and the
migration is applied in place the first time a v0.7.8 binary opens the database.
Earlier releases cannot read a migrated database, so upgrade every Aethyme
installation that shares a repository together.

## [0.7.7] - 2026-09-04

### Fixed

- Graph enrollment now adds the final redb query store and its interrupted-
  index staging file to the Aethyme-managed `.gitignore` block. Existing
  managed blocks are upgraded in place, while committed fragments under
  `.aethyme/graph/` remain visible to Git.

### Upgrade notes

Broker storage, repository deployment, engine protocol, and
graph cache schemas are unchanged from v0.7.6.

## [0.7.6] - 2026-09-04

### Added

- Graph lifecycle reports expose phase timing, observable bytes,
  node/edge/file counts, and peak memory without putting runtime measurements
  into refresh authorization digests.
- Missing worktree graph stores can reuse a verified immutable host-cache
  artifact keyed by source tree, fragment manifest, engine/protocol version,
  and storage schema. Each worktree still receives a private redb copy.
- `broker promotion-record plan/apply` provides digest-confirmed recovery when
  integration contains a proven promotion whose queue record was interrupted.

### Changed

- Disabled graph status is a healthy, cheap no-op. Materialization validates
  and reads committed fragments directly; it never clones or indexes source.
- Committed graph objects are read through one binary-safe Git batch. On the
  9,363-fragment Playground, cold materialization fell from 393 seconds to
  20.3 seconds, and a verified cross-worktree cache install took 3.53 seconds.
- Broker preparation reports warn when gates exist but dependency setup is not
  configured. Main-checkout adoption names out-of-scope dirty paths, and status
  JSON includes a `sessions` alias for `agents`.

### Fixed

- Large committed graph batches no longer deadlock when both Git pipes exceed
  capacity.
- Failed coordinated pushes using implicit refspecs such as `HEAD`, a branch,
  or a tag are planned from exact refs, so unchanged remote evidence records a
  retryable failure instead of unnecessarily write-blocking the repository.

### Upgrade notes

Broker storage, repository deployment, and engine protocol are
unchanged from v0.7.5. Graph support remains opt-in.

## [0.7.5] - 2026-09-04

### Added

- `aethyme graph materialize --repo .` validates committed graph policy, the
  engine pin, and fragments against the exact `HEAD`, then atomically builds
  only the ignored worktree-local redb store. It never changes fragments,
  accepts no hidden network input, reports elapsed milliseconds in stable
  JSON, and is a no-op when the store is already current.
- Opt-in graph enrollment through `aethyme deploy --repo . --with-graph`, with
  `--graph-repository owner/name` for repositories where no canonical `origin`
  resolves. Enrollment writes policy plus the exact engine pin and defers all
  generation until those files have been reviewed and committed.

### Changed

- Graph support is disabled by default; a repository opts in explicitly.
- `--with-graph` is refused together with `--local-only`, because untracked
  policy cannot authorize shared committed fragments.

### Upgrade notes

Broker storage, repository deployment, and engine protocol are
unchanged from v0.7.4, and graph support stays off unless a repository enrolls.

## [0.7.4] - 2026-09-04

**Breaking:** see [UPGRADING.md](UPGRADING.md#v074).

### Added

- `broker gc` reclaims git-ignored build caches inside retained worktrees.
  Build caches hold no committed work, so they are considered independently of
  a worktree's cleanup disposition: a worktree whose provenance is unproven
  still has reclaimable bytes. A directory qualifies only when its name is
  recognized and a witness confirms it is a real cache.
- `broker gc` sweeps orphaned host worktree roots whose owning repository no
  longer exists, using the `.aethyme-worktree-root.json` breadcrumb. A root
  with no readable breadcrumb is reported, never removed.
- `broker gc plan` reports `estimated_retained_bytes` and
  `estimated_blocked_bytes` alongside `estimated_reclaimable_bytes`, so the
  plan states total disk pressure rather than only the bytes it will act on.
  Both totals are excluded from the authorization digest.
- `retention.retained_bytes_budget` declares a soft repository storage budget
  surfaced as non-blocking warnings in status, doctor, and finish. It never
  authorizes deletion.
- Declarative per-worktree dependency preparation.
- Scheduler adapter boundary.

### Changed

- Repositories under the system temporary directory no longer anchor worktrees
  in the implicit platform host-state directory. Worktree storage is
  host-scoped while the records owning it are repository-local, so an
  ephemeral repository previously left a tree no database could account for.
  An explicitly configured host state or worktree root is still honored.
- The autonomous build-cache sweep is disabled by default
  (`artifact_sweep_budget_ms = 0`); set a positive bounded duration to opt in.
  `artifact_reclaim_days` defaults to 14, deliberately longer than
  `closed_worktrees_days`.
- Generated agent policy output is more compact.
- Graph refresh follows a version-safe lifecycle.

### Fixed

- A recursive submit verification slot no longer deadlocks: the non-reentrant
  lock is released before promotion or a stale verification can re-simulate
  the same entry.
- Graph authority parity for standalone gates and pre-push.
- Graph freshness correctness.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v074)
before updating. Broker storage, repository deployment, and engine protocol
remain unchanged from v0.7.3. The GC plan schema moves from 1 to 2, so an
outstanding `.aethyme/gc-journal.json` written by an earlier version is
refused; complete or remove it before upgrading.

## [0.7.3] - 2026-09-03

### Changed

- `broker status` and `broker integration status` now distinguish a stale
  local promotion layer that is conclusively present upstream from unresolved
  or unrecorded integration divergence.

### Fixed

- A successful coordinated `gh pr merge` now refreshes the configured target
  branch through a separately journaled Git operation and automatically
  reconciles the complete recorded promotion layer when exact commits, stable
  patch identities, cumulative squash evidence, or path content prove it has
  landed upstream.
- Automatic post-merge cleanup refuses ambiguous, partially landed, pending,
  or unrecorded work and reports the exact reviewed reconciliation command
  instead of moving integration optimistically.

### Upgrade notes

Broker storage, repository deployment, and engine protocol
remain unchanged from v0.7.2.

## [0.7.2] - 2026-09-02

**Breaking:** see [UPGRADING.md](UPGRADING.md#v072).

### Added

- Durable pull-request watches observe metadata-only comments, reviews, and
  checks through a provider-neutral polling contract.
- New activity is normalized into deterministic, acknowledgeable batches with
  stable deduplication across repeated polls.
- A provider-neutral delivery outbox supports explicit notify, resume, and
  same-PR review-and-push policies with fenced claims and durable retries.

### Changed

- Delivery prompts contain allowlisted metadata and explicit safety guidance;
  comment and review bodies remain at the provider and are treated as
  untrusted when retrieved by an agent.
- Live delivery clients such as Chau7 are separate adapters. Aethyme owns
  observation, authorization, deduplication, and durable state without
  depending on any one transport.

### Fixed

- Existing provider activity seeds a new watch baseline instead of appearing
  as fresh feedback.
- Retry claims use worker and generation fencing, preventing a stale adapter
  from completing work reclaimed by another process.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v072)
before updating. Broker storage migrates from schema 25 to 28; repository
deployment and engine protocol remain unchanged.

## [0.7.1] - 2026-09-01

**Breaking:** see [UPGRADING.md](UPGRADING.md#v071).

### Added

- Closed review lifecycles can be reassigned to an exact-head live session or
  explicitly abandoned without deleting state, evidence, or generation
  history.
- Checkpoint recovery plans expose stable refusal codes and ordered,
  preservation-first next actions in JSON.

### Changed

- Closed sessions remain available for diagnostics but cannot claim leases or
  run review mutations.
- `broker repair` is limited to recorded submit and promoted-path conflicts;
  checkpoint drift now routes directly to the dedicated checkpoint planner.
- The maintainer release contract now records patch increments as the default;
  changing either leading version component requires explicit maintainer
  authorization.

### Fixed

- A session closed during draft/review coordination no longer leaves its pull
  request identity permanently locked without a supported recovery path.
- Rejected closed-session lease claims no longer persist phantom ownership.
- Unsafe checkpoint recovery no longer recommends repeatedly rebasing onto an
  integration history that can contain unrelated promoted work.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v071)
before updating. Broker storage migrates from schema 24 to 25; repository
deployment and engine protocol remain unchanged.

## [0.7.0] - 2026-09-01

### Added

- Review policies can opt into `github_check_run` evidence with an exact check
  name and trusted GitHub App slug, while formal GitHub approval remains the
  default.
- Sanitized evidence records the selected check ID, status, conclusion, app,
  and exact head SHA without retaining review comments or arbitrary provider
  payloads.

### Changed

- `broker review unlock` polls configured review evidence and can advance a
  check-backed lifecycle directly from `review_requested`; comment-only
  reviewers no longer require a synthetic approval webhook.
- Review-gated publication revalidates the configured evidence adapter against
  the exact pull-request head immediately before publishing.

### Fixed

- Automated reviewers that report through comments and a repository-owned
  check are no longer permanently excluded by the hardcoded
  `reviewDecision == APPROVED` condition.
- Wrong-app, stale-head, unsuccessful, unavailable, and truncated check-run
  evidence fails closed without running the validation-unlock mutation.

### Upgrade notes

Broker storage, repository deployment, and engine protocol
schemas are unchanged from v0.6.0; no mandatory migration is required.

## [0.6.0] - 2026-09-01

### Added

- `aethyme broker worktree-root` provides a read-only, structured placement
  plan with the canonical checkout identity, clone-specific key, preferred
  external root, and constrained fallback.
- Session-start reports now retain the selected worktree root, its source, the
  scanner boundary, and any fallback reason in text and JSON.

### Changed

- Broker-managed worktrees now live in private per-user host state outside the
  repository by default. Independent same-named clones receive distinct roots,
  and starts invoked from a broker worktree create siblings rather than nested
  checkouts.
- Existing `.aethyme/worktrees/` sessions remain cleanup-compatible and serve
  only as a reported fallback when the platform host-state root is unavailable.

### Fixed

- Repository-wide scanners no longer traverse broker-managed nested checkouts,
  preventing duplicate findings and runaway recursive work.
- Explicit roots inside the repository or another linked worktree are refused,
  while private ownership markers keep external cleanup fail-closed.

### Upgrade notes

Broker storage, repository deployment, and engine protocol
schemas are unchanged from v0.5.0; no migration is required.

## [0.5.0] - 2026-09-01

**Breaking:** see [UPGRADING.md](UPGRADING.md#v050).

### Added

- Gate scope manifests, authenticated external event ingestion, review
  lifecycles, lease-routing exports, and reviewed publication policies provide
  auditable coordination without making repository-specific integrations part
  of the broker core.
- Broker retention and garbage-collection policies bound historical state and
  safely reclaim represented session worktrees, branches, events, operations,
  advisories, and expired resource leases.
- Atomic first enrollment publishes the complete repository contract or leaves
  the repository unchanged.

### Changed

- Publication evidence now controls exposure resolution: promoted paths remain
  visible until the exact entry is verified on remote main or is explicitly
  reconciled as an equivalent landing.
- Status, finish, cleanup, gate diagnostics, checkpoint recovery, and planned
  lease conflicts now report bounded, preservation-first next actions.
- Shared remote publication can require configured reviews and policy evidence;
  the default remains backward-compatible until those controls are enabled.

### Fixed

- Submission provenance, equivalent-tree leases, divergent upstream counts,
  amended promoted checkpoints, and `git -C` operations are classified from
  their actual Git evidence instead of inferred wording or commit identity.
- Protected-branch commits require an active broker session, while ship can
  synchronize a clean main checkout without rejecting unrelated untracked
  files.
- Enhance verification reports the exact tracked provenance used to generate
  onboarding, avoiding false freshness claims.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v050)
before updating. Repository deployment stays at schema 1 and engine protocol
stays at 1. Broker storage advances to schema 24 and migrates automatically;
review the rollback limitation before first opening an existing broker database.

## [0.4.2] - 2026-08-29

### Added

- Operators can inspect retained broker-owned worktrees with
  `aethyme broker cleanup --all-cleaned` and explicitly apply the unchanged
  sweep with `--apply`.
- Cleanup plans expose per-worktree eligibility and estimated reclaimable
  bytes in both text and JSON output.

### Fixed

- Cleanup safety now recognizes commits represented by local integration or
  configured upstream, so a successfully promoted session is not retained
  merely because the primary local main checkout has not been synchronized.
- Bulk cleanup revalidates every candidate and leaves adopted, dirty,
  symlinked, unsafe, uninspectable, or unrepresented worktrees untouched.
- Broker status warns when safely reclaimable cleaned worktrees remain.

### Upgrade notes

Broker storage, engine protocol, and repository deployment schemas are
unchanged; no repository migration is required.

## [0.4.1] - 2026-08-29

### Fixed

- Regenerated the checked-in AGENTS and CLAUDE protocol so the safe,
  preservation-first remediation for unsupported session merge commits is
  present in the released repository.
- Refreshed deterministic onboarding freshness metadata after the v0.4.0
  release inputs changed, restoring a clean `aethyme enhance verify` result
  on the exact release tree.

### Upgrade notes

Binary protocols and repository schemas are unchanged from v0.4.0; no
repository migration is required.

## [0.4.0] - 2026-08-29

### Added

- Rewritten session checkpoints have a digest-confirmed, preservation-first
  recovery plan instead of requiring an unsafe baseline reset.
- Blocked external-main reconciliation exposes a complete schema-2 resolution
  template with exact identifiers, structured evidence, field rules, and an
  atomic no-clobber writer.
- Broker command failures persist in allowlist-only diagnostic reports without
  command output, task text, diffs, file contents, or secrets.

### Changed

- Submission planning selects an explicit safe base, explains unsupported
  owned merge commits, and removes failed planning entries instead of leaving
  misleading submitted queue residue.
- Submission verification distinguishes missing gate configuration, no
  triggered gates, fresh execution, cache reuse, and failure.
- Guarded execution attributes changes to already-dirty files as well as newly
  dirty paths, so both remain subject to explicit lease ownership.

### Fixed

- Closed sessions can no longer authorize new coordinated Git or GitHub
  operations.
- External-main reconciliation recognizes stable patch-equivalent landings
  when local main already equals upstream and avoids replaying an empty commit.
- Session start refuses implicit or ambiguous bases rather than inheriting an
  unsafe checkout position.

### Upgrade notes

Broker storage remains schema 17, engine protocol remains 1,
and repository deployment remains schema 1, so v0.3.0 repositories do not
require a mandatory policy migration.

## [0.3.0] - 2026-08-27

**Breaking:** see [UPGRADING.md](UPGRADING.md#v030).

### Added

- Durable, non-blocking promotion advisories and queue-entry path exposures
  remain visible until verified publication or confirmed reconciliation.
- Operation history supports stable filtering, pagination, exact inspection,
  and evidence-based reconciliation of failed or ambiguous pushes.
- Repository upgrades expose exact content-and-mode plans, local diffs,
  customization resolutions, transactional apply, and crash recovery.
- Session handoffs, lease planning, gate provenance, cache bypass, canonical
  remote coordination, and host-wide resource leasing are available through
  stable broker surfaces.

### Changed

- Submission planning replays only session-owned commits from the accepted
  checkpoint and classifies ancestry, patch identity, and ambiguity before
  integration.
- Active sessions pin their repository contract, keeping diagnostic,
  recovery, finish, reporting, and eligible pre-commit lanes available across
  binary updates.
- Generated onboarding derives repository identity and surfaces from the
  tracked snapshot and uses content-addressed, reproducible freshness data.
- Generated AGENTS and CLAUDE policy teaches advisory delivery, persistence,
  acknowledgment, rebase, session-close, and publication lifecycles.

### Fixed

- Canonical remote identity and host-wide write barriers now serialize Git and
  GitHub mutations across independent clones without retaining credentials.
- External-main reconciliation preserves reviewed unrecorded work and updates
  integration and queue state under a digest-confirmed transaction.
- Failed exact pushes are classified from destination-ref evidence rather
  than stderr, while mixed or missing evidence remains safely unknown.
- Synthetic submission commits preserve reviewed contract decisions without
  copying arbitrary session commit bodies into integration.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v030)
before updating an enrolled repository. This release advances broker storage
to schema 17. Repository deployment remains schema 1, but regenerated policy
and onboarding should be reviewed per repository.

## [0.2.2] - 2026-08-24

**Breaking:** see [UPGRADING.md](UPGRADING.md#v022).

### Added

- Host-wide resource coordination allocates ports, Docker namespaces,
  database names, and capacity slots as atomic lease bundles with TTL,
  heartbeat, ownership credentials, and generation-fenced reconciliation.
- Gate definitions can declare shared host resources. Broker gate execution
  acquires those resources while preserving independent local test processes.
- The opt-in pre-push adapter proves the outgoing commit and runs the selected
  broker gates without making pre-commit hooks depend on shared services.
- `aethyme upgrade plan` and digest-confirmed `aethyme upgrade apply` migrate
  Aethyme-owned repository policy from logic embedded in the installed binary.

### Changed

- Canonical deployments track `.aethyme/repository.json`; local-only
  deployments keep their repository schema marker clone-local and ignored.
- Enrolled repositories fail closed on broker use when their generated policy
  is missing a required migration or is newer than the installed binary.
- Release manifests advertise repository schema compatibility, and successful
  paired-binary updates name the explicit per-repository follow-up.

### Security and reliability

- Repository upgrade plans bind Git HEAD, deployment mode, relevant file
  state, planned paths, and migrations into a full SHA-256 confirmation.
- Upgrades refuse dirty worktrees, mismatched confirmations, malformed or
  future markers, and managed paths that escape through symlinks.
- An in-progress marker prevents an interrupted migration from appearing
  current.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v022)
before updating an enrolled repository. This release advances broker storage
to schema 8 and introduces repository deployment schema 1.

## [0.2.1] - 2026-08-24

### Changed

- Stable release formula generation now emits Homebrew-audit-clean output
  while continuing to install both product binaries from one archive.

### Fixed

- Reusing an active broker session preserves its recorded submission baseline,
  so rebased or patch-equivalent commits cannot silently disappear from the
  ownership calculation.
- Submission, reuse-drift reporting, and finish handoffs now derive pending
  work from the same normalized submission plan.
- Patch-equivalent submissions whose content is already integrated are
  recorded as superseded without rerunning gates or creating an empty
  promotion commit.

### Upgrade notes

There is no broker storage or engine protocol migration from v0.2.0;
rollback is unrestricted.

## [0.2.0] - 2026-08-24

**Breaking:** see [UPGRADING.md](UPGRADING.md#v020).

### Added

- A local-first broker workflow covering isolated sessions, leases, guarded
  commands, gates, normalized submission, integration promotion and shipping,
  durable finish handoffs, and redacted offline issue reports.
- Read-only ship, lease, reconciliation, semantic-gate, submission-provenance,
  and report-planning surfaces with stable JSON output.
- Paired prebuilt binaries for Apple Silicon macOS, Intel macOS, and x86-64
  Linux, plus a stable-channel installer and explicit version pinning.
- A Sigstore-signed release manifest containing the exact source SHA,
  supported platforms, both required binaries, artifact sizes and SHA-256
  digests, compatibility boundaries, minimum Git version, and release channel.
- A public `schiste/homebrew-tap` formula that installs the router and engine
  from one archive and participates in normal `brew update` / `brew upgrade`.
- Explicit `aethyme update check`, `update plan`, and digest-confirmed
  `update execute` commands for installer-managed binary pairs.

### Changed

- The product and development test stack are now entirely Rust. The retired
  `python -m src.cli` entry point is a deliberate hard break with no shim.
- All production crates inherit one workspace version. Both `aethyme` and
  `aethyme-engine-cli` report that version and the embedded source description.
- Commit hygiene is driven by one typed policy. Bodies remain mandatory for
  `fix`, `feat`, `refactor`, and `perf`; non-substantive types may be
  subject-only; inline section content is accepted.
- Submission simulation replays session-owned patches onto integration and
  classifies exact and stable patch-equivalent history instead of treating two
  commit identities as an undifferentiated merge.
- The portable installer and native updater use one versioned pair layout with
  an atomic activation link and one retained rollback bundle.

### Fixed

- Doctor version repair now installs and verifies both required binaries.
- Task-next output deduplicates top anchors from the same file while preserving
  first-seen ranking across map and redb paths.
- External default-branch movement can be planned and reconciled without
  blanket deletion of unrecorded work.
- Successful pre-commit gates stay quiet, while failures retain their complete
  output and exit diagnosis.

### Security and release integrity

- Release archives are smoked after extraction on every supported target.
- Standalone checksums and `SHA256SUMS` are published; the manifest and
  installer digest are covered by a keyless Sigstore bundle.
- Semantic graph gate suggestions remain advisory and never expand enforced
  gates or submit-time checks.
- Report snapshots use an explicit allowlist and omit task text, reasons,
  absolute paths, file contents, diffs, and hunks by default.

### Upgrade notes

Read [UPGRADING.md](UPGRADING.md#v020)
before upgrading an existing broker repository. It covers the paired-binary
requirement, broker database backup/migration, graph-store regeneration,
compatibility, rollback, and known issues.
