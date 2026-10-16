# L0 / T33 — legacy CLI, JSON and exit-code baseline

Last Updated: 2026-10-09 (L0 audit, #651)

- **Baseline:** `schiste/Aethyme` `5e8daf712e26c63f1d4082e9a617f556cf995de2`. Installed binary `aethyme 0.8.26 (build_commit=5e8daf71, build_date=2026-10-08T19:30:03Z)`. Captured 2026-10-09 on macOS (Darwin 27.2).
- **Disposable fixture:** a `mktemp -d` directory outside the repository (not retained).
  - `repo/`: a synthetic repo containing `README.md` and `main.rs`, with one commit on `main`.
  - `origin.git`: a local bare repo used as the remote (no network, no GitHub).
  - `host-state/`: host state isolated via `AETHYME_HOST_STATE_DIR`.
  - `wt-root/` and `Application Support/wt`: worktree roots, set via `AETHYME_WORKTREE_ROOT`.
- **Raw captures:** `caps/<label>.{argv,out,err,exit}` in the disposable directory; stdout and stderr were captured separately and nothing was filtered before inspection. They are not checked in; the tables below transcribe them.
- **Scope note:** no broker command was run against the Aethyme repository. Code reading was done in the #651 session worktree.

## 1. What is already pinned by tests

| Mechanism | Location | What it pins |
|---|---|---|
| Help snapshots: 84 files; regenerate with `AETHYME_UPDATE_SNAPSHOTS=1` | `aethyme-cli/tests/help_snapshots.rs`, `tests/snapshots/help/*.snap` | Help text for every surface below, including `broker-start`, `-status`, `-submit`, `-submit-prepare`, `-finish`, `-finish-close`, `-finish-cleanup`, `-push`, `-gc`, `-gc-storage`, `-advanced-gates`, `-advanced-ship`, `-advanced-hooks`, `deploy`, `hook`, `repo`, `init`. Help text only, **not** behavior. |
| JSON shape snapshots (key and leaf-type skeleton only) | `aethyme-cli/tests/json_shape_snapshots.rs`, `tests/snapshots/json-shapes/{start,adopt,submit,status,finish}.snap` | `--json` shape for exactly 5 commands, on the success path only. Every call asserts `status.success()`. The fixture isolates `AETHYME_HOST_STATE_DIR` and `AETHYME_AGENT_PID`, and removes `AETHYME_WORKTREE_ROOT`. |
| Events contract v1 (frozen) | `aethyme-broker/tests/contract_v1.rs`, `docs/events-contract.md` | Event kind catalog, envelope row and payload field names; any change needs an `EVENTS_SCHEMA_VERSION` bump. This covers events, not command stdout. |
| Per-command CLI tests | `aethyme-broker/tests/*_cli.rs`, `*_e2e.rs` (~110 files) | Behavior assertions per command. Exit codes are mostly checked as `success()` or `!success()`. Explicit `code() == Some(n)` checks are rare. The heaviest are `blockers_cli` (16), `store` (21), `closed_worktree_gc` (11), `gc` (9), `broker.rs` (8), `merge_chain_cli` (8) and `gates_cli` (7). |
| Other docs | `api-contract.yaml` | Describes the engine HTTP API (`/api/v1/...`), not the broker CLI. `parity-test-contract.md` and `broker-inspection-json.md` do not define an exit-code table. **No central exit-code contract exists.** |

### Coverage per legacy surface (plan §6.1)

| Surface | Help pinned | JSON shape pinned | Exit codes pinned | Gap |
|---|---|---|---|---|
| enrollment / deploy (`deploy plan/execute`, `init`) | yes | no (`repository_enrollment_cli.rs`, `repository_deploy_cli.rs` assert fields ad hoc) | partly (2 explicit checks in enrollment) | No shape snapshot for `deploy plan --json` (27 top-level keys). |
| start | yes | **yes** | success only | Refusal exit codes unpinned. |
| start --adopt / --reuse | yes (in `broker-start`) | adopt **yes**; reuse no | success only | `--reuse` shape (`outcome:"reused"`) unpinned. |
| status | yes | **yes** | success only | — |
| submit (+ prepare) | yes | submit **yes**; prepare no | prepare refusal unpinned | Prepare `--json` refusal is plain text (see §2). |
| finish (+ close / cleanup) | yes | finish **yes** (success); close/cleanup no | `finish_cli.rs` has 5 explicit checks | A blocked `finish` exits **0** (see §2). `finish close --json` (`{"closed":id}`) and the `cleanup` refusal are unpinned. |
| gates (affected / run) | yes | no snapshot (`gates_cli.rs` has 46 ad hoc JSON asserts) | 7 explicit checks | No shape snapshot. |
| ship / publication (`advanced ship`, `push`) | yes | no (`ship_e2e.rs` uses ad hoc asserts) | thin | `ship plan --json` (21 keys) and `push` refusals unpinned. |
| cleanup / gc | yes | no (`gc_sweep_cli.rs`, `storage_cli.rs` use ad hoc asserts) | `gc.rs` (9), `closed_worktree_gc.rs` (11) | `gc plan --json` (33 keys) and `gc storage --json` (13 keys) have no shape snapshot. |
| hooks | yes | no | `hooks_e2e.rs` (1) | `hooks status --json` returns a **JSON array**, the only list-shaped top level seen. |

## 2. Live baseline (disposable repo, no gates/prepare/promote config)

`JSON?` means stdout parses as JSON. stderr is listed separately; a stderr warning never corrupted stdout.

| Surface | Command | Exit | JSON? | Top-level keys | Pinned by test? | Notes |
|---|---|---|---|---|---|---|
| status | `broker status --json` (before, mid, after submit) | 0 | yes | 30: `advice, advisory_delivery, agents, blockers, cleanup_retention, coordinated_operations, deferred_checks, integration_branch, integration_head, lease_liveness, lease_release_requests, leases, leases_refreshed, leases_refreshed_at_ms, main_ahead_upstream_commits, main_behind_upstream_commits, main_head, outstanding_advisories, outstanding_entry_exposures, overlap_pairs, overlaps, ownership_claims, phase_timings_ms, promoted_conflicts, publication_baseline_head, publication_baseline_ref, queue, queue_history, review_refusals, summary` | shape: yes | Once the broker writes `.aethyme/`, stderr always shows `warning: left the main checkout unchanged: tracked or non-ignored untracked changes are present…` (the fixture has no `.gitignore` for `.aethyme/`; the shape test adds one). |
| start | `broker start --task "t33 baseline" --short-name T33 --json` | 0 | yes | 37 incl. `id, worktree_path, worktree_placement, branch, start_base, preparation, repository_contract, guidance, tab_rename…` | shape: yes | Same stderr warning. The worktree root came from `environment_override`. |
| start --adopt | `broker start --adopt --task … --short-name Adopt --json` (in a `git worktree add` checkout) | 0 | yes | 37 incl. `outcome:"created"`, `integration_drift`, `default_branch` (no `start_base`/`worktree_placement`) | shape: yes | |
| start --reuse | `broker start --adopt --reuse --task … --json` | 0 | yes | same as adopt, `outcome:"reused"`, same id | **no** | |
| gates affected | `broker advanced gates affected --session 1 --json` (no `gates.toml`) | **1** | **no (empty)** | — | partial | stderr: `Error: no gates config at <worktree>/.aethyme/gates.toml (create it to define gates)`. **No JSON error envelope under `--json`.** |
| gates affected (worktree gone) | same, after session 1 was cleaned | **1** | no (empty) | — | no | stderr: `Error: <removed worktree path> is not inside a git repository`. This is the **same wording as #688**, for a path that really is gone. |
| submit prepare | `broker submit prepare --session 1 --json` (no `prepare.toml`) | **1** | no (empty) | — | no | stderr: `Error: preparation configuration at .aethyme/prepare.toml is invalid: repository declares no preparation steps`. A missing optional config is reported as *invalid*. |
| submit | `broker submit --session 1 --json` | 0 | yes | `conflict_details, conflicts, entry, gate_outcomes, gate_verification, graph_integrity, no_changes, promoted, promotion_suppressed, submission_plan, verified_against` | shape: yes | With no `[promote]` config, **`promoted:true`**: the default is auto, so the local `aethyme/integration` moved. `verified_against.source:"integration"`. No stderr. |
| push | `broker push --session 1 --json` (no upstream) | **3** | no (empty) | — | no | `Error: broker push refused: the main checkout's branch has no fetched upstream…` |
| push | `broker push --session 2 --json` (adopted non-`agent/*` branch) | **3** | no (empty) | — | no | `Error: broker push refused: "adopted" is not a session branch; broker push publishes only agent/* branches` |
| finish (clean, submitted) | `broker finish --session 1 --json` | 0 | yes | 20: `cleanup, cleanup_safe, closed, delivery, dirty_paths, last_gate, last_graph_integrity, latest_queue_entry_id, latest_queue_status, leases_held, next_commands, pending_work, recommended_next_action, representation, session_id, status, summary, unsubmitted_commits, warnings, worktree_path` | shape: yes | `status:"cleaned"`. Worktree **and** branch removed automatically (`cleanup.branch_removed:true`). `recommended_next_action:"aethyme broker advanced ship plan --entry 1"`. |
| finish (unsubmitted commit) | `broker finish --session 2 --json` | **0** | yes | same 20 | **no** | **`closed:false, status:"blocked"`, `unsubmitted_commits:1`, yet exit 0.** A caller that branches on exit code treats a refusal as success. |
| finish close | `broker finish close --session 2 --json` | 0 | yes | `closed` → `{"closed":2}` | no | Closes despite the unsubmitted commit, as documented ("does not check whether commits were submitted"). This is the path used to work around #688. |
| finish cleanup | `broker finish cleanup 2 --json` | **3** | no (empty) | — | no | `Error: refusing to clean session 2: 1 session commit(s) after the adoption boundary have never been accepted (use --force to discard)` |
| cleanup (bulk) | `broker finish cleanup --all-cleaned --json` | 0 | yes | `applied, failures, plan, removed_session_ids` | no | Read-only plan by default. |
| gc | `broker gc --json` | **1** | no | — | help only | `Error: gc requires \`plan\`, \`apply --confirm <sha256>\` or \`sweep\`` |
| gc plan | `broker gc plan --json` | 0 | yes | 33 incl. `digest, schema_version, worktrees, closed_worktrees, orphans, gate_caches, estimated_*_bytes/inodes, budget_verdict, reclaim_order` | no snapshot | |
| gc storage | `broker gc storage --json` | 0 | yes | 13 incl. `schema_version, digest, roots, candidates, storage_root` | no snapshot | |
| hooks | `broker advanced hooks --json` | **1** | no | — | help | `Error: hooks requires an action: install, uninstall, status, or snippet` |
| hooks status | `broker advanced hooks status --json` | 0 | yes (**array**) | list | no | |
| ship plan | `broker advanced ship plan --entry 1 --json` (bare origin, HEAD → missing `master`) | **1** | no | — | no | `Error: git ls-remote --symref origin HEAD failed: remote "origin" did not advertise a symbolic HEAD` |
| ship plan | same, after fixing the origin HEAD to `main` | 0 | yes | 21 incl. `plan_digest, proposed_push, publication_policy, freshness, local_main_sync_safe, target…` | no snapshot | |
| deploy | `deploy verify --repo <repo> --json` | **2** | no | — | help | stderr `aethyme deploy: unknown option --json`. **Usage printed to stdout.** There is no `deploy verify` (only `plan`/`execute`). |
| deploy plan | `deploy plan --repo <repo> --json` (origin HEAD broken) | 1 | no | — | no | Same `ls-remote --symref` error, prefixed `aethyme deploy:`. |
| deploy plan | same, origin fixed | 0 | yes | 27 incl. `schema_version, plan_digest, safe, blockers, planned_paths, generated_changes, preservation_refs, next_action…` | no snapshot | |
| repo | `repo enroll --help` | 0 | n/a | — | help (`repo.snap`) | There is no `repo enroll`. An unknown subcommand with `--help` prints the parent `repo` help and exits 0. Enrollment is `init` plus `deploy`. |
| help | `--help` for the 13 surfaces above | 0 | n/a | — | **yes** (help snapshots) | All help goes to stdout; stderr is empty. |

### Cross-cutting observations

1. **There is no JSON error envelope.** On every refusal, `--json` produces empty stdout, a plain `Error: …` line on stderr, and exit code 1, 2 or 3. A JSON consumer cannot tell a refusal class from a crash without parsing stderr text.
2. **Exit-code classes are informal.** The observed codes are 1 (usage, missing config, or a git failure), 2 (unknown option in `deploy`), 3 (a policy refusal in `push` or `finish cleanup`), and 0 for a *blocked* `finish`. None of this is documented as a contract, and only some of it is pinned.
3. **Repository default without config:** promote is auto, and submit moves the local `aethyme/integration`. The Aethyme repo itself is `verify-only` (`.aethyme/config.toml:27`), so a test fixture's default and the dogfood repo's behavior differ.
4. **Network and daemons:** every remote was a local file path, and no command errored on network. Comparing `aethyme` processes before and after (`pids-before.txt`/`pids-after.txt`) showed only processes from other repos (`aerie`, a `resources acquire` from another repo), none under this fixture. **No lingering daemon or process was started by this run.** The broker did write `.aethyme/` into the fixture repo, which made the main checkout "dirty" for its own warning.

## 3. #688 probe: "not inside a git repository" on `finish`

| Variant | Result |
|---|---|
| Worktree root containing a space (`…/Application Support/wt`, via `AETHYME_WORKTREE_ROOT`), `start` → `finish` | **exit 0, `status:"cleaned"`**. Captures: `30-start-space`, `31-finish-space`. |
| Same, plus `--agent "Claude Opus 5.5 <noreply@anthropic.com>"` and a long slug truncated to 40 characters like the real one | **exit 0, cleaned**. Captures: `32-*`, `33-*`. |
| `gates affected` on a session whose worktree was already removed | Exit 1 with the #688 wording, for a path that is actually missing. |

**The space-in-path hypothesis in #688 is refuted** for the start → finish path on a local volume.

**Code finding (likely mechanism):** `GitRepo::discover` (`aethyme-broker/src/git.rs:1165`) runs `git rev-parse --show-toplevel` through `run_git`, which applies `git_timeout()` (`git.rs:957`). It maps **every** error to `GitError::NotARepository`, via `.map_err(|_| …)`. That includes a spawn failure, a git error, a deadline kill (#219) and an `UntrustedOutput` refusal. So in #688, "not inside a git repository" can hide a timeout or a refusal. That fits the real environment:
- the worktree is on an external volume (`/Volumes/T7-repositories`);
- the repository is large, with 44 live sessions;
- `finish` applies a shared 10-second git deadline by default;
- the `finish close`/`cleanup` paths, which do not share that pre-close check budget, succeeded.

**Confirmed during synthesis (outside this slice's no-Aethyme-broker rule):** on the real repository, with its external-volume worktree root, a fresh session failed `finish` after 10.7 s with this error, and `finish --timeout 120` then cleaned it in 13.2 s. Recorded on #688. The remaining fix is to stop `discover` from swallowing the underlying `GitError`.
