# FX10 actual sustained-arrival evidence

This folder preserves the frozen FX10 trial, its earlier failed harness attempt, the heartbeat-race regression proof, selected controller/target checks, and strict Clippy output. The corrected monolithic actual trial and selected ordinary checks ran at source commit `1f7b226660dbef8f036e5e10c1c301fe7a38d50a`; the test source SHA-256 is `5b46a4281c56518c361643563e602d084f820b12a7ebec37614489a1b183ed32`. The 19-input public fixture manifest SHA-256 is `01aa466182a9054f5f4f0bd1f71fa1a3f2869ea731cee0f69ed0098a2b679ae1`. No fixture bytes or oracle expectations changed during those runs. The earlier actual attempt and heartbeat regression checks have separate provenance below; the regression preflights did not capture enough working-tree state to establish a clean full-tree run at `1f7b2266`.

## Corrected monolithic actual trial

`current-monolithic/` is the reviewed rerun. Its report has `status=completed`, no failures, zero model requests, and zero sealed task executions. The exact invocation, commit, source and manifest hashes, clean PATH, Cargo target directory, Docker CLI, and pinned image are in `current-monolithic/preflight.txt`:

```text
caffeinate -i cargo test --locked -j4 -p aethyme-broker actual_fx10_sustained_arrivals_serve_old_group_before_later_features -- --ignored --nocapture
```

The run used Docker image `sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251`. The selected test passed (1 passed, 0 failed, 1801 filtered; 67.91 seconds); the Cargo command exit status was 0.

The positive schedule selected controller-created older group A-C and kept it `Verifying` with zero completions before release. D/E/F were persisted at sequences 4/5/6 while the real host gate child was in its release-controlled heartbeat wait loop, so their arrivals overlapped live heartbeats. At dispatch the ready group age was 19,309 ms and its oldest member age was 19,319 ms. The gate child ran 254 ms and recorded 16 heartbeat updates; an independent typed capacity contender was held for 251 ms, and the actual native gate admission waited 288 ms. After the release signal ended the heartbeat wait loop and the child completed, the test ran its separate integrity commands (`test -f app.sh && git diff --check HEAD^ HEAD`). The run then recorded two accepted completions, all six final obligations, and zero blocked waves, invalidated verification attempts/time, unknown runs, or automatic uncertain retries. These figures come from `current-monolithic/report.json`.

The separate finite external-ref control advanced a disposable bare Git ref once by same-tree fast-forward, from `ac503d7f90ca8eaf4b7794b66c9b2895e62a2b9c` to `38a6421dda5142e955f0f01ce49955724d4ded0e` (tree `8ef66dddb617a4cbe12743e63878adbafb595b3b` on both sides). A distinct capacity contender waited 255 ms; the real gate admission waited 292 ms. The gate child ran 253 ms before and 254 ms after the ref advance, with 16 heartbeat updates in each window. The verifier passed 11,075 ms after the ref advance. Publication was dispatched once and its transport failed; the service correctly stayed `Held` for explicit reconciliation, the final remote remained at the competing tip, private completions were zero, and there were no replays or unknown runs. This is one bounded same-tree drift control, not evidence about repeated drift reliability.

The heartbeat is a process-level witness for the host gate child during its bounded wait loop. It overlapped D/E/F arrivals; the later integrity commands ran after release and child completion. This does not claim a heartbeat inside Docker or that integrity commands overlapped the live heartbeat window.

## Preserved earlier failed actual attempt

`prior-failed-monolithic/` is intentionally retained as a separate denominator. At prior source commit `143d995220334d426934b330c54b9ba2f9a329db` (source SHA `4a051002ec8258542699f9ef490ed22910c64bb02da36f1c27d70232e87e82e9`), the full command exited 101 after the test failed because the harness checked for the first gate-child heartbeat too early. Its positive schedule had two completions and all six obligations. In its separate negative schedule one legitimate completion occurred before the harness stopped; there were zero controlled ref advances, so that attempt did not test drift handling. Do not combine its outcome with the corrected run or describe it as a product drift-guard failure.

## Harness regression and selected checks

`heartbeat-regression-checks/` retains the normal regression and temporary premature-error mutation. Their preflights record Git HEAD `143d995220334d426934b330c54b9ba2f9a329db`; the normal check's source-file SHA-256 was `5b46a4281c56518c361643563e602d084f820b12a7ebec37614489a1b183ed32`, while the committed file at that HEAD was `4a051002ec8258542699f9ef490ed22910c64bb02da36f1c27d70232e87e82e9`. The mutation check's source-file SHA-256 was `84426abe9ff80a34f99bdf7ec37dc20eed768b14d5962303599ad56d49a1d1d8`. The normal named regression passed (exit 0); the mutation failed the same test (exit 101). The mutation was removed and the source file restored to its pre-mutation SHA. These preflights did not capture full working-tree status or diff, so they establish the recorded HEAD and file contents only; they do not establish a clean full-tree run at `1f7b2266`. The folder also records two uncredited setup failures: the first wrapper launch had shell exit 1 and no captured Cargo exit; the shared target lock attempt had Cargo exit 101 before any test ran. Neither is counted as a test result.

`ordinary-checks/` contains full logs, preflights, and numeric statuses for:

- `local_controller`: 24 passed, 4 ignored.
- `local_target`: 19 passed.
- `gate_policy`: 5 passed.
- `broker_discarded_results`: 3 passed.
- `strict-broker-clippy`: `cargo clippy --locked -j4 -p aethyme-broker --all-targets -- -D warnings`, exit 0.

The full output for the monolithic runs and checks is retained even where repository ignore rules match `*.log`. `INVENTORY.tsv` records byte lengths and SHA-256 hashes for each payload file and this README. `SHA256SUMS.txt` checks those files plus the inventory itself; the two inventory files are excluded from their own contents.
