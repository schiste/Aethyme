# Upgrading to Aethyme v0.7.20

Last Updated: 2026-09-16

v0.7.20 is a maintenance release. It corrects the tag-aware release
validation contract so version banners with build metadata are accepted for
both production binaries. Runtime behavior and broker storage compatibility
are unchanged from v0.7.19.

## What is new

### Release validation

The release contract now parses the tag token from the version detail section
before comparing it with the requested release tag. This keeps CI aligned with
the real aethyme --version and aethyme-engine-cli --version banner shape.

## Compatibility

- The broker database schema remains 39; no database migration is required.
- The engine protocol remains version 1.
- v0.7.20 is compatible with the v0.7.19 repository and broker database
  formats.
- The router and aethyme-engine-cli binaries remain a single release unit.

## Before upgrading

Record in-flight work and back up the broker database:

```bash
aethyme broker status
cp .aethyme/broker.db .aethyme/broker.db.pre-0.7.20
which -a aethyme aethyme-engine-cli
```

If the database is shared by several checkouts, make the backup from the
checkout that owns the machine-wide broker state.

## Install or update

Install both binaries from the same release:

```bash
brew update && brew upgrade aethyme
# or, from a checkout
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
```

## Migrate and verify

No broker database migration runs for v0.7.20. Verify the installed pair and
the repository after installation:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme certify
aethyme broker status
```

Both version banners should report 0.7.20 and the same release tag.

## Rollback

Because v0.7.20 does not change the broker schema, restore the previous binary
pair if a rollback is needed:

```bash
brew install schiste/tap/aethyme@0.7.19
# or install both 0.7.19 binaries from the checkout
```

Keep the v0.7.20 database and binaries together unless you intentionally
restore the v0.7.19 backup pair. If a later maintenance update has changed
machine-wide state, use the exact backup created before that update.

## Known issues

review run and review tick use read-only gh; an unauthenticated or unreadable
GitHub client causes pull requests to be skipped rather than making the sweep
fatal.

Review workspaces persist after a pull request closes and are not reclaimed by
broker cleanup or broker gc. A reviewer that changes directory away from its
workspace can also evade exact-cwd tab teardown; check for orphaned tabs
manually after routed reviews.

For repositories without an absolute or network origin, gate coordination
uses a path-derived identity. broker gates doctor reports this because the
identity changes when the checkout moves or is renamed.
