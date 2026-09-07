# Upgrading to Aethyme v0.7.14

Last Updated: 2026-09-07

v0.7.14 lets `main reconcile` carry reviewed decisions about local work it
cannot prove already landed. No schema changes, and no change to the generated
agent policy.

## Compatibility

| Contract | v0.7.14 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.13 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

`main reconcile` previously refused whenever the default branch carried a commit
it could not prove represented on integration. That is right for work still
needed there, and wrong for work superseded elsewhere or an experiment that
should simply leave the branch.

```bash
aethyme broker main reconcile plan --write-resolution-template resolutions.json
# edit each entry's resolution and reason
aethyme broker main reconcile plan --resolution-file resolutions.json
aethyme broker main reconcile apply --session <id> --confirm <digest> \
    --resolution-file resolutions.json
```

Three dispositions, one of which unblocks the move:

| Disposition | Effect |
| --- | --- |
| `replay_through_broker` | default; keeps refusing, because the work belongs in integration |
| `archive_local` | accepts that it leaves the default branch, still reachable from the preservation ref |
| `keep_local_and_block_publication` | keeps it and refuses to move at all |

Three properties are deliberate. A commit with no entry stays undecided and
keeps refusing, so the file records a decision rather than waiving the check.
`already_represented` cannot be chosen — it stays computed from content, because
asserting it would defeat the comparison that makes moving the branch safe. And
the decisions are bound into the plan digest with a required reason per entry,
so an edited file no longer matches a reviewed plan.

`archive_local` is safe because the preservation ref is created before anything
moves: archiving changes what the default branch carries, not what the
repository can reach.

## Before upgrading

Nothing. Mixed v0.7.13/v0.7.14 installations can share a repository, no
migration runs, and the generated policy is unchanged.

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
aethyme broker quick-test
aethyme broker main reconcile plan
```

Both version commands must report `0.7.14`. On a repository whose default branch
is level with integration, `main reconcile plan` reports that it carries nothing
integration does not already contain.

## Rollback

Restore both v0.7.13 binaries together through the original installation
manager. No schema migration has run and no repository state changed. A
resolution file written under v0.7.14 is simply unused by v0.7.13, which refuses
unrepresented work unconditionally as before.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- `main reconcile` names unrepresented work and requires a decision about it,
  but does not replay it for you: creating a session and cherry-picking remains
  manual.
- The no-catch-up-merge practice ships static. Emitting it conditionally on the
  default branch's freshness requirement would need a provider API call inside
  `enhance deploy`, which is offline today.
- Coordinated operations still serialize per repository.
  `[coordination] hooks_outside_lock` removes the local gate from the critical
  section, but two operations on unrelated refs still contend.
- Schema migration, when a release carries one, is implicit. This release
  carries none.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
