# Local resume attempt ledger

Session 1105 / PR #762; worktree `implement-the-actual-fx17-20-controlled`. This ledger retains the validation attempts and denominators; source contracts, frozen FX17 states/vectors, proxy, and score profile were not changed. No model calls or held-out reruns were made.

Shared Cargo target: `/Volumes/T7-repositories/Application Support/Aethyme/worktrees/aethyme-6f13a03e16f294c6/integrate-reviewed-local-bare-git-servic/target/local-resume/cargo`. Cargo/native runs use `--locked -j 4`, `RUSTC_WRAPPER=''`, clean PATH, foreground `caffeinate -i`, and the locally inspected Docker image ID `sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251`.

## Attempts already run

| Artifact prefix | Source | Result | Denominator / note |
|---|---|---|---|
| `fx17-harness-compile-no-run` | Before 1108 compile fix | exit 101 | Compile failed (`E0599`, `CommitOid` lacks `Display`); no tests ran. Retained full log and numeric status. |
| `fx17-harness-compile-no-run-fix1` | After 1108 compile fix | exit 0 | Compile-only passed; no tests ran. Retained full log and numeric status. |
| `ordinary-controller-later-edit` | Before 1108 borrow fix | exit 101 | Compile failed (`E0382`, moved `native`); no tests ran. Retained full log and numeric status. |
| `ordinary-controller-later-edit-fix1` | After 1108 borrow fix | exit 0 | 1 passed, 0 failed, 0 ignored, 1795 filtered out. |
| `ordinary-controller-known-publication` | After compile fix | exit 0 | 1 passed, 0 failed, 0 ignored, 1795 filtered out. |
| first `local_controller::` selection | Before corrected absolute log destination | shell exit 1 after logger path failure | Test output reported 19 passed, 0 failed, 3 ignored, 1774 filtered out, 21.91s, but output/status were not durably captured; uncredited. |
| `local-controller-selection-captured` | Before final fixture edits | exit 0 | 19 passed, 0 failed, 3 ignored, 1774 filtered out, 23.99s. |
| `local-target-selection` | Before final fixture edits | exit 0 | 19 passed, 0 failed, 0 ignored, 1777 filtered out, 5.87s. |
| `actual-unknown-publication` | `e55abfc23324d4eec72697a7fea5aebb474f0bcb`, tree `913d2a91cf7c284c1bdcbc0012557f605aafdd9c` | exit 101 | 0 passed, 1 failed, 0 ignored, 1795 filtered out, 4.55s. Failed before service dispatch: bare receiver `hooks/` directory did not exist. Evidence `/var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/.tmpn7Ac8q`; source host `.tmpUDE86p`; repository `.tmpme5gnL`. Full output/status/JSON retained. |
| `actual-unknown-publication-repair1` | `dafca2995069420fd75fb82a2496a05ffe93ef5a`, tree `52836ed2a724d83d08c8b10d9d471789f2af1d1b` | exit 101 | 0 passed, 1 failed, 0 ignored, 1795 filtered out, 50.59s. Actual push exited 0 and advanced the receiver ref, but hook identity record stayed empty; investigation verified `/usr/bin/cat` is absent while `/bin/cat` exists. Kept as a failed attempt. All roots/logs/JSON/status preserved. |

Attempt 2 retained roots: evidence `/var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/.tmpF2whr3`; source host `.tmpW34Zlq`; repository `.tmplAFgol`; receiver `.tmpweASev`. Receiver candidate ref after the push was `f7defde31bbafea32a7b9605e779195279daefc5`.

| `actual-unknown-publication-attempt3` | `b16af023c07f22817fb8bb11abe464e916507850`, tree `7e857b8e50e28f4b62acb8ed71a5070215061c95` | exit 0 | 1 passed, 0 failed, 0 ignored, 1795 filtered out; 23.53s test / 61.975s wall. Report confirms run state `unknown`, source observation `unobserved`, no receipt, candidate ref advanced, receiver hook start/finish records present with both processes exited, replay refused, and zero completions/qualifications/acceptance receipts. Full log, numeric status, metadata, and report copy retained. |

Attempt 3 retained roots: evidence `/var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/.tmprAe1Yv`; source host `.tmpif4Yv`; repository `.tmpT5ijmz`; receiver `.tmpP239DH`. Copied report SHA-256: `f27346d8bd50273cb386d7b3050797bf244b5a5bec242fb797d779a30b0c7e8c`.

| `actual-fx17-twenty-stage-attempt1` | `b16af023c07f22817fb8bb11abe464e916507850`, tree `7e857b8e50e28f4b62acb8ed71a5070215061c95` | exit 101 | 0 passed, 1 failed, 0 ignored, 1795 filtered out; 7.37s test / 8.230s wall. Report status `failed`, only C01 staged, service outcome `held`, ticks 1, no completion; `sealed_task_executions=0`, `model_exports=0`. Runtime detail: `infrastructure deferred: resource directory must be private`. Full log/status/source-bound JSON/report copy retained. |

C01 permission finding: report host root `/var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/.tmp3Ds4QO` was mode `0755`, uid `501`, gid `20`; `ResourcePolicy.temporary_root` and `host_state.parent()` both point there. The unchanged production `private_directory` guard rejects any `mode & 0077 != 0`. Its `local-execution-journal` child was never created. In the successful unknown-control run, host root `.tmprAe1Yv` was `0700` and `local-execution-journal` was `0700`; that harness explicitly sets its host root to `0700`. The FX17 correction sets only its fixture-owned host root to `0700`, records before/after mode and uid/gid in the report, and leaves the production guard unchanged. Failed FX17 root copy: `target/local-resume/retained-roots/fx17-attempt1-host` (and source, harness, and remote roots). Inventory: `target/local-resume/retained-roots-inventory.json`.

