# Upgrading to Aethyme v0.7.19

Last Updated: 2026-09-16

v0.7.19 is the broker reliability and graph-indexing follow-up release. It
gives abandoned sessions a terminal path, makes cleanup budget-aware, keeps
review facts and waivers explicit, and strengthens gate and GitHub-write
protection. It also fixes JavaScript and TypeScript graph indexing for both
quoted fetch-worker registrations and avoids false middleware facts from bare
mentions.

This release migrates the broker database from schema 35 to 39. Read
**Rollback** before upgrading.

## What is new

### Lifecycle and cleanup

Sessions with no evidence of a working agent can now be abandoned after
`session_abandoned_after_hours` (72 by default), releasing leases and making
their worktrees cleanup candidates. `broker status` and `broker gc plan` also
report unclaimed directories under managed worktree roots.

Reclamation is now driven by `retained_bytes_budget`. Plans order candidates by
the active policy, apply operations until their deadline, and report whether
the budget can actually be cleared. Routine commands use recorded sizes and
stay cheap; `gc plan`, `gc apply`, and `cleanup --apply` perform the full audit
needed to authorize removal.

### Review routing

Review routing can waive one dimension at one head with a recorded reason,
declare freshness per dimension, and fall back to a local reviewer after a
classified provider refusal. Completion facts now remain separate from request
facts, and an abbreviated head is accepted when it identifies exactly one
record.

### Protection and graph indexing

Exact-tree verification slots are placed outside the repository when host
state allows it. Gate coordination is anchored to the main checkout, GitHub
label validation happens before create, and failed issue or pull-request
creates are reconciled from collection evidence before the broker reports an
outcome.

The graph indexer now recognizes both `addEventListener(\"fetch\", ...)` and
`addEventListener('fetch', ...)`. JavaScript and TypeScript middleware facts
require a structural `.use(` registration, so comments and strings containing
the word “middleware” no longer create false positives.

## Compatibility

The broker database migrates from schema 35 to 39:

- v36 records the base commit needed for per-dimension review freshness.
- v37 adds the explicit `waived` review state.
- v38 separates review request facts from completion facts and records the
  completion verdict and reviewer identity.
- v39 adds release tracking for closed-session pins and an `expired` state for
  old publication exposures.

The migration runs automatically when a v0.7.19 binary first opens the
database, and it is one-way. The broker database is machine-wide: upgrade
every installed copy of both `aethyme` and `aethyme-engine-cli`, including
copies found earlier on `PATH`, before using the migrated database.

## Before upgrading

Record in-flight work and back up the broker database:

```bash
aethyme broker status
cp .aethyme/broker.db .aethyme/broker.db.pre-0.7.19
which -a aethyme aethyme-engine-cli
```

If the database is shared by several checkouts, make the backup from the
checkout that owns the machine-wide broker state.

## Install or update

The router and engine sibling are one release unit. Install both together:

```bash
brew update && brew upgrade aethyme
# or, from a checkout
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
```

## Migrate and verify

Migration happens on first open. Verify the pair and the repository after
installation:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme certify
aethyme broker status
```

Review routing remains inert unless the repository opts into its review tables.
Use `aethyme broker review plan --pr <number>` to inspect a decision without
performing a review or writing to GitHub.

## Rollback

**Constrained.** A v0.7.18 binary cannot open a database at schema 39, and
there is no down-migration. Restore the pre-upgrade database together with the
older binary pair:

```bash
aethyme broker status
cp .aethyme/broker.db.pre-0.7.19 .aethyme/broker.db
brew install schiste/tap/aethyme@0.7.18
# or install both 0.7.18 binaries from the checkout
```

Rows and operations recorded after the migration are lost when the backup is
restored. If no backup exists, keep every binary on v0.7.19 rather than
mixing an older binary with the schema-39 database.

## Known issues

`review run` and `review tick` use read-only `gh`; an unauthenticated or
unreadable GitHub client causes pull requests to be skipped rather than making
the sweep fatal.

Review workspaces persist after a pull request closes and are not reclaimed by
`broker cleanup` or `broker gc`. A reviewer that changes directory away from
its workspace can also evade exact-cwd tab teardown; check for orphaned tabs
manually after routed reviews.

For repositories without an absolute or network `origin`, gate coordination
uses a path-derived identity. `broker gates doctor` reports this because the
identity changes when the checkout moves or is renamed.
