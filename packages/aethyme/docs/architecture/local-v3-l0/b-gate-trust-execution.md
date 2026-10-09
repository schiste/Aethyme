# L0 slice B: gate trust and execution supervision

Last Updated: 2026-10-09 (L0 audit, #651)

Baseline `5e8daf712e26c63f1d4082e9a617f556cf995de2`. All paths are relative to `packages/aethyme/rust/crates/aethyme-broker/` unless noted. The slice itself was read-only; the tests named here were then run as part of the audit's targeted test step (see the README, §7).

This slice records the **reusable mechanisms** in full. Gaps that bear on the trustworthiness of verification evidence are stated only as **missing properties** and are tracked at design level in #671, #672 and #673. Following `SECURITY.md`, exploitable detail is not published here.

Status key:
- `confirmed`: the code does this, as read.
- `changed`: the plan or I3 assumption differs from the code.
- `unverified`: plausible from the code, but not proven by a test or by execution.

## 1. Config loading and trust

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| Session-path gates (`run_gates*`, `gates affected`, `run_all_gates`) load `.aethyme/gates.toml` from the live session worktree files, dirty edits included, and not from a commit. | confirmed | `src/broker/gates.rs` `gate_selection_inputs_with_timings` :998–1003 and `gate_inputs_with_integrity` :1033–1038, then `load_and_sync_gates_from` :1080 → `gates::load_gates` (`src/gates.rs:205`). | `tests/gates_e2e.rs::selection_run_cache_and_fail_fast` | This path is advisory; the trust digest below covers it. |
| Submit judges the candidate by the gate policy **committed at the base**, never by the merged tree. A policy change applies to later submissions and is recorded as `MERGE_POLICY_DEFERRED`. | confirmed | `src/merge.rs:1035–1080` (`load_and_sync_gates_at_commit(&base)`); trust check `require_trusted_policy_at_commit(&base)` at `merge.rs:652` and `:816`; `ship.rs:2007–2010` behaves the same. | `tests/merge_e2e.rs::base_gate_policy_judges_a_submission_that_weakens_or_deletes_its_gates` | Reusable for L6 #672 (policy pinned to the authority's base). |
| Exact-ref scope evaluation reads the config from a committed head. | confirmed | `src/gates.rs:386` `load_gates_at_commit`; callers `cli/gates.rs:627`, `:686`, `gate_doctor.rs:565`. | `tests/gates_cli.rs` | The scope manifest is content-free (command excluded, `execution_definition_hash` kept), schema v3, self-digest checked: `gates.rs:40`, `:454`, `:475`. |
| Trust-on-first-use: nothing repository-defined runs until a human approves the policy digest. The record is host state keyed by the git common dir, and approval refuses without a TTY. | confirmed | `src/broker/gate_trust.rs` header :1–19; `require_trusted`; `record_path`; `read_record` fails closed on corruption. | `tests/gate_trust_cli.rs::{a_new_clone_refuses_gates_and_submit_until_trusted, a_landed_gate_change_refuses_the_next_submit_until_trusted_again, a_repository_with_gate_history_is_grandfathered, trust_refuses_without_a_terminal, the_pre_commit_hook_refuses_an_untrusted_policy}` | Scope of what the approval covers: see §8, #672. |
| Grandfathering auto-trusts the current checkout and integration-tip policies when the repository's broker DB has gate history and no trust record exists. | confirmed | `require_trusted`, `current_policy_digests` in `gate_trust.rs`. | `a_repository_with_gate_history_is_grandfathered` | One-time migration; deleting the trust record re-opens it for a repository with history. Note for D18/L9. |

## 2. Selection

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| Selection over-selects: an empty trigger list matches every path; a bad glob fails validation; gates are sorted by cost, then name. | confirmed | `gates.rs` module doc :23–25; `Gate::matches` :196; `select_gates` :628. | `tests/gate_policy.rs::gate_policy_changes_select_a_triggered_gate`; `src/gates.rs` unit tests | |
| Submit computes the changed set between the verify base and the exact merge commit. | confirmed | `merge.rs:1003–1005` `gate_scope_changed_between(verify_base, &merge_commit)`. | `tests/merge_e2e.rs` | |
| Graph integrity and semantic advice are recorded but never block a gate or promotion (#280). | confirmed | `merge.rs:1043–1056`, :1082. | — | Matches the plan principle that analysis is advisory. |

## 3. Which checkout runs the command

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| Submit gates run in a disposable detached worktree materialized at the exact merge commit, inside a pooled verification slot outside the repository. | confirmed | `merge.rs:1020–1035`; `src/verification.rs:274` `materialize` (`worktree_add_detached`); `cleanup` :283, also on `Drop`. | `src/verification.rs` unit tests `a_slot_is_placed_outside_the_repository_it_verifies`, `slot_reuses_one_path_and_removes_stale_contents`, `a_full_pool_waits_for_a_released_slot` | Reusable for #670 as the materialization primitive. No execution-snapshot identity exists beyond the commit; checkout-time transformations (filters, LFS, submodules) are not recorded. |
| Session-path gates run in the live session worktree, with ignored files present. | confirmed | `broker/gates.rs` `run_gates_with_policy` :763. | `tests/gates_e2e.rs` | Interaction with result reuse: §5, #689. |
| Pre-push runs on a clean checkout of the pushed HEAD. | unverified | `docs/guides/host-resource-coordination.md:286`; `gates.rs:810` `plan_pre_push`. | — | |

## 4. Process supervision

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| The command runs through `sh -c` with cwd at the checkout, stdin null, stdout and stderr to a per-run log, in a new process group. | confirmed | `gates.rs:2955–2975` (`run_gate_command`). | `tests/gates_e2e.rs::native_timeout_terminates_the_gate_process_group_and_is_typed` | |
| The broker sets per-run variables (worker id, `AETHYME_TEST_DB_SUFFIX`, an isolated broker DB and scope, owner paths, run labels, resource values, managed cache dir) and sanitizes `PATH` against wrapper shims. | confirmed | `gates.rs:2955–2990`; `git.rs:468` `sanitized_subprocess_path`. | `src/git.rs` tests `a_wrapper_that_forwards_faithfully_is_accepted`, `a_wrapper_that_decorates_porcelain_is_caught` | Environment and filesystem confinement: §8, #671. |
| The resource-lease ownership token is not exported to the child; only lease id, generation and allocated values are. | confirmed | `resources.rs:204` `HostResourceGrant::environment`. | `tests/resources_cli.rs` | |
| Timeout: `killpg(SIGTERM)`, 1 s grace, then `killpg(SIGKILL)`; a timeout is typed and never cached. Load-admitted gates scale the deadline by load per CPU, 1×–4×. | confirmed | `gates.rs:3110–3150`; `gate_admission.rs` (`MAX_TIMEOUT_LOAD_MULTIPLIER = 4.0`). | `tests/gates_e2e.rs::{native_timeout_terminates_the_gate_process_group_and_is_typed, environment_and_timeout_failures_are_classified_and_not_cached}`; `gate_admission.rs::load_admitted_gate_timeouts_scale_proportionally_and_stay_bounded` | A gate without `timeout_seconds` is unbounded (historical), flagged by the gate doctor (`gates.rs:76–78`). Every gate in this repository sets one. |
| A pidfile records pgid, pid, start time, tree and run id; if it cannot be written the child is killed rather than left untracked. Superseded runs are cancelled by tree. | confirmed | `gates.rs:3000–3025`; `write_gate_pidfile` :1204; `cancel_obsolete_runs` :1228; `signal_target` :1117 guards against PID reuse. | `tests/gates_e2e.rs::resubmit_with_new_tree_cancels_obsolete_slow_run`; `tests/store.rs::gate_result_cache_ignores_cancelled_and_error_runs` | Reusable for #671. Lease-renewal failure SIGTERMs the group before authority expires (`gates.rs:3060–3085`). |
| Processes that leave the group are labelled (`AETHYME_GATE_LABELS`, `AETHYME_GATE_RUN_ID`) and reclaimed by `doctor apply` with a reviewed digest. | confirmed (design) | `src/gate_debris.rs` (#287). | — | Cooperative only. |
| Isolated broker DB per gate run (#232/#361); a run that changes a protected schema version is invalidated and never passed. | confirmed | `gates.rs:2930–2950`, `:3175–3185`; `protected_schema_versions` :3259; see also the comment at `gates.rs:2839`. | `tests/gate_database_isolation.rs::{a_dirty_tree_migration_in_a_gate_lands_only_in_its_disposable_database, concurrent_gate_workers_receive_distinct_databases}` | Isolation is a schema-version guard, not an integrity guarantee for stored results: §8, #672. |

## 5. Results, subject binding and cache

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| Subject = `working_tree_hash()`: a private index from `read-tree HEAD` + `add -A .` + `write-tree`; tracked and non-ignored untracked files, ignored files excluded. | confirmed | `src/git.rs:2705`. | `tests/gates_e2e.rs::selection_run_cache_and_fail_fast` | A Git tree id, not a build-input identity (#670). |
| Cache key = `(gate_name, tree_hash, definition_hash)`, reused across sessions. Only `pass` and `fail/test_failure` are reusable; `cache=false` gates always rerun. | confirmed | `src/store/queue.rs:75`; `gates.rs:2004–2050`; `cached_failure_class` :2641. | `tests/gates_e2e.rs::{cache_false_gate_reruns_for_the_same_tree, cargo_target_dir_infra_errors_are_not_cached_failures, environment_and_timeout_failures_are_classified_and_not_cached}`; `tests/store.rs::gate_result_cache_ignores_cancelled_and_error_runs` | No test asserts that a definition change misses the cache: #689. |
| A cache hit reports `GateEnvironment::default()`; the reused verdict carries no host or environment provenance in its outcome payload. | confirmed | `gates.rs:~2055`. | `tests/gates_e2e.rs::executed_gates_record_machine_load_and_free_disk_and_cache_hits_do_not` | #689. |
| A submit is deferred, not rejected, only when every non-pass is a broker-observed host fault. | confirmed | `merge.rs:1101–1115`; `GateRunOutcome::is_host_fault` `gates.rs:710`; classifiers :2524–2760. | `tests/simulated_free_space_cli.rs::a_gate_that_fails_on_a_healthy_disk_is_still_a_verdict` | Robustness of the classification against candidate output: §8, #672. |

## 6. Host resources, admission and cleanup

| Claim | Status | Evidence | Enforcing tests | Notes / follow-up |
|---|---|---|---|---|
| Disk floor: refused before spawn below 8 GiB or 100k free inodes. | confirmed | `gates.rs:2853–2895`; `disk_headroom.rs:22`, `:28`. | `tests/simulated_free_space_cli.rs` (3 tests) | |
| Load admission: a gate with `cost >= 3` or `max_load_per_cpu` waits while load per CPU exceeds its threshold (default 3.0), bounded by `resource_wait_seconds`, then defers without starting. Unreadable load means run. | confirmed | `gate_admission.rs` :1–40. | `gate_admission.rs` unit tests (10); `tests/gate_policy.rs::cross_process_contract_uses_load_admission_despite_its_low_cost` | A wait, not a reservation: per-target budgets are new work (#668). |
| Resource bundles are atomic leases with a TTL, renewed at TTL/3; lost renewal kills the group; cleanup failure quarantines the generation. | confirmed | `gates.rs:1694` `acquire_gate_resources`, renewal :3050–3085; `host-resource-coordination.md:35–42`, `:169–174`. | `tests/resources_cli.rs` | Closest existing primitive to #668; per resource key, not per target budget. |
| `AETHYME_TEST_DB_SUFFIX` = worker id `s<session>-<gate>` (or `p<pid>-<gate>`); `broker exec` uses `s<id>-exec`. | confirmed | `gates.rs:1664` `gate_worker_id`, :2963; `broker/leases.rs:1482`. | `tests/gate_database_isolation.rs::concurrent_gate_workers_receive_distinct_databases` | Unverified whether two pooled verifications of the same session and gate can share a suffix; follow-up test. |
| A managed gate cache (`CARGO_TARGET_DIR`, key `rust-workspace-v3`, 12 GiB) is held under an exclusive host lease and rotated inside it. | confirmed | `gates.rs:1817–1955`; `.aethyme/gates.toml`. | `src/gates.rs` unit tests `managed_cache_rotates_only_its_broker_owned_directory`, `missing_managed_cache_is_measured_as_empty_before_creation`; `tests/gate_cache_inventory.rs` | Isolation between candidates: §8, #673. |
| Scratch cleanup: verification slot removed on completion and on `Drop`; isolated DB is a `TempDir`; pidfile removed after wait; failed-gate logs preserved. | confirmed | `verification.rs:283–296`; `gates.rs:2843`, :3160; `preserve_failed_gate_log` :3329. | verification unit tests | A crash between materialize and cleanup is recovered by the next `materialize` (`verification.rs:279`). |

## 7. This repository's gates

| Gate | Invokes |
|---|---|
| `pytest-aethyme-eval` | Creates `.venv` if missing, installs `pytest`, runs `pytest packages/aethyme-eval/tests`. Unpinned install: #690. |
| `fast-guards`, `cargo-test`, `script-contract`, `workflow-contract`, `gate-policy` | `cargo test`/`nextest --locked` over the workspace or named tests. |
| `cross-process-contract` (`cache=false`) | `cargo run … aethyme broker check-contract` against the merge base with `aethyme/integration`. |

## 8. Reuse vs. missing for L6 (#670–#673) and L5-BUDGET (#668)

| Need | Reuse | Missing property (tracked at design level) |
|---|---|---|
| Exact materialization (#670) | `ExactTreeVerificationSlot` | An execution-snapshot identity covering recipe, toolchain, environment, dependencies, filters, submodules and generated inputs. |
| Supervisor (#671) | Process group, pidfile with start time, SIGTERM/SIGKILL timeout, lease-loss kill, obsolete-run cancel, debris labels | Confinement of the gate child: its environment, its filesystem reach, and memory/CPU/process limits beyond wall clock. |
| Non-forgeable evidence (#672) | Base-pinned policy; host-state trust record; isolated broker DB | Evidence that is independent of candidate-controlled code, storage and output end to end, bound to an execution profile, with expiry and revocation. Today only the *policy* is base-pinned. |
| Resources and cache (#673) | Disk floor, load admission, atomic leases, managed-cache rotation inside a lease | Cache reuse bound to a full execution profile, and build-cache isolation between candidates. |
| Budget reservations (#668) | Host resource leases (key-scoped, TTL, generation fencing) | Per-target budgets with atomic reserve/release across workers and retry lineage. |

## 9. Doc vs. runtime

- None found in `host-resource-coordination.md` for the rows checked.
- The `gates.rs` module doc :1–3 says "tree-hash result cache"; the key also includes the gate name and `definition_hash`. Incomplete, not contradictory.
