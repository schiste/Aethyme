# Upgrading to Aethyme v0.7.24

Last Updated: 2026-09-21

A maintenance release. The broker database is unchanged, so this upgrade is
reversible and safe to install alongside live sessions.

## What is new

### The autonomous sweep reclaims the shared preparation cache

The sweep ran on its daily cadence and reclaimed nothing. Its cadence stamp
updated while the last recorded sweep event was days old, because the shared
preparation cache was reachable only from `aethyme broker storage` — a command
someone had to type. On the machine this was measured, a cache reclaimed by
hand grew back from 6.9 GB to 15 GB in three days.

Cache entries are named after a content hash of the inputs that produced them,
so an entry no checkout on the host would compute will never be read again.
That is why removal needs no operator review, unlike a worktree, which holds
work. Liveness is evaluated across every enrolled checkout, so a sibling
repository's entries are not removed by a sweep running here.

### The sweep works harder when the disk is the problem

Effort now follows free space, measured against the same headroom a gate
requires before it will start. Below that line the budget widens and the
interval shortens to an hour; above it, nothing changes. Free space that
cannot be determined is treated as routine rather than urgent, so an
unreadable filesystem does not put a machine into a permanent hurry.

### Status reports the disk, not just the budget

`aethyme broker status` now shows host free space beside retained bytes, and
states that the retained-bytes budget is per repository while the volume is
shared. Many repositories can each sit inside their own budget while the disk
is nearly full and every broker reports healthy.

### Graph enrollment and committed artifacts

Graph enrollment, the refresh path, and the committed graph artifacts are
included in this release.

## Compatibility

- The broker database schema remains **41**. No migration runs.
- The engine protocol remains version 1.
- Repository layout and `.aethyme/` configuration are unchanged from v0.7.23.
- Installing alongside live sessions is safe, because no migration means no
  binary is locked out of a database another has upgraded.

## Before upgrading

1. Note the version you are on, so a rollback is a decision rather than a
   guess:

   ```bash
   aethyme --version
   ```

2. Nothing else is required. There is no database migration to schedule around
   and no state to back up.

## Install or update

Install **both** binaries, and always pass `--locked`:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

`--locked` is required, not stylistic. Without it `cargo install` re-resolves
the dependency graph instead of using the workspace lockfile, and the
resolution it picks fails to compile `ra-ap-rustc_lexer` with `error[E0080]`
on rustc 1.96. The same crate builds correctly from the workspace, which makes
the failure look like a toolchain fault rather than a missing flag.

Install the pair together. A router and engine from different revisions is not
a supported combination.

## Migrate and verify

There is no migration. Confirm the install:

```bash
aethyme --version
aethyme broker status
aethyme certify
```

`broker status` now includes a host free-space line. If it reports less free
space than a gate needs to start, the sweep has already widened its budget and
shortened its interval; no action is required beyond letting it run.

## Rollback

Rollback is unrestricted in this release, because no database migration runs.
Reinstall the previous pair:

```bash
git checkout v0.7.23
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

A v0.7.23 binary opens a database a v0.7.24 binary has used, because the schema
is identical. This is unlike the v0.7.21 to v0.7.22 window, where a migration
made rollback restricted.

## Known issues

- **`broker submit` can refuse on graph integrity while `graph refresh` reports
  nothing to refresh.** Tracked in
  [#254](https://github.com/schiste/Aethyme/issues/254). Changes to indexed
  broker source may need to land by pull request until it is resolved.
- **Worktree roots without a marker cannot be attributed to a repository**, so
  no cleanup path can reclaim them. The sweep improvements in this release do
  not reach them.
- **`aethyme broker resources reap` can report holders it does not name.**
  Tracked in [#234](https://github.com/schiste/Aethyme/issues/234).
