# T56 sensitive-content response qualification

Last Updated: 2026-10-15 UTC

This bundle records the finite Local T56 administrative response profile against source commit 425a0bc8e41031cea6b0ff8b5bc0e9cc80748b31 (tree 01f8a9f6a51818732cf6feb38125a34c81745d5b). The profile uses synthetic fixture content only.

The current-source broker module run passed 28 tests with no failures or ignored tests, and strict broker Clippy passed with warnings denied. Eight named mutation controls each failed the expected selected assertion and restored the complete source inventory byte-for-byte. M1's original machine status missed the test name because of its output parser; the original status is preserved unchanged with a separate reconciliation sidecar that confirms the exact failure. The old v23 binary returned the exact schema-too-new refusal for a disposable schema-24/minimum-24 state; the paired minimum-23 control removed that refusal as expected.

Historical compile/setup failures and earlier composite/module failures remain included as separate raw attempts. They are not rewritten or counted as passes for the final source.

## Limits

This is a finite, synthetic qualification, not an erasure guarantee. Unmanaged exports, raw Git objects, external backups, already-open v23 readers, same-user direct filesystem access, and copies outside the enumerated managed store remain outside the claim. See [ledger.json](ledger.json) for source binding, denominators, control outcomes, and the byte-count/SHA-256 inventory.

## Payload storage

The 212 original evidence payloads are stored as opaque, fixed-ordinal files under `payloads/artifacts/0001.raw` through `payloads/artifacts/0212.raw`. Their bytes and SHA-256 values are unchanged. The original nested path and media type remain in each `ledger.json` `files` entry as `logical_path` and `media_type`; `path` is the storage path relative to `payloads/`. Use those fields to identify and inspect a payload. Markdown payloads are preserved as opaque evidence artifacts, not maintained documentation. Their original Markdown media type and logical path remain recorded.

`payloads/artifacts/0213.raw` is a byte-exact copy of the pre-repackaging `ledger.json` (90,225 bytes; SHA-256 `ae892f97564e327bac1d170eb848de0cddefe96dc92104e358fa75774b7e69b1`). It is one provenance artifact in the 213-file storage inventory, in addition to the 212 original payloads (3,082,009 bytes). The current `ledger.json` is the machine-readable index for all 213 stored artifacts and records the original payload count separately.

The maximum repository-relative payload filename is now 115 characters. The flat storage layout keeps checkout paths portable on Windows without changing any evidence bytes.