## Current source and upcoming exact runs

The reviewed receiver cleanup Clippy fix was merged into session 1105 by merge commit `9b31b827cbe12da5527b8c272ec9576719996e21`. Commit `b16af023c07f` contains only the test harness corrections: `/bin/cat` in the disposable hook and `CONTROLLED_STATES.iter().enumerate().skip(1)` with the bound state used for the same indexed assertions. Both files pass rustfmt and diff checks; commit-message lint passed. Broker push succeeded.

The exact unknown-outcome test passed after the fixture repair. FX17 attempt 1 failed at C01 due the fixture tempdir mode finding above. A harness-only permission correction is committed and pushed before one corrected retry of `local_controller::drive::tests::fx17::actual_fx17_twenty_controlled_changes_preserve_all_65_vectors`; the production privacy guard, frozen states, vectors, proxy, and score profile remain unchanged. Preserve every attempt and root.


## FX17 attempt 3 and complete schedule accounting

Cargo/Docker/native work is complete for this session; the exclusive slot was released after attempt 3. No further runtime or Cargo commands were run during evidence archiving.

| Exact FX17 run | Exit / test result | Scheduled stage positions | Executed stages | Accepted | Held | Unrun |
|---|---|---:|---:|---:|---:|---:|
| `actual-fx17-twenty-stage-attempt1` | exit 101; 0 passed, 1 failed | 20 | 1 (C01) | 0 | 1 (private-directory fixture setup) | 19 |
| `actual-fx17-twenty-stage-attempt2-private-root-fix` | exit 101; 0 passed, 1 failed | 20 | 2 (C01, C02) | 1 (C01) | 1 (C02 explicit-reconciliation hold) | 18 |
| `actual-fx17-twenty-stage-attempt3` | exit 0; 1 passed, 0 failed | 20 | 20 (C01–C20) | 20 | 0 | 0 |
| **All three actual runs** |  | **60** | **23** | **21** | **2** | **37** |

The first failed run stopped at C01 with `infrastructure deferred: resource directory must be private`; its fixture root was 0755. The harness-only correction set that root to 0700 and left the production privacy guard unchanged. Attempt 2 accepted C01, then held C02 with `publication dispatch Failed; explicit reconciliation required`; it remains an unsuccessful run and contributes no twenty-stage completion. Attempt 3 passed the named test with all twenty changes accepted and verified. The separate launcher path error is not a fourth run: exit 2, `cargo_started=false`, and zero tests or scheduled stage positions. Its raw files remain `actual-fx17-twenty-stage-launcher-path-error.{log,json,status}`.

Attempt 3 used source HEAD `c45d4096f9d0238b800ccadbeb07f9b93ae30a3e`, tree `874654af26ac7fd9bbaa9bedbc7a04fa8a834e40`, clean status, and Docker image `sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251`. Exact selected test: `local_controller::drive::tests::fx17::actual_fx17_twenty_controlled_changes_preserve_all_65_vectors`; Cargo exited 0 after 462.53 seconds test time / 564.769 seconds wall time (`1 passed; 0 failed; 0 ignored; 1798 filtered out`). Each accepted stage records exactly 65 expected verifier vectors and one verifier evidence receipt; the successful run therefore covers 1,300 expected vector comparisons. All twenty accepted candidates have an actual remote parent equal to the immediately previous accepted commit, candidate and accepted commits/trees match, one completion and one fresh currently-authorized passing native gate, and the target lease/baseline follows the previous accepted commit. The current remote `refs/heads/accepted` and `HEAD` both point to C20 `2d8d7e4ff2912ebd55bd0eaa12e3b6cb93d9ac2f`.

The 29 files in the pre-run frozen-input manifest match the after-run paths, sizes, and SHA-256 values exactly. The session source worktree remains at the same c45 HEAD/tree with clean status. In the disposable original source checkout, `main` HEAD remained `f44e567d8c5a357d9b72341ab3a60c56a9126aec` and tracked status is clean; Aethyme runtime `.aethyme/` paths are untracked. Its refs did change as test contribution refs were created, which is recorded in the verification summary; this is not described as a no-ref-write run. The run reports 0 model exports and 0 sealed task executions. No reliability or generality claim follows from this finite controlled test.

Attempt 3 raw paths:

- `target/local-resume/actual-fx17-twenty-stage-attempt3.log`
- `target/local-resume/actual-fx17-twenty-stage-attempt3.status`
- `target/local-resume/actual-fx17-twenty-stage-attempt3.json` (full argv, environment, source binding, image pin, duration and report hash)
- `target/local-resume/actual-fx17-twenty-stage-attempt3-report.json` (full per-stage report and parent chain)
- `target/local-resume/actual-fx17-twenty-stage-attempt3-frozen-inputs-before.json`
- `target/local-resume/actual-fx17-twenty-stage-attempt3-frozen-inputs-after.json`
- `target/local-resume/actual-fx17-twenty-stage-attempt3-verification-summary.json` (per-stage parent, source, candidate, tree, receipt, gate, and vector-count checks)
- `target/local-resume/actual-fx17-twenty-stage-attempt3-roots-inventory.json` (byte/mode/owner inventory for host, source, harness and disposable remote archives)

Attempt 1 and attempt 2 logs, statuses, metadata, reports and their root inventories remain alongside attempt 3. Original unknown-outcome test roots remain in `target/local-resume/retained-roots/unknown{1,2,3}-*`; FX17 roots for attempts 1–3 remain in `target/local-resume/retained-roots/fx17-attempt{1,2,3}-*`. The three attempt-3 runtime roots were copied without changing files, symlinks or modes. Inventory hashes are in the attempt-3 roots inventory and evidence inventory.
