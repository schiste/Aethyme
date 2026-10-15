# T56 sensitive-content response qualification

This bundle records the finite Local T56 administrative response profile against source commit 425a0bc8e41031cea6b0ff8b5bc0e9cc80748b31 (tree 01f8a9f6a51818732cf6feb38125a34c81745d5b). The profile uses synthetic fixture content only.

The current-source broker module run passed 28 tests with no failures or ignored tests, and strict broker Clippy passed with warnings denied. Eight named mutation controls each failed the expected selected assertion and restored the complete source inventory byte-for-byte. M1's original machine status missed the test name because of its output parser; the original status is preserved unchanged with a separate reconciliation sidecar that confirms the exact failure. The old v23 binary returned the exact schema-too-new refusal for a disposable schema-24/minimum-24 state; the paired minimum-23 control removed that refusal as expected.

Historical compile/setup failures and earlier composite/module failures remain included as separate raw attempts. They are not rewritten or counted as passes for the final source.

## Limits

This is a finite, synthetic qualification, not an erasure guarantee. Unmanaged exports, raw Git objects, external backups, already-open v23 readers, same-user direct filesystem access, and copies outside the enumerated managed store remain outside the claim. See ledger.json for source binding, denominators, control outcomes, and a byte-count/SHA-256 inventory of copied payloads.

## Files

payloads/ contains the byte-preserved original command logs, status records, source bindings, mutation patches, old-binary probe records, frozen plan, and prior attempt history. ledger.json inventories each payload by relative path, byte count, and SHA-256.
