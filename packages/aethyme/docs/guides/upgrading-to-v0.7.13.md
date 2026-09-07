# Upgrading to Aethyme v0.7.13

Last Updated: 2026-09-07

v0.7.13 changes maintainer documentation only. There are no behaviour changes,
no schema changes, and no change to the generated agent policy.

## Compatibility

| Contract | v0.7.13 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.12 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

The release runbook now states the enhancement redeploy as its own step rather
than leaving it implicit, and records the two orderings it depends on:

- **Build at the new version before deploying.** The policy stamp comes from
  the compiled `CARGO_PKG_VERSION`, so deploying from the installed binary
  records the previous version.
- **Stage new files before deploying.** The generated onboarding freshness
  digest counts tracked files, so deploying before `git add` omits the upgrade
  guide the release just created.

The second produced a digest one file short in v0.7.12. It is self-correcting at
the next deploy and affects no behaviour.

## Before upgrading

Nothing. Mixed v0.7.12/v0.7.13 installations can share a repository, and no
migration or redeployment is required.

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
```

Both version commands must report `0.7.13`. No repository action is needed: the
generated policy is unchanged from v0.7.12, so `enhance deploy` will report the
policy files unchanged apart from the version stamp.

## Rollback

Restore both v0.7.12 binaries together through the original installation
manager. No schema migration has run and no repository state changed. Never
combine binaries from different releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- The no-catch-up-merge practice ships static. Emitting it conditionally on the
  default branch's freshness requirement would need a provider API call inside
  `enhance deploy`, which is offline today.
- Coordinated operations still serialize per repository.
  `[coordination] hooks_outside_lock` removes the local gate from the critical
  section, but two operations on unrelated refs still contend.
- `main reconcile` classifies commits as represented or not; four-way
  classification with resolution templates is not implemented.
- Schema migration, when a release carries one, is implicit. This release
  carries none.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
