# Windows Port: Scope and Estimate (P5.2)

Last Updated: 2026-09-26

Status: scoping only. No port code exists or is planned by this document.
Date: 2026-09-26. Baseline: `1d9f5a35` (all file:line references are at that
commit, relative to `packages/aethyme/rust/crates/`).

Method: static inspection of the Rust workspace, the scripts and the CI
workflows. A Windows cross-check (`cargo check --target x86_64-pc-windows-msvc`)
was **not** run: no Windows target is installed on the scoping host. That
check is the first task of any native option below.

## 1. What works on Windows today

| Surface | Native Windows | WSL2 |
|---|---|---|
| Release binaries | None. `release.yml:48-53` builds aarch64/x86_64 macOS and x86_64 Linux only; `install.sh:57-62` refuses anything else. | The x86_64 Linux release installs with `install.sh`. |
| Workspace compile | Does not compile (by inspection). No `cfg(windows)` code exists anywhere. | Same as Linux. |
| Crates with no un-gated Unix API | `graph-schema`, `graph-storage` (its Unix code is gated, `cache.rs:100`, `performance.rs:82`), `graph-indexer`, `producers`, `testkit` | n/a |
| Crates that fail to compile | `engine` (`daemon.rs:45` `UnixListener`, `daemon.rs:186` `setsid`), `enhance` (`skills.rs:51`, `deploy.rs:211`, `local.rs:381` `PermissionsExt`), `quality` (`fix/github.rs:346` `localtime_r`), `broker` (`operations.rs:13` `os::fd`, `update.rs:8`, `gates.rs:2239`, `disk_headroom.rs:31`, `blockers.rs:1183`), and `cli`, which depends on all of them | n/a |
| Existing non-Unix fallbacks | 20 `cfg(not(unix))` blocks. Most either no-op (`host_state.rs:152` permissions) or return a hard error, for example `graph_refresh.rs:2054` "transactional graph refresh requires Unix file locking", `cache.rs:110`, `repository_upgrade.rs:1460` | n/a |

Conclusion: nothing works natively. WSL2 already works to the extent that Linux
does, because it runs the Linux binary unchanged.

## 2. Unix dependency inventory

Counts cover production code (`src/`, excluding `tests/`) unless noted.
`cfg(unix)`/`cfg(not(unix))` attributes: 144 across 40 files, including tests.
`cfg(target_os)`: 10 sites (`host_state.rs:45,137`, `gates.rs:917-965`,
`performance.rs:82-84`, `cache.rs:68`, `symbol_index.rs:225`).

