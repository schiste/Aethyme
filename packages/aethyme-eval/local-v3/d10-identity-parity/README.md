# D10 TypeScript identity parity (provisional source checkpoint)

Last Updated: 2026-10-16

This directory is a finite, arm's-length TypeScript qualification harness for the Local D10 identity contracts. It lives under `packages/aethyme-eval`; it is not a production TypeScript consumer and is not an Aethyme eval target. It uses Node built-ins and the reviewed T01 canonical JSON module, with no external assets or dependencies.

## Frozen inputs

The three vector directories each retain the four independently supplied files byte-for-byte:

- `vectors/ordinal-supplement/` is the corrected frozen expected-data supplement. Its manifest SHA-256 is `69ac3502ed87470275922c27a5098c06d0272049b53f4141c354346f66ca978f`.
- `vectors/prior-v1-invalid/` preserves the earlier invalid attempt, whose ContributionId suffix has 25 characters instead of 26.
- `vectors/prior-v2-partially-invalid/` preserves the v2 attempt. Its baseline and rationale-only immutable ContributionRevision vectors remain attributable; its proposed FileId and new-file revision expectations are unqualified because the FileId omitted the required `sha256:` component.

The runner checks the invalid v1 ContributionId and v2 FileId are rejected before identity hashing. It does not credit either invalid attempt as a passing vector.

## Pending qualification work

This checkpoint is source-only and **UNRUN**. `d10-identity-parity.mts` expects `source-pins.json`, which will be added only after the contract author freezes the final Rust source commit and the independent reviewer freezes a corrected bound-file ContributionRevision golden. Node qualification, intended-failure controls, and their retained output have not been run or claimed in this checkpoint. No Cargo or native product runtime is used by the intended qualification.

The adapter covers descriptor canonical bytes and immutable RecordId, enrolled and proposed FileIds, source snapshots, source changes, Copy/Split/plain-new ordinals, operation-order invariance, and the two valid existing immutable ContributionRevision goldens. The independent corrected ordinal fixture provides the bound-file source-change and FileId goldens; the requested corrected bound-file ContributionRevision golden remains pending.
