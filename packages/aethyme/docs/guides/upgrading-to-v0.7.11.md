# Upgrading to Aethyme v0.7.11

Last Updated: 2026-09-06

v0.7.11 adds two reconciliation tools for work that drifts away from the broker,
and lets a repository stop its push hooks from holding the coordination lock. No
schema changes.

## Compatibility

| Contract | v0.7.11 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.10 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

### Push hooks need not hold the coordination lock

The lock orders remote mutations, but `git push` runs `pre-push` inside its own
process, so the lock was held across that hook. Where the hook is a legitimately
long full suite, every other session's coordinated operation on that repository
waits for it, and a fleet serialises on whoever is pushing the largest change.

```toml
# .aethyme/config.toml
[coordination]
hooks_outside_lock = true
```

With it enabled, a coordinated push first runs `git push --dry-run`, executing
`pre-push` against exactly the commits the real push will send, before queueing.
A hook that refuses stops the operation there without ever taking the lock. The
broker then acquires the lock, re-plans, and refuses if any destination or
proposed commit changed while it waited -- the hook's verification would no
longer describe what is being sent. Only then does it push, with `--no-verify`.

**This is off by default and is a real trade.** `--no-verify` skips every
`pre-push` protection, not only a slow gate. A repository whose hook also does
secret scanning or signing checks is choosing to run those in the dry run alone.
Enable it when the hook's cost is the constraint and its checks are
deterministic over the same commits.

### Reconciling a local default branch

```bash
aethyme broker main reconcile plan
aethyme broker main reconcile apply --session <id> --confirm <plan-sha256>
```

Classifies every commit the local default branch carries that integration does
not. Representation is decided by content, not ancestry: the broker promotes by
replaying into a squashed commit, so work that already landed is usually not an
ancestor of anything on integration and `git cherry` misses it too. A commit
counts as represented when integration holds its content for every path it
touched.

The apply refuses unless every local-only commit is represented and no tracked
path is dirty, and creates `aethyme/preserve/<branch>-<sha>` before moving
anything. Unrepresented work is named and never moved over.

### Renamed targets are reported before a replay fails

`broker adopt` now reports paths a session still targets that a later promotion
renamed, naming the new path and the promoted entry responsible. A genuine
deletion stays a deletion.

### A lost submit response no longer strands its promotion

A submit can advance integration while its queue row is still simulating. If the
row was superseded in that window the commit stayed on integration with nothing
claiming it, and `ship plan` refused it as unrecorded. A retry now claims it.

## Before upgrading

Nothing special. Broker storage, repository deployment, engine protocol and
graph cache schemas are unchanged from v0.7.10, so no migration runs and mixed
v0.7.10/v0.7.11 installations can share a repository during a rollout.

## Install or update

```bash
brew update
brew upgrade aethyme
```

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

```bash
aethyme --version
aethyme-engine-cli --version
aethyme deploy --repo .
aethyme enhance verify --repo .
aethyme broker quick-test
aethyme broker main reconcile plan
```

Both version commands must report `0.7.11`. `main reconcile plan` should report
that the default branch carries nothing integration does not, on a repository
that is level -- which also confirms the new command is present.

## Rollback

Restore both v0.7.10 binaries together through the original installation
manager. No schema migration has run, so no database restore is required and
repository deployment files are compatible in both directions. If you enabled
`hooks_outside_lock`, remove the setting: older binaries ignore it and will run
hooks inside the lock as before. Never combine binaries from different releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- Coordinated operations still serialize per repository. `hooks_outside_lock`
  removes the local gate from the critical section, but two operations on
  unrelated refs of the same repository still contend.
- `main reconcile` classifies commits as represented or not; the four-way
  classification with resolution templates, and replaying unrepresented work
  automatically, are not implemented. Unrepresented commits are named and refuse
  the apply.
- Schema migration, when a release carries one, is implicit: the first newer
  binary to open the database migrates it, with no confirmation to other
  installations. This release carries none.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`; only
  capacity units are reclaimed automatically.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
