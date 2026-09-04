# Upgrading to Aethyme v0.7.6

Last Updated: 2026-09-04

v0.7.6 makes opt-in graph materialization practical across broker worktrees,
adds measurable graph lifecycle evidence, and delivers broker recovery and
field-report fixes. Repositories that have not enrolled in graph support do no
graph work and need no graph migration.

## Compatibility

| Contract | v0.7.6 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.5 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds compatibility to the exact source SHA and
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `graph status` is cheap and healthy when graphing is disabled.
- `graph materialize` reads and verifies committed fragments without cloning,
  parsing source, or regenerating fragments.
- Identical graph inputs may reuse an immutable host-cache artifact. The key
  binds the source tree, manifest, engine and protocol version, and redb schema.
  A cache hit is copied, verified, rebound to the receiving worktree, and
  atomically published; writable databases are never shared.
- Graph lifecycle JSON exposes content-free phase, byte, count, disk, and peak
  memory evidence. Runtime observations do not authorize a refresh.
- `broker promotion-record plan/apply` can restore a proven interrupted
  promotion record under a reviewed digest.
- Implicit push refspecs are resolved to exact refs before coordinated failure
  reconciliation. Unchanged remote refs now prove an ordinary failed push.
- New-worktree preparation and unsafe main-checkout adoption report their
  consequences earlier, and status JSON exposes both `agents` and `sessions`.

## Before upgrading

Finish active sessions when practical and verify that the installation manager
owns both binaries. No broker database or repository policy migration is
required. Existing local redb files remain valid; cache entries are derived and
are ignored when any key or integrity check differs.

## Install or update

Homebrew updates both binaries as one formula transaction:

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
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme graph status --repo . --json
```

Both version commands must report `0.7.6`. On a graph-disabled repository,
status should be successful, healthy, and require no action. On an enrolled
repository whose local store is missing, run
`aethyme graph materialize --repo . --json`; inspect `cache.status` without
assuming a hit is required.

## Rollback

Restore both v0.7.5 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol are
unchanged, so no data or repository rollback is required. Worktree graph stores
and host-cache entries are derived from committed fragments and may be removed.
Never combine binaries from different Aethyme releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- A killed gate can leave a host-resource lease active until expiry; inspect
  `aethyme broker resources list --json` if capacity appears stuck.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
