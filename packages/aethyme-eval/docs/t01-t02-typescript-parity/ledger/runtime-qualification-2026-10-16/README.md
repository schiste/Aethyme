# Local T01/T02 and D10 runtime receipts

Last Updated: 2026-10-16

This dated evidence package preserves both the stale-pin attempt and the later passing offline qualification. The complete raw stdout/stderr, numeric status files, unique mutation backups, pin backups, and reviewed driver files are in `attempts-and-controls.tar.gz`. `archive-index.json` lists and hashes every regular member, and `archive-files.sha256` provides per-member SHA-256 values. The tar archive contains 76 regular-file members; no fixtures or oracle bytes were edited.

## Source checkpoint and runtime

- Branch/commit/tree: `agent/add-finite-d10-typescript-parity-for-pro` / `f02096084b3b17703ff8dc3e07d0fdc84795ddb2` / `e261027b49b71015305017a473584322073b98c5`.
- T01 profile: `local-state26-e6fb95`, Rust reference `e6fb95f65f3bf91d46c1ab76136486f492529ebb`. It replaces only the frozen `canonical_json.rs` source pin with `6bbeea89e7d5850651687382255619b2ea9d30538341ebbba51a53a0605a6a35`; the legacy default profile and golden vectors remain unchanged.
- Node: v26.8.2 at `/opt/homebrew/Cellar/node/26.8.2/bin/node`, binary SHA-256 `3e401401d90e7f0367b7737a613801242c9a6b2966fb50c2e48eb1c45d701c6a`. Each Node process used a 256 MiB old-space cap and a 60-second deadline.
- Scope: offline and eval-side. No network/install, Cargo/native, provider, held-out, or fixture/oracle mutation occurred. This is not a production TypeScript consumer or an Aethyme eval result.

## Attempt 001: retained, unqualified

At commit `d7b34d65435222bdf3a60e2a15e038364a6ec6f2`, the no-argument T01 runner stopped before assertions because the frozen canonical JSON pin expected the earlier hash. Exact refusal: `Error: frozen source drift at packages/aethyme/rust/crates/aethyme-contracts/src/experimental_v0/canonical_json.rs: expected 90070c56f613bb94564072c56076daf2428f2888f0b71e34a526b718c55b52f3, found 6bbeea89e7d5850651687382255619b2ea9d30538341ebbba51a53a0605a6a35`. D10 passed 224 assertions with 35 verified pins. T01/T02 receives no assertion or parity credit from this attempt; no mutation controls ran. Its full logs remain in the archive.

## Attempt 002: PASS

Initial and restored positives each passed T01/T02 with 880 assertions and D10 with 224 assertions across 35 verified Rust, TypeScript, and fixture files. The explicit source profile and reference commit are asserted in the runner report.

Both T01 argument controls passed with exact parser refusals before frozen-source reads. All six D10 mutation controls failed at their named intended assertions, had exit status 1 without timeout or preflight/compile errors, and restored their unique pristine source and pin backups byte-for-byte. Every restore was clean; the final HEAD/tree matched the qualification checkpoint. The detailed assertion names, statuses, stdout/stderr hashes, backup hashes, and restored hashes are retained in `summary.json` and the raw archive.

## Retained artifacts

- Driver v3 used for attempt 001: SHA-256 `0f882b260aae632094fe193c5ff2768e76edb3f8f1654fe804fd6931d5086494`.
- Reviewed driver v4 used for attempt 002: SHA-256 `b5110523da6b3faa333a75e30141898e500d43070ea8502f780e1723001cfb4b`; proposal manifest SHA-256 `2d52971538f36518c9b4eb265c1652fc7a97ccecc787fc9287c8452955220f03`.
- Attempt 002 qualification-results SHA-256 `6c053b8aaebcbd8695aec38cbedff6313df950188b940c0e102f396a250a6fb7`; start manifest SHA-256 `3ef58f04db9cd307a4f6382e0c5f2f9c9549fe3a7560c357d69cfe4430d87237`.
- Archive SHA-256 `6acde8924274b1d609db326b59b4d207a80cfceb9254ee54f4f735cbaff2ba24`.