| Area | Sites | Where | Centrality |
|---|---|---|---|
| **File locking.** Portable helper `file_lock.rs` (std `File::lock`, which is `LockFileEx` on Windows) | 4 users | `advisories.rs:20`, `gates.rs:1237` (gate owner locks), `verification.rs:166`, `host_operations.rs:294` | Already portable |
| Raw `libc::flock` | 11 calls in 5 modules | `operations.rs:1020,1056,1070,1343` (per-repo operation lock), `graph-storage/cache.rs:102`, `cli/graph_refresh.rs:1714,2049`, `cli/repository_upgrade.rs:774,1453`, `cli/repository_enrollment.rs:1112` | Core: serialises remote operations, graph refresh, upgrades |
| Broker DB | 1 | `store.rs:278` SQLite WAL + busy_timeout (rusqlite `bundled`) | SQLite locks natively on Windows |
| **Process groups.** `process_group(0)` | 2 | `gates.rs:2239` (gate runner), `resources.rs:1005` (supervised resources) | Core |
| `killpg` SIGTERM/SIGKILL | 7 | `gates.rs:1043,2277,2349,2385,2394`; `resources.rs:1053,1079` | Core: gate timeout and cancel |
| Signal mask + `sigwait` forwarding | 1 struct, 4 calls | `resources.rs:926-970` (`BlockedSignals`: SIGINT/TERM/HUP), `resources.rs:1007` (`pre_exec` restores mask) | Core for `resources run` |
| Process identity before signalling (P2) | 3 | `gates.rs:902` `signal_target`; start time from `proc_pidinfo` (`gates.rs:917`, macOS) or `/proc/<pid>/stat` (`gates.rs:948`, Linux); other targets return `None` (`gates.rs:957`), so every signal is refused | Safety-critical |
| Liveness `kill(pid, 0)` | 5 | `operations.rs:1120`, `resources.rs:1634`, `blockers.rs:1183`, `engine/daemon.rs:150`, `engine/repo_cli.rs:234` | Stale-holder cleanup |
| Other signals | 2 | `engine-cli.rs:940` (SIGTERM stops daemon), `daemon.rs:186` (`setsid` detach) | Daemon lifecycle |
| **Shells.** `sh -c` in production | 5 | gate commands `gates.rs:2226`; pre-commit hook `hooks.rs:728`; `start --cmd` `broker.rs:3144`; resource cleanup `resources.rs:791` | Core: every gate |
| Generated `#!/bin/sh` scripts | 5 templates | git hooks `hooks.rs:449`; git wrappers `git.rs:2251,2274,2365,2426` | Git for Windows runs these through its bundled `sh` |
| Repo bash scripts | 8 `.sh`, 0 `.ps1` | `install.sh` (189 lines, `uname`/`curl`/`tar`); plugin hook `plugins/aethyme/hooks/aethyme-hook.sh` (invoked as `bash …` by `hooks.json:8-41`); deployed hook `.claude/hooks/aethyme-load-context.sh` (`enhance/deploy.rs:26`); adapters `aethyme-pr-monitor.sh` (+ launchd `.plist`), `codex-luna-review.sh`; eval/bench scripts | Install and agent integration |
| **Paths.** Mode bits `PermissionsExt` | 33 uses in 18 files | e.g. `host_state.rs:146` (0700/0600), `enhance/deploy.rs:211` (exec bit), `graph-storage/manifest.rs:190` (mode in manifest) | Mostly no-op-able; the manifest mode must be normalised |
| Symlinks in production | 1 | `update.rs:1163` `atomic_symlink` (versioned `current` link) | Self-update; Windows symlinks need Developer Mode or admin |
| Host-state dirs | 9 `HOME` reads in 7 files | `host_state.rs:45` (`~/Library/Application Support/Aethyme` or `~/.local/state/aethyme`), `host_state.rs:137` (cache) | `HOME` is normally unset on Windows, so host state resolves to `None` |
| Temp and socket dir | 1 | `daemon.rs:90` falls back to `/tmp` | Engine |
| Byte-path conversions `OsStrExt`/`OsStringExt` | 5 | `disk_headroom.rs:31`, `onboarding.rs:3651,3662`, `snapshot.rs:181`, `repository_upgrade.rs:971` | Small (UTF-16 paths) |
| Misc libc | 4 | `statvfs` `disk_headroom.rs:36`; `getrusage` `performance.rs:74` (gated); `O_NOFOLLOW` `repository_upgrade.rs:1446` (gated); `localtime_r` `quality/fix/github.rs:346` | Small |
| PATH-style joins | 1 | `gates.rs:2235` `AETHYME_GATE_OWNER_PATHS` joined with `:`; `C:\` paths collide | Small but silent |
| **IPC.** Unix domain socket | 1 module | `engine/daemon.rs:45,66` (`SOCKET_PREFIX` `engine-<hash>.sock`), 7 connect/bind sites. Explore has no in-process path: the router auto-starts the daemon (`cli/main.rs:754-762`) | Core for Explore |
| **Git.** Worktree root | 1 | host-state dir + `<repo-hash>/<slug≤40>` (`broker.rs:9911`) | See §3.6 |
| Crate dependencies | 5 crates | `libc = "0.2"` in `engine`, `graph-storage`, `quality`, `cli`, `broker`, with no `[target.'cfg(unix)']` table. No `nix`, `rustix`, `fs2` or `windows` crate | `libc` builds on Windows but lacks these symbols |

## 3. Work per subsystem (native port)

Effort is in engineer-days for one engineer who knows the codebase, and it
includes tests. Risk is how likely the estimate is to be wrong.

| # | Subsystem | Replacement | Effort | Risk |
|---|---|---|---|---|
| 3.1 | Compile scaffolding | Move `libc` under `[target.'cfg(unix)'.dependencies]`; add `cfg(windows)` arms or stubs at every site above; add the `windows-sys` crate | 2–3 | Low |
| 3.2 | File locking | Route the 11 raw `flock` calls through `file_lock.rs` (std `File::lock`/`try_lock`, which are `LockFileEx` on Windows). Audit every reader of a lock file: Windows locks are mandatory byte-range locks, so another process cannot read a locked range (`graph_refresh.rs:2060` writes `pid=` into its lock) | 2–3 | Low–Med |
| 3.3 | Process groups, signals, identity (P2) | Gates and resources: spawn into a Job Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), with `CREATE_NEW_PROCESS_GROUP`; graceful stop is `CTRL_BREAK_EVENT` then `TerminateJobObject` after the grace period. Identity: `OpenProcess` + `GetProcessTimes` creation time replaces `proc_pidinfo`/`/proc`. Liveness: `OpenProcess` + `GetExitCodeProcess`. `BlockedSignals`: `SetConsoleCtrlHandler`. The gate pidfile records a `pgid`, which has no Windows meaning, so the record format needs a job or handle story | 8–12 | **High** |
| 3.4 | Shells | Require Git for Windows and run `sh.exe -c` (resolved through `split_paths`, as for git). Keeps `gates.toml`, hooks and `--cmd` identical across hosts. The alternative, `cmd /C` or PowerShell, forks every gate definition by OS. Port `install.sh` to `install.ps1` plus a `.zip` release. The plugin hook already calls `bash`, which works only when Git Bash is on PATH | 3–5 | Med |
| 3.5 | Paths and host state | `%LOCALAPPDATA%\Aethyme` via `SHGetKnownFolderPath` or the `dirs` crate; `%TEMP%` for `/tmp`; make mode bits a no-op except the manifest's `file_mode`, which needs a stable Windows value; drop exec-bit checks (`ensure_executable`); replace `atomic_symlink` with a pointer file or junction; `GetDiskFreeSpaceExW` for `statvfs`; `std::env::join_paths` for `AETHYME_GATE_OWNER_PATHS`; UTF-16-safe path hashing | 4–6 | Med |
| 3.6 | Git worktrees | Set `core.longPaths=true` on managed worktrees and keep the root short (260-char `MAX_PATH`); retry `worktree remove` and gc deletes on `ERROR_SHARING_VIOLATION` (open handles, indexers, antivirus); pin `core.autocrlf=false` where diffs or tree digests are compared; replacing a running `.exe` during `update` needs a rename-then-replace step. Case-insensitivity is already exercised on default APFS | 3–5 | Med–High (unknowns) |
| 3.7 | IPC (engine daemon) | Named pipes through the `interprocess` crate's local sockets (std has no pipe API and no Windows `AF_UNIX`). Detach with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP` instead of `setsid`. Stop through the existing `shutdown` request instead of SIGTERM (`engine-cli.rs:940`) | 3–5 | Med |
| 3.8 | Test-suite portability | See §4 | 8–12 | Med–High |
| 3.9 | CI and release | `windows-latest` job, `x86_64-pc-windows-msvc` in the `release.yml` matrix, zip packaging, update the `release_contract`/`release_installer` tests, and a winget or Scoop manifest if wanted | 3–4 | Low |
| | **Total** | | **36–55** | |

