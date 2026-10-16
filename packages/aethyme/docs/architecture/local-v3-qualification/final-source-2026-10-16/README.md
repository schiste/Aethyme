# Final combined Local source: bounded qualification

This package records actual commands on clean source
`2f2d2c1f27eaddded1875316de59c0935591804c`, tree
`86124ec0cdb795b1b3d546d51d443566a9538fdf`. The archive retains full outputs,
numeric statuses, command metadata, drivers, original attempts and source checks.
The [archive index](archive-index.json) binds every regular member by size and
SHA-256. Packaging does not rerun checks or extend qualification claims. The collection
script checks workspace command linkage; build/floor manifest-to-payload
consistency is checked separately during independent archive review.

## Completed checks

All five final local commands exited zero and left the exact source unchanged:

| Command | Recorded result |
|---|---|
| `packages/aethyme/scripts/test-like-ci.sh --full` | 4,718 tests passed; 38 skipped; JUnit retained |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked -j 4 --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --locked -j 4 --workspace --examples` | Passed |
| `cargo test --locked -j 4 --workspace --doc` | Passed |

The full check ran for about 13 minutes 39 seconds. Cargo commands were serialized
with four jobs, an empty Rust compiler wrapper and a shared-target lock. These
are local command executions, not a count of unique tests across other ledgers.
The separate GitHub OSS run 38857121712 passed 4,715 tests with 32 skipped;
Windows run 38857121764 also passed. The [OSS status receipt](ci-38857121712.json)
and [Windows status receipt](ci-38857121764.json) retain the exact source and
job URLs. Their differing test profiles keep their own denominators. The successful full OSS output is retained in the archive.

The evidence checkout also passed the single `internal_links_valid` test with
two existing, hash-pinned test binaries; the [command record](documentation-link-checks.json)
retains both outputs. This checks current documentation paths without recompiling
or extending the product-runtime qualification claim.

## Binary and schema-floor compatibility

The archive retains the initial build and a fresh build after the workspace
checks. Both built the CLI and engine from the exact clean source above and
produced the same binary digests: CLI `85133fccd6e9e8f7ac366bf7b18ac93bf2676734e264eea76ba88f1c3b4f8120`
and engine `e068305cf6fc88cc0c7257ccdac5d743f16d5838129ef10198469e6c9db4e971`.
The two build receipts retain their distinct timestamps and provenance.

On a synthetic local Playground with file-only bare origin and no gates, the
schema-floor attempt 002 passed: a real capture established collaboration schema
26 and floor 26, and the pinned old binary supporting schema 23 refused both
JSON and text status with typed `schema_too_new` responses. Five complete root
inventories were byte-identical before, between and after the old commands.
Source and Playground Git state at the command boundary remained unchanged.
CLI binary identities are pinned by the build/refusal receipts; no post-refusal
binary rehash is claimed.

Attempt 001 remains a failure: its final Git comparison used the seed state from
before normal broker-created files. The later driver compares the actual old
command boundary and retains the seed identity separately. Neither failed bytes
nor the old binary were overwritten. This is one pinned old-binary compatibility
profile, not a general downgrade claim.

## Remaining qualification

The independent T64 two-contributor public-CLI rehearsal remains unrun. The
original setup failed its prebootstrap empty-directory condition; that setup is
preserved. A separate location-only recovery proposal requires review and user
authorization, followed by real human Terminal trust before contributor/runtime
execution. None of these checks reruns consumed E1, FX08 or FX17 provider trials.
The six FX17 provider failures still yield zero candidates. This package does
not establish general autonomous reliability, universal structural composition,
or production TypeScript interoperability. All PRs remain draft and unmerged.
