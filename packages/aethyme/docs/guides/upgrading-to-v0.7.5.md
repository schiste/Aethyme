# Upgrading to Aethyme v0.7.5

Last Updated: 2026-09-04

v0.7.5 makes graph support something a repository opts into deliberately.
Enrollment writes reviewable policy and an exact engine pin without generating
anything, and a separate `materialize` step builds only the ignored
worktree-local store from what was committed. Nothing about the graph changes
for a repository that does not enroll.

## Compatibility

| Contract | v0.7.5 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.4 |
| GC plan schema | 2 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- Graph support is disabled by default. `aethyme deploy --repo . --with-graph`
  enrolls a repository, writing policy and the exact engine pin; use
  `--graph-repository owner/name` only when no canonical `origin` resolves.
- Enrollment generates nothing. Review and commit the policy and pin first,
  then run `aethyme graph materialize --repo .` to build the store.
- `materialize` validates committed policy, pin, and fragments against the
  exact `HEAD` and atomically builds only the ignored worktree-local redb
  store. It is a no-op when that store is already current.
- `--with-graph` is refused together with `--local-only`: untracked policy
  cannot authorize shared committed fragments.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. v0.7.5 changes no broker storage,
repository deployment, or engine protocol contract, so no database backup or
repository migration is required.

A repository that has not enrolled in graph support needs no action at all.

## Install or update

Homebrew updates the paired binaries as one unit:

```bash
brew update
brew upgrade aethyme
```

If the formula was pinned, `brew upgrade` reports the pin instead of updating.
Release `brew unpin aethyme` before upgrading.

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

No broker-storage or repository-deployment migration is required for v0.7.5.
Verify the installed pair and the current repository contract:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

Both version commands must report `0.7.5`. The upgrade plan should report no
mandatory repository migration for a current v0.7.4 deployment.

To enrol a repository in graph support, review the written policy and pin
before materializing:

```bash
aethyme deploy --repo . --with-graph
git diff            # review policy and engine pin, then commit
aethyme graph materialize --repo .
```

## Rollback

Restore both v0.7.4 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol are
unchanged, so no data or repository rollback is required solely because v0.7.5
was installed. A materialized graph store is worktree-local and ignored; delete
it if a rollback should discard it. Never combine binaries from different
Aethyme releases.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
- Graph enrollment is deliberately two-step: nothing is generated until the
  written policy and pin are committed.
- The autonomous build-cache sweep remains disabled by default
  (`artifact_sweep_budget_ms = 0`); `artifact_reclaim_days` defaults to 14.