Add 20–30% contingency for 3.3 and 3.6, where Windows behaviour has to be
discovered rather than looked up. Realistic range: **45–70 engineer-days**
(about 2–3 months for one engineer).

## 4. CI and test-suite portability

| Fact | Value |
|---|---|
| Test functions (`#[test]`) | 2,776, of which 1,307 are in 140 integration files |
| Integration files touching Unix (`sh`, shebang fakes, `os::unix`, `libc`, `/tmp`, mode bits, symlinks) | 32 files, 360 tests |
| `src/` files with the same patterns (upper bound, covering product and test code) | 62 files, ≤581 tests |
| Shebang fake executables written by tests (fake `git`/`gh`/`aethyme` on PATH) | 53 |
| Tests already `#[cfg(unix)]`-gated (skipped, not failing, on Windows) | 40 |
| Current CI OSes | `ubuntu-latest` (`oss-ci.yml`, `aethyme-gates.yml`, `cross-process-contract.yml`, `aethyme-local-tests.yml`), `macos-14` (`oss-ci.yml:210`, `macos-nightly.yml`). No Windows job |

Roughly 350–900 tests assume Unix, directly or through a harness. The main
cost is the fake-executable pattern: Windows resolves `PATHEXT` (`.exe`,
`.cmd`), not shebangs, so each fake becomes either a `.cmd` shim or a small
compiled helper binary in `testkit`. A compiled helper is the durable choice.

