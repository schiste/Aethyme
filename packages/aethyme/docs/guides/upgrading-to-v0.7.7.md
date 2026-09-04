# Upgrading to Aethyme v0.7.7

Last Updated: 2026-09-04

v0.7.7 completes the opt-in graph rollout by ensuring that derived redb query
stores stay local and never appear as candidate repository changes.

## Compatibility

| Contract | v0.7.7 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.6 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

- Canonical deployment adds `.aethyme/graph_store.redb` and
  `.aethyme/graph_store.redb.indexing` to the managed `.gitignore` block.
- Existing managed blocks are upgraded without changing maintainer-owned text.
- Committed graph fragments under `.aethyme/graph/` remain tracked authority.

## Before upgrading

Finish active sessions when practical and verify that the installation manager
owns both binaries. No broker database or repository schema migration is
required. If an enrolled repository currently shows a derived redb file as
untracked, do not add it to Git; deploy v0.7.7 to refresh the managed block.

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

Refresh repository-owned deployment files, review the exact managed-block
change, and commit it with the repository policy:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme deploy --repo .
git check-ignore -v .aethyme/graph_store.redb
git status --short --untracked-files=all .aethyme
aethyme enhance verify --repo .
aethyme graph status --repo . --json
```

Both version commands must report `0.7.7`. Graph-disabled repositories remain
healthy no-ops. Enrolled repositories keep committed fragments visible while
the derived redb files are ignored. The status check may list committed graph
fragments that have not been added yet, but it must not list either redb file.

## Rollback

Restore both v0.7.6 binaries together through the original installation
manager. Compatibility schemas are unchanged. The two new ignore entries may
remain safely: they name derived local files, not repository authority. Never
combine binaries from different Aethyme releases.

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
