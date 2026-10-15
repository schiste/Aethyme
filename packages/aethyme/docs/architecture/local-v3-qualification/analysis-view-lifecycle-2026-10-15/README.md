# Analysis-view lifecycle evidence

Last Updated: 2026-10-15

## Scope

This folder records focused validation for per-view lifecycle coordination, fail-closed hold scanning, unique temporary hold publication, and their interaction with view GC and context readers. It is not a full-workspace CI run or an evaluation result.

No model calls, native execution trials, or held-out evaluations were run for this task.

## Source identity

The final selections ran against clean commit `2a61bcf69759662a0014b6edb54d133163c97338`, tree `a4ec0df4b43ba81ce690a97160a816b4837ac68c`. The tested `analysis_view.rs` SHA-256 was `67ec472f77148caa2cfb322baa3dcc312a7f34aae824c499bb1708d68f60a310`.

The five guard-removal checks ran against clean parent commit `d819d1758a341ee90fb7f6640865859295befef0`, tree `ddbf3de65d28a80321146d07462236d00248c710`. Their temporary source mutations were individually hashed and restored byte-for-byte. Independent review confirmed that commit `2a61bcf` changes only temporary hold cleanup error reporting; the guarded lifecycle paths and tests are unchanged, so those mutation results still apply to the final code. Their original `d819` attribution is retained in the metadata.

Every Cargo log has a matching numeric status and source-bound recorder metadata. The raw recorder files are copied byte-for-byte into `runs/`; `artifact-inventory.json` lists each file's source path, byte count, SHA-256, command, source identity, and exit status.

## Final selections on `2a61bcf`

| Selection | Result |
| --- | --- |
| `cargo test --locked -j4 -p aethyme-broker --lib analysis_view:: -- --nocapture` | 21 passed |
| `cargo test --locked -j4 -p aethyme-broker --lib collaboration_gc:: -- --nocapture` | 22 passed, 1 ignored |
| `cargo test --locked -j4 -p aethyme-broker --lib collaboration_context:: -- --nocapture` | 38 passed |
| `cargo test --locked -j4 -p aethyme-broker --test context_selection_vectors -- --nocapture` | 2 passed |
| `cargo test --locked -j4 -p aethyme-testkit --test broker_discarded_results` | 3 passed |
| `cargo test --locked -j4 -p aethyme-testkit --test deprecated_spelling_callers` | 4 passed |
| `cargo test --locked -j4 -p aethyme-testkit --test docs_hygiene` | 14 passed |
| `cargo clippy --locked -j4 -p aethyme-broker --lib --tests -- -D warnings` | Passed, exit 0 |

The environment and full argv, including `caffeinate -i`, clean `PATH`, empty `RUSTC_WRAPPER`, and shared Cargo target, are recorded in each run's metadata JSON.

## Guard-removal checks

Each mutation compiled and made its named test fail with Cargo exit 101. The source file was restored exactly after each attempt; the original and mutated source hashes, binary diff hash, named test result, and restoration status are in the matching `.mutation.json` audit sidecar.

| Removed behavior | Detector |
| --- | --- |
| Treat non-directory holds path as empty | `unknown_holds_keep_views_during_plan_and_apply` |
| Reuse `.hold.tmp` for concurrent publication | `hold_publication_preserves_another_temporary_file` |
| Skip retirement lifecycle lock | `hold_and_retire_wait_for_the_same_lifecycle_lock` |
| Skip reader lifecycle lock | `opening_a_view_pins_before_retirement_can_finish` |
| Skip final publication lifecycle lock | `build_publication_waits_for_retirement_lock` |

The first audit wrapper selected a panic-detail line containing the test name instead of the harness result line, so its initial derived verdict was false and its wrapper exit was 1. That history remains in the unknown-holds mutation sidecar. The unchanged Cargo log records the intended test as `FAILED`, the test binary had compiled successfully, and Cargo exited 101; the corrected sidecar records that verified result without changing the raw log or recorder metadata.

## Preserved ratchet failure

The first discarded-results ratchet run on `d819d17` failed at 144 production `let _ =` sites against the baseline of 143. The added site was `HoldTemporary::drop` ignoring removal errors. Commit `2a61bcf` keeps cleanup best-effort for a missing temp file and reports other errors through `warn_unrecorded`. The post-fix discarded-results ratchet passes 3/3. Both the initial failing log/status/metadata and the passing rerun are included.

## Artifact map

`runs/` contains the pre-cleanup focused baselines, all five guard mutations, the initial ratchet failure, and every final selection. `artifact-inventory.json` enumerates the copied metadata, status, log, and mutation-audit artifacts with byte-level SHA-256 hashes.