CI needs:
1. `cargo check --target x86_64-pc-windows-msvc --workspace` on
   `windows-latest`. This is a compile-only guard that stops new un-gated
   Unix code. It is diff-scoped and takes minutes, so it can be a required
   pull-request check.
2. `cargo test --workspace` on `windows-latest` with Git for Windows (it is
   preinstalled on hosted images). Run it nightly, not per pull request,
   until it is stable.

For option (b): GitHub-hosted Windows runners do not provide the nested
virtualisation that WSL2 needs; the `setup-wsl` action is understood to support WSL1 only.
Verify this before planning a WSL2 job. The existing `ubuntu-latest` jobs
already test the exact binary that WSL2 runs, so a WSL2 CI job adds little.

## 5. Options

| Option | Scope | Code | Effort | Risk | Covers |
|---|---|---|---|---|---|
| **(a) Full native port** | §3 in full | Large; touches P2 process safety | 45–70 d | High | Everything, including the broker on NTFS |
| **(b) WSL2-only** | Document WSL2 and its limits; give an explicit refusal in `install.sh` under Git Bash/MSYS; add a manual smoke checklist; rely on `ubuntu-latest` CI | Near zero | 1–3 d | Low | Everything, inside WSL2 |
| **(c) Native subset: Explore + graph, no broker** | Feature-gate `broker` (and the broker-only commands) out of `cli`; 3.1 for engine/enhance/quality; 3.2 for the graph cache, refresh and upgrade locks; 3.5 without the broker parts; 3.7 (or an in-process Explore mode instead of the daemon, about 2 d); Windows CI for those crates | Moderate | 15–22 d | Med | Read-only navigation; no coordination, gates or hooks |

Limits of WSL2 to document for (b):
- Keep repositories on the Linux filesystem (`~/…`), not `/mnt/c`. Performance
  and POSIX lock/mode semantics on the Windows mount were not verified.
- Run the agent (Claude Code, Codex) inside WSL, so hooks and `aethyme` resolve
  in the same environment.
- x86_64 only. There is no `aarch64-unknown-linux-gnu` release
  (`install.sh:57-62`), so Windows on ARM would need that target added
  (small, separate from this port).

## 6. Recommendation

**Adopt (b) now; defer (a) and (c) until demand exists.**

- (b) costs 1–3 days and gives Windows users the whole product, including the
  broker, with no change to the process-safety code that P2 hardened.
- (c) buys only the read-only half of the product, and still pays for the
  engine IPC rewrite. It is worth doing only if a concrete user wants Explore on
  native Windows without WSL.
- (a) costs about 2–3 engineer-months, and most of the risk is in 3.3. A
  second process-control implementation (Job Objects) doubles the surface
  that P2 had to prove safe. Start it only with a named user base and a
  maintainer willing to own Windows CI.

Total for the recommended path: **1–3 engineer-days.**

Prerequisites before any native work, (a) or (c):
1. Install the target and run `cargo check --target x86_64-pc-windows-msvc`
   to replace this document's by-inspection compile findings with measured
   errors, then re-estimate.
2. Decide the shell policy. The recommendation is to require Git for Windows
   `sh`, which keeps `gates.toml` portable.
3. Agree how gate pidfiles identify a process tree on Windows (a job handle
   instead of a `pgid`) before touching `gates.rs`.
4. Add the compile-only `windows-latest` check first, so the gap stops
   growing while the port is under way.
