# Native job lifecycle qualification checkpoint

Recorded 2026-10-15 for native-job root release, retention, reopened status, and GC behavior.

## Source binding

The final restored native source is byte-identical at the three tested heads:

- e9a62f328db011d00def7a40ffccbe2da4c82967, tree 61a3bceeb876587819222cca6344574e04d77857: full native-gates suite, migration fixtures, snapshot checks, and strict Clippy.
- d52bc0a514fc5035fe143b97208a9e328f6a5d8e, tree 8840983777e2bc46dac1a2b5c748935955cb7be6: M1-M6 guard mutations and initial restored-source attempts.
- 3df43525dafede5f5c0119b9e8444ed9730dea9f, tree 04d4728e2f5a92c8e6928197d26fb743b2269ac8: final restored-source sanity, snapshot matcher, and discarded-results ratchet.

source-bindings.json records repository-relative file paths, bytes, SHA-256 values, commit relationships, and byte-for-byte restoration checks for all six mutation backups.

## Results

- Focused native_gates::tests: 15 passed, 0 failed, 0 ignored; exit 0.
- Seven migration fixture tests: 7 passed.
- Restored-source released-prepared-job sanity test: 1 passed.
- CLI help snapshot matcher and discarded-results ratchet both passed again after source restoration.
- Strict broker Clippy from the Rust workspace: exit 0.
- Six guard-removal mutations each produced the expected named assertion failure against mutated source. All six source files were restored byte-for-byte. M1's classification correction changed metadata only and did not rerun the command.

## Denominator and incomplete attempts

The 15 local native_gates commands scheduled 85 selected test slots: 51 returned ok, 8 returned assertion failures, 15 failed environmental preflight or setup, 1 did not return before SIGTERM, and 10 remain unresolved in two commands without numeric status. The two complete passing native commands are the 15-test suite and the restored-source one-test sanity check; 16 baseline native-gate passes receive credit. Mutation failures are expected guard confirmations, not baseline passes.

The 11:16:55Z off-tree cached-binary run has six completed ok lines and ends during the ninth test without final status; nine selected cases have no result. The 13:55:42Z restored-source run compiled the broker and began one selected test but has no result or numeric status. Its recorder metadata and raw log remain unchanged; a separate reconciliation records the observed lack of a remaining process and open log writer. Neither attempt has an inferred exit or outcome.

A stale cached mutant binary caused the first restored-source sanity attempt to fail. A later clean-source attempt failed setup because no host-state directory was configured. The successful sanity run used a private per-run host-state directory.

Historical full workspace CI at d524 recorded 4,557 cases: 4,548 passed, 9 failed, 31 skipped (exit 100). Those historical fixture, snapshot, and discarded-results findings remain in the raw CI log and are not presented as a current full-workspace result. The full workspace was not rerun in this package.

## Evidence

ledger.json contains historical and resumed attempt records, reconciliations, denominators, limits, and a top-level artifacts inventory. Every inventory path is relative to this folder and binds the copied payload's byte count and SHA-256. Original raw logs, metadata, status files, mutation backups, CI logs, and the initial ledger are preserved under evidence/; no raw output was normalized or rewritten.
