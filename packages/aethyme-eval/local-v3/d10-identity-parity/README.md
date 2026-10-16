# D10 TypeScript identity parity

Last Updated: 2026-10-16

This directory is a finite, arm's-length TypeScript qualification harness for the Local D10 identity contracts. It lives under `packages/aethyme-eval`; it is not a production TypeScript consumer and is not an Aethyme eval target. It uses Node built-ins and the reviewed T01 canonical JSON module, with no external assets or dependencies.

## Frozen inputs

All five vector package directories retain their supplied files byte-for-byte:

- `vectors/ordinal-supplement/` provides the corrected descriptor, enrolled and proposed FileId, source snapshot, and mixed Copy/Split/plain-new ordinal goldens. Its manifest SHA-256 is `69ac3502ed87470275922c27a5098c06d0272049b53f4141c354346f66ca978f`.
- `vectors/contribution-revision-bound/` provides the corrected bound-file ContributionRevision canonical bytes and RecordId. Its manifest SHA-256 is `ab85554b8b16543b2b356acaa29fcfbfcea8456292baa705d167daab5a14c9d1`.
- `vectors/explicit-rename/` provides a Rename transition and ContributionRevision golden. Its manifest SHA-256 is `6fb13ac0e523d65efcaece433353e3fe787aaf09eb90cebab35804bafcc287e0`.
- `vectors/prior-v1-invalid/` preserves the earlier invalid attempt, whose ContributionId suffix has 25 characters instead of 26.
- `vectors/prior-v2-partially-invalid/` preserves the v2 attempt. Its baseline and rationale-only immutable ContributionRevision vectors remain attributable; its proposed FileId and new-file revision expectations are unqualified because the FileId omitted the required `sha256:` component.

The runner rejects invalid ContributionIds and FileIds before identity hashing and does not credit either invalid attempt as passing evidence. It also distinguishes the generic Rust proposal token profile (1–128 ASCII letters, digits, `.`, `_`, `:`, or `-`) from the strict opaque ContributionId profile.

## Qualification coverage

The finite harness checks descriptor canonical bytes and immutable RecordId, enrolled/proposed FileIds, source snapshots, raw-path ordering, explicit Rename identity preservation, Copy/Split/plain-new ordinals, immutable ContributionRevision IDs, and no-hash validation refusals. It consumes both the generic `proposal-new-file` seed golden and the ContributionId-bound seed golden. Source paths match Rust `SourceSnapshot::new`: the 4096-byte cap, raw-byte component checks, protected `.git` aliases (case, HFS-ignorable, NTFS stream/trailing-dot/space and 8.3 forms), duplicate paths, and file/directory-prefix conflicts. Snapshot path sets are preflighted before the adapter hashes content or identities. ContributionRevision source changes are validated for relation shape, fields, canonical FileIds, strict raw-path order, and the Rust change-count cap before hashing. `source-pins.json` covers the exact reviewed Rust source set, TypeScript source set, and 20 frozen vector files, and binds the Rust contract source commit.

## Pending qualification work

This checkpoint is source-only and **UNRUN**. `source-pins.json` binds Rust contract source commit `324a493208517ba12f537c9e6a4ec7f608af322f` to its source bytes and pins the authored TypeScript sources and all 20 preserved fixture files. Node qualification, intended-failure controls, and their retained output have not been run or claimed in this checkpoint. No Cargo or native product runtime is used by the intended qualification.

The adapter covers descriptor canonical bytes and immutable RecordId, enrolled and proposed FileIds, source snapshots, source changes, Rename/Copy/Split/plain-new identity behavior, operation-order invariance, and four immutable ContributionRevision goldens. The generic-seed minimal new-file, corrected bound-file, and Rename expectations come from independent standard-library-only oracles. The preserved invalid v1/v2 packages remain provenance only.
