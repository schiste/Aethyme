# L0 slice C: state stores, retention/cleanup reach, hooks

Last Updated: 2026-10-09 (L0 audit, #651)

Baseline: `5e8daf712e26c63f1d4082e9a617f556cf995de2` (session-1030 worktree). Read-only: no cleanup, gc apply, or broker mutation was run. Paths are relative to `packages/aethyme/rust/crates/aethyme-broker/src/` unless stated. "Confirmed" means read in code at the baseline; nothing was exercised unless a test is named.

Live environment observed (2026-10-09): `AETHYME_WORKTREE_ROOT=/Volumes/T7-repositories/Application Support/Aethyme/worktrees`. `AETHYME_HOST_STATE_DIR`, `XDG_STATE_HOME` and `AETHYME_HOST_CACHE_DIR` are unset.

## 1. Persistent roots

| Root | Owner / writer | Schema / versioning | Status | Evidence |
|---|---|---|---|---|
| `<repo>/.aethyme/broker.db*` | Broker store, per repository (main checkout) | `SCHEMA_VERSION = 49`, `MIN_COMPATIBLE_SCHEMA = 47`. A newer DB opens on an older binary only if `meta.min_compatible_schema` ≤ its version; otherwise `SchemaTooNew`. Migrations from v48 are idempotent and every open runs a repair pass. | confirmed | `schema.rs:36,68,73-97,1819-1886`; test `schema.rs::an_older_binary_opens_a_newer_database_only_when_declared_compatible` (:1922), `the_min_compatible_marker_is_never_lowered` (:1946) |
| `<repo>/.aethyme/{logs,locks,reports,run,reviews,worktrees}/`, `graph_store.redb[.indexing]`, `gc-journal.json`, `gc.lock`, `worktree-sizes.json`, `generated/experience-*`, `broker-action-required.md`, `broker-advisory.md` | Broker runtime catalog (single list feeds `.gitignore` managed block, ship classifier, deploy summary) | none (files) | confirmed | `runtime_paths.rs:62-80` `BROKER_RUNTIME_PATH_RULES` |
| Host state `H` = `$AETHYME_HOST_STATE_DIR` → `$XDG_STATE_HOME/aethyme` → macOS `~/Library/Application Support/Aethyme` / else `~/.local/state/aethyme` | shared by all repos on host; dirs 0700/files 0600 via `protect_host_state_path` | n/a | confirmed | `host_state.rs:37-50,148-160` |
| `H/host-operations.db` | host coordinated-operation journal | `SCHEMA_VERSION = 1`, **exact-match**: any other version → `UnsupportedSchema` (host-wide lockout, no compatibility floor) | confirmed | `host_operations.rs:16,87,310-317` |
| `H/host-resources.db` | host resource leases | meta `schema_version = 1`; request schema exact-match | confirmed (open-path compat not fully traced) | `resources.rs:15-22,1151,1164,1205` |
| `H/gate-trust/`, `H/console-markers/`, `H/operation-locks/`, `H/run/<key>/<ns>`, `H/preparation-cache/`, `H/recovery-archives/<repo-key>/` | gate trust records; console markers; op locks; verification slots; dependency prep cache; cleanup-resolve archives | per-file JSON schema versions | confirmed | `broker/gate_trust.rs:177`; `console.rs:590`; `host_operations.rs:288`; `verification.rs:54,81`; `preparation.rs:1070`; `cleanup_resolve.rs:39,223-237` |
| Worktree container `C` | session worktrees, one `<repo-key>/` per repo, each with `.aethyme-worktree-root.json` marker | `WORKTREE_ROOT_SCHEMA_VERSION`; marker const `broker.rs:98` | confirmed | precedence in `broker/lifecycle.rs:1184-1253` `worktree_root_plan`: library override → `AETHYME_WORKTREE_ROOT` → committed `[worktrees] root` (`worktree_location.rs`) → `H/worktrees` → legacy `.aethyme/worktrees` |
| Host cache `K` = `$AETHYME_HOST_CACHE_DIR` → `$XDG_CACHE_HOME/aethyme` → `~/Library/Caches/Aethyme` | `gates/<repo-key>/` gate cache, `graph-stores/`, `symbol-index/`, `update-manifests/` | n/a | confirmed | `host_state.rs:129-140`; `gate_cache_gc.rs:45-53`; `aethyme-graph-storage/src/cache.rs:75`; `aethyme-engine/src/explore/symbol_index.rs:236`; `update_cache.rs:41` |
| Repository identity key `<slug>-<sha256(canonical git-common-dir)[..16]>` | used for `C/<key>`, `H/recovery-archives/<key>`, `K/gates/<key>` | — | confirmed | `host_state.rs:19-35`. **Path-derived:** moving or re-cloning the repo changes the key. This must not be reused as a portable `ProjectId` (relevant to #652). |

## 2. Deleters and their reach

| Deleter | Trigger | Reaches | Guards | Status | Enforcing tests |
|---|---|---|---|---|---|
| Inline artifact sweep (`gc.rs::resume_gc_maintenance` → `sweep_artifacts_autonomously(Inline)`) | **Every broker open**, including plugin hook calls; 250 ms cap, cadence/spacing-limited | ignored regenerable build dirs in session worktrees (closed or idle-open) | `BUILT_IN_REGENERABLE` plus manifest evidence (`auto_cleanup.rs:30-60`, `reclaim.rs:317-405`); GC lock; cursor/interrupted keys | confirmed | `tests/gc_sweep_cli.rs::broker_opens_space_their_continuations_of_an_unfinished_sweep` (:1674) |
| Auto-removal of whole checkouts (#588) `auto_cleanup.rs:874 auto_remove_disposable_checkouts` | Called from the **general sweep, including Inline scope** when `closed_session_id` is None (`gc.rs:2551-2558`, `:2259-2262`) | session worktree + session branch | 4 proofs re-checked under GC lock: all sessions closed, no use, clean (no non-regenerable ignored file), contained in **fetched remote** default branch; `auto_remove=false` opt-out | confirmed by reading. **Comment/runtime mismatch:** `resume_gc_maintenance` doc (`gc.rs:2103-2109`) says the startup sweep "only removes git-ignored build caches", but it can reach whole-checkout removal. | `tests/auto_cleanup.rs::an_open_session_keeps_its_checkout` (:237), `auto_remove_false_disables_it_and_keep_pins_and_regenerable_globs_apply` (:476) |
| GC plan/apply (digest-confirmed, journaled) `gc.rs:1224-3180` | operator `gc plan` then `gc apply --confirm`; journal resumed on broker open (`gc.rs:2111-2117`) | `.aethyme/logs/gates/` row-referenced logs; rewrite of `.aethyme/logs/command-metrics.jsonl`; gate caches; recovery archives; **orphan roots in `C`** | `runtime_path` refuses paths outside `.aethyme/` or containing non-normal components (`gc.rs:705-717`); revalidation per item at apply; marker removed last | confirmed | `tests/gc_sweep_cli.rs::orphaned_roots_are_swept_while_owned_and_unmarked_roots_are_protected` (:157), `a_reappearing_repository_revokes_an_authorized_orphan_removal` (:681); `tests/recovery_archive_gc_cli.rs` |
| Orphan-root sweep `gc.rs:1117-1219 orphan_candidates`, apply `:3018-3058` | part of gc plan/apply | **every direct child of container `C`**: removes `C/<x>` iff it has a valid marker, the marker's `repository_root` no longer exists, and it is past `orphan_worktree_roots_days` | no marker / unreadable marker → `unmarked_worktree_root` blocker, never removed; real-directory check; repository reappearing revokes | confirmed | as above |
| Storage reclaim `storage.rs` (`gc storage` plan/apply) | operator, digest-confirmed | empty broker-bookkeeping roots in `C` (`remove_empty_root` :2208-2245, only `.DS_Store` and exact cargo `config.toml` allowed); dead `H/preparation-cache/<key>` entries (:1334-1350) | per-item re-proof; `is_infrastructure` skips dot-names (:2764); unmarked roots never disposable | confirmed | `tests/storage_cli.rs` (specific names not enumerated) |
| Storage attribution `storage.rs:2479-2560 storage_attribute` | operator `--apply` | **writes** markers on unmarked `C/<x>` iff name == this repo's key or all its Git worktrees are registered here | never replaces an existing marker | confirmed | `tests/worktree_root_cli.rs::cleanup_accepts_a_worktree_owned_by_the_external_root_marker` (:205) |
| `storage_container` (`storage.rs:2247-2276`) | storage plan/attribution | `AETHYME_WORKTREE_ROOT` else `H/worktrees` | **Ignores committed `[worktrees] root`**, which `worktree_root_plan` honours, so storage inventory can miss a repository-configured container. | confirmed (discrepancy) | none found |
| Finish cleanup / per-session cleanup (`broker/cleanup.rs`, `removal.rs`), bulk `finish cleanup --all-cleaned --apply --confirm` | operator / finish | the session's own worktree path and branch (compare-and-delete) | digest, revalidation, landed proof | confirmed (not re-traced in depth here) | `tests/cleanup_cli.rs`, `tests/finish_cli.rs`, `tests/git.rs::a_dirty_worktree_refused_without_force_is_not_removed_by_the_orphan_recovery` (:210) |
| Gate cache GC `gate_cache_gc.rs` | gc plan/apply | `K/gates/<repo-key>/` incl. `.<key>.retired-<ms>-<pid>` | liveness via registry | confirmed | `tests/gate_cache_inventory.rs` |
| Install updater `update.rs:1464-1485 cleanup_old_bundles` | `aethyme update` | non-current/previous dirs in the install `versions_dir` | keeps link targets | confirmed | — |
| Repository upgrade/deploy `aethyme-cli/src/repository_upgrade.rs` | `aethyme deploy` | only journaled transaction artifacts (replacement/tombstone paths) | rollback journal | confirmed | — |
| Graph store lifecycle `aethyme-engine/src/store/redb/graph_store/lifecycle.rs:144-196` | graph refresh | own staging/db files | — | confirmed (path-local) | engine tests |
| Git hooks `hooks.rs` | explicit `hooks install/uninstall` only | `<git-common-dir>/hooks/*` marker blocks; deletes a file only if it was nothing but our shim | foreign hooks untouched | confirmed | `hooks.rs::marker_block_strip_and_replace_preserve_user_content` (:1272), `tests/hooks_e2e.rs` |

**No deleter enumerates the host-state root `H` itself.** Every consumer joins a fixed subpath (`grep default_host_state_dir()`: `cleanup_resolve.rs:225`, `verification.rs:54`, `console.rs:590`, `host_operations.rs:87`, `storage.rs:2254`, `resources.rs:1164`, `broker/gate_trust.rs:177`, `broker/cleanup.rs:1289` (headroom probe, read-only), `broker/lifecycle.rs:1187`). Status: confirmed by grep at baseline.

## 3. T35 / D09: where a new collaboration state root can live

| Candidate | Reached by an existing deleter? | Verdict |
|---|---|---|
| `H/collaboration/<project-id>/` (sibling of `recovery-archives`) | No enumerator of `H`; not under `C` unless `C` is `H/worktrees` and the new dir is placed inside it. Precedent: `recovery-archives` already lives here and is GC'd only by its own explicit rule. | **SAFE today** (confirmed by reading; needs a T35 test that runs gc plan/apply, storage apply and sweep with the dir present) |
| Inside container `C` (e.g. `C/collaboration` or `C/<repo-key>/collab`) | `C/<x>` with no marker → blocker noise; **with** a marker, deletable once `repository_root` vanishes. Inside `C/<repo-key>/`, which is session-worktree territory; `storage` reconciliation and empty-root removal look here. `C` is environment-dependent (see §4). | **NOT SAFE** |
| `<repo>/.aethyme/collab/` (main checkout) | GC touches only named files under `.aethyme/` (gate logs, metrics rewrite) | Safe from deleters, but violates plan §6.3 (state must outlive a deleted or moved clone, and stay outside worktrees). Moving or re-cloning loses it. **Reject.** |
| `<session-worktree>/...` | Removed with the checkout by auto-removal or cleanup | **NOT SAFE** (and an ignored file there would block auto-removal: proof 3) |
| `K/...` (Caches) | gate-cache GC and OS cache purges | **NOT SAFE** for retained source |
| Existing host DBs (`host-operations.db`, `host-resources.db`) | Not deleted, but `host-operations.db` is exact-version: any schema change locks out **every** older binary on the host | **Do not extend**; use a separate `state.db` (matches plan §6.3) |

Old-binary behaviour toward unknown data:
- `broker.db`: an additive table or nullable column is invisible to old binaries (≥ v47); a non-compatible bump locks them out of that repo (`SchemaTooNew`).
- An unknown directory in `H` is ignored.
- An unknown unmarked directory in `C` is reported, never removed.
- An unknown file under a session worktree blocks auto-removal.

So an old binary cannot delete `H/collaboration/`. That is the only candidate above for which this holds without a fence.

## 4. Additional findings and follow-ups

1. **Environment-dependent container (confirmed, live).** Two containers exist on this host: `/Volumes/T7-repositories/Application Support/Aethyme/worktrees` (from the env var) and `~/Library/Application Support/Aethyme/worktrees` (default; it still holds `aethyme-6f13a03e16f294c6` and 21 other repo roots). Orphan sweep, storage inventory and attribution see only the container resolved in the invoking process's environment. A hook or agent shell without `AETHYME_WORKTREE_ROOT` inventories a different tree. Follow-up for #335/#656: record the resolved container per repo (DB/config) and report a mismatch. A collaboration root must not be derived from `AETHYME_WORKTREE_ROOT`.
2. **`storage_container` ignores `[worktrees] root`** while `worktree_root_plan` honours it (`storage.rs:2247` vs `broker/lifecycle.rs:1200-1240`). Follow-up for #335.
3. **Stale comment:** `gc.rs:2103-2109` says startup maintenance only removes ignored build caches, but the Inline sweep also invokes `auto_remove_disposable_checkouts` (`gc.rs:2551-2558`). That deletion is bounded by four proofs, but it is an implicit whole-checkout deleter on any broker open, including plugin hooks. T33 should record this as part of the legacy baseline ("broker open may remove a proven-contained closed checkout").
4. **Repository key is path-derived** (`host_state.rs:19-35`). Fine as a local storage key; not a `ProjectId` (#652).
5. **`.DS_Store`** is tolerated in `remove_empty_root` and in rmdir retry (`gc.rs:723-`), consistent with the known GC wedge.
6. **Plugin hooks** (`packages/aethyme/plugins/aethyme/hooks/hooks.json`): SessionStart, UserPromptSubmit, PreToolUse, PostToolUse and Stop each call `aethyme-hook.sh`, i.e. a broker open on every tool call, which therefore triggers the inline sweep. `plugin.json` is still `0.1.1` (#687).

## 5. #688 (`finish` → "is not inside a git repository"): code path (cause VERIFIED during synthesis)

> **Verified 2026-10-09 after this slice:** on the real repository, a fresh session 1031 failed `broker finish` after 10.7 s, then `broker finish --timeout 120` cleaned it in 13.2 s. The cause is a deadline expiry masked by `GitRepo::discover`, as reasoned below. Recorded on #688.

- The message is produced only by `GitError::NotARepository` (`git.rs:126`). In the broker it is constructed by `GitRepo::discover` (`git.rs:1165-1173`), which maps **any** `run_git` failure, including `GitError::TimedOut`, to `NotARepository` (`map_err(|_| …)`).
- `broker finish` runs `finish_with_options` inside `with_git_deadline(--timeout, default 10 s)` (`cli/session.rs:1418-1446`). Each git call's budget is the remaining deadline, floored at 1 ms (`git.rs:166-176`). `finish_with_options_inner` calls `GitRepo::discover(&worktree_path)` (`broker/finish.rs:557`) after store reads and `finish_leases`.
- `finish_timeout_error` (`cli/session.rs:1470-1505`) only rewrites errors containing `"did not finish within"`. A timeout swallowed by `discover` therefore surfaces as "not inside a git repository".
- Git is spawned with `Command::current_dir` + args (`git.rs:960-975`), with no shell. A space in the path is therefore **unlikely** to be the cause; #688's space hypothesis is probably wrong.
- `finish close`/`finish cleanup` do not run under that deadline, which is consistent with them succeeding.
- Cheap verification: on a throwaway session, `aethyme broker finish --session <id> --timeout 600`. If it succeeds, the cause is a deadline expiry masked by `discover`. Fix candidates: propagate `TimedOut` from `discover`, and let `finish_timeout_error` match `GitError::TimedOut` structurally.
