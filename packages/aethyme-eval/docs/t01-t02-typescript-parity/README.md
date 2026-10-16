# T01/T02 Rust and TypeScript parity qualification

Last Updated: 2026-10-16

`qualification.tar.gz` is the frozen offline qualification evidence bundle. It contains exactly 346 plain regular-file members: the 345 files listed in the original package manifest and that manifest itself. The new archive has no directory records, PAX records, links, duplicate or traversal paths, AppleDouble sidecars, or unlisted members. The archived bytes were compared member-by-member against the frozen package; all 346 match. No Node executable is bundled.

This is a finite contract-parity qualification against existing frozen vectors. It is not a production TypeScript consumer, production runtime path, or Aethyme evaluation target. No golden fixture or Rust contract oracle was modified to fit the TypeScript implementation. No external assets or dependencies were added.

## Qualification result

- Baseline commit: `a4b478126edc5f03b84dd8bebc3cedeab7c5947b` (tree `daacf68f45a6ab2bce5ece68542bc73ddadfec2c`).
- Restored positive: Node v26.8.2, 256 MiB heap limit, 60-second deadline; exit 0 with 880 assertions and empty stderr. All frozen source pins, HEAD/tree, and clean status matched before and after.
- Five reviewed mutant controls each failed at the exact intended first assertion, exited 1, and restored the pristine source hash with clean status and unchanged HEAD/tree.
- The complete original attempt ledger is retained, including the first uncredited Chinese token-vector behavioral failure before its generic fix and the two failed patch preflight attempts.

## Bounded runtime qualification (2026-10-16)

The additive finite qualification now has retained runtime evidence at [`ledger/runtime-qualification-2026-10-16/README.md`](ledger/runtime-qualification-2026-10-16/README.md). The package contains both the first unqualified attempt and the later clean PASS, including full process outputs, exact status records, source backups/restores, reviewed drivers, and per-file digests.

The passing run used the allowlisted T01 profile `local-state26-e6fb95` at Rust reference `e6fb95f65f3bf91d46c1ab76136486f492529ebb`: 880 T01/T02 assertions and 224 D10 assertions passed both before and after the six mutation controls, with 35 source/fixture pins verified. Two CLI refusal controls and all six intended mutation controls passed with exact expected outcomes and clean byte-for-byte restoration. Attempt 001 remains explicitly uncredited for T01/T02 because it stopped at the stale default canonical JSON source pin before assertions; its D10 result is retained separately.

This is finite offline fixture qualification only. It does not claim a production TypeScript consumer, general interoperability, or an Aethyme evaluation result.

## Packaging correction

An earlier packaging proposal was rejected because its macOS tar output carried 377 AppleDouble sidecars, producing 723 regular archive entries. Its proposal-manifest SHA-256 was `e59e1bcae2036376aa3caf88082b267e1ed719600a240e07dd6e9618927a683f` and archive SHA-256 was `4764c824ae7f9e6817ab6a31d87d362890af64fa522bb40baab3202340b94592`. That rejected archive remains unchanged locally, with its full member inventory and review note in the new proposal ledger. The underlying frozen qualification package did not change. This accepted-candidate archive contains only the 346 original regular files and is independently verified against the frozen source bytes.

See `ledger/portable-package-manifest.sha256` for the original package digest list, `ledger/frozen-inputs.sha256` for source and fixture pins, and `ledger/runtime-provenance.txt` for Node details. `ledger/qualification.sha256` records the archive hash; `ledger/proposal.sha256` hashes the proposal files other than itself.
