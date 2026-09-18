# Upgrading to Aethyme v0.7.22

Last Updated: 2026-09-18

v0.7.22 is the first release since v0.7.19 that migrates the broker database.
Read [Compatibility](#compatibility) before installing it on a machine with
live sessions: the migration is one-way and older binaries cannot open a
migrated database.

## What is new

### A build failure is no longer recorded as a test failure

A gate whose command never compiled is recorded as `build_failure` rather than
`test_failure`. The two need different repairs — one fixes a symbol, the other
fixes behaviour — and the record now says which. Detection is cargo-scoped and
sits behind the resource and environment checks, so a build stopped by a full
disk is still recorded as contention and cannot condemn the change that would
free the space.

### Failing gate logs survive a later pass

Gate logs are named after `(gate, tree, worker)`, so a re-run on an unchanged
tree used to overwrite the log of the failure it contradicted — precisely the
flake evidence worth keeping. A failing log now takes a unique name, including
on the host-resource and managed-cache failure paths.

### A delivery to a target that never returns stops being retried

Deferral still absorbs a target that is briefly away, but a delivery is
dead-lettered after a bounded number of attempts instead of re-queuing forever.

### Storage inventory for enrolled primary checkouts

`aethyme broker storage` reports regenerable artifacts, Git cleanliness and
tracked-file protection for enrolled primary checkouts. Removal is deliberately
narrower than reporting: only the checkout you invoke it in can lose bytes, and
only once its build directories have stopped changing.

### Broker sessions carry their Chau7 context

A session records the repository, tab and AI provider it belongs to, so a
session can be identified by where it is running rather than only by its
worktree path. This is the change behind migration 41.

### Bounded Imports graph-impact mode

`explore` and the semantic gate report can traverse bounded incoming Imports
edges, answering where a Calls traversal has nothing to say.

## Compatibility

- **The broker database moves from schema 39 to 41**, applying two migrations:
  - **40** rebuilds `gate_results` to admit the new `build_failure` class.
    Every row and index is carried over.
  - **41** adds `repository_name`, `tab_name` and `ai_provider` to `sessions`.
    All three are nullable and existing rows are left untouched.
- **The migration runs on first open.** The first v0.7.22 binary to touch a
  broker database migrates it, and that binary may be a gate's build rather
  than an installed one.
- **v0.7.21 binaries then refuse that database** with
  `broker db schema version 41 is newer than this binary supports (39)`. Plugin
  hooks fail the same way, and a hook that swallows the exit code goes quiet
  rather than erroring. A binary built between the two migrations reports the
  intermediate number; the remedy is the same, which is to install the pair
  from this release rather than to touch the database.
- The engine protocol remains version 1.
- Repository layout and `.aethyme/` configuration are unchanged from v0.7.21.

Unlike the v0.7.20 to v0.7.21 window, live sessions **are** affected here.

## Before upgrading

1. Let running gates finish. A gate interrupted mid-migration is not dangerous,
   but its session will need the new binary before it can continue.
2. Note which machines share this repository's broker database. Upgrading one
   agent upgrades the database for all of them.
3. Record the version you are on, so rollback is a decision rather than a guess:

   ```bash
   aethyme --version
   ```

## Install or update

Install **both** binaries, and always pass `--locked`:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

`--locked` is not optional. Without it `cargo install` re-resolves the
dependency graph instead of using the workspace lockfile, and the resolution it
picks fails to compile `ra-ap-rustc_lexer` with `error[E0080]` on rustc 1.96.
The same crate builds correctly from the workspace, which makes the failure
look like a toolchain problem rather than a missing flag.

Install the pair together. A router and engine from different revisions is not
a supported combination.

## Migrate and verify

The migration needs no command; it runs when the broker next opens its
database. Confirm the result:

```bash
aethyme --version
aethyme broker status
aethyme certify
```

`broker status` returning normally means the database is on schema 41 and the
installed binary speaks it. To see that gate history survived the rebuild:

```bash
aethyme broker metrics --json
```

## Rollback

**Rollback is restricted in this release.** Reinstalling v0.7.21 gives you a
binary that cannot open a database v0.7.22 has already migrated — the usual
"check out the previous tag and reinstall" leaves you with a broker that
refuses to start.

If you have not yet run any v0.7.22 binary against a database, rollback is
ordinary:

```bash
git checkout v0.7.21
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

If the database has already been migrated, you must either stay on v0.7.22 or
restore the database from a backup taken before the upgrade. There is no
downgrade migration. Committed work is never at risk either way: sessions,
branches and worktrees are Git objects, and only the broker's own coordination
state lives in the database.

## Known issues

- **`cargo install` without `--locked` fails to build.** See
  [Install or update](#install-or-update). This affects v0.7.21 equally; it is
  documented here because a migration release makes a failed install harder to
  back out of.
- **A gate can migrate the shared database from work that is not merged.** The
  `cross-process-contract` gate runs a tree-built binary against the real
  broker database, so a session whose worktree carries a newer schema migrates
  the database for the whole machine before that schema is reviewed, merged, or
  even committed. Tracked in
  [#232](https://github.com/schiste/Aethyme/issues/232).
- **`aethyme broker storage plan` is slow on large monorepos.** The primary
  checkout lane sizes build directories rather than honouring the recorded-size
  budget the other lanes use. Tracked in
  [#235](https://github.com/schiste/Aethyme/issues/235).
- **`aethyme broker resources reap` can report holders it does not name.**
  Tracked in [#234](https://github.com/schiste/Aethyme/issues/234).
