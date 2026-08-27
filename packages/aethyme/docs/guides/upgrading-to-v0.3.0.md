# Upgrading to Aethyme v0.3.0

Last Updated: 2026-08-27

v0.3.0 completes the broker stabilization, transactional repository-upgrade,
cross-clone coordination, durable advisory, and reproducible onboarding work
that followed v0.2.2. The router and engine remain one paired installation;
repository policy is still upgraded explicitly in each enrolled repository.

## Compatibility

| Contract | v0.3.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 17; writes schema 17 |
| Repository deployment | schema 1; generated policy may require regeneration |
| Release channel | `stable` for a final `v0.3.0` release |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## Before upgrading

Finish active sessions and inspect unresolved operations before replacing the
binary pair. Back up broker history when rollback matters:

```bash
aethyme broker status --json
aethyme broker operations list --status unknown --json
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /safe/location/broker.db.v0.2.2
```

Opening a repository with v0.3.0 may migrate broker storage through schema 17.
Older binaries cannot reopen a newer database, so restoring the older pair
also requires restoring its compatible database backup.

Do not stash shared multi-worktree state. Commit through an eligible pinned
session, finish active sessions, or leave unrelated dirty work in place while
reviewing an exact repository migration plan.

## Install or update

Homebrew-managed installations remain owned by Homebrew:

```bash
brew update
brew upgrade aethyme
```

Installer-managed users review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

For a source build of an unreleased or pinned revision, install both crates
from the same checkout and verify the pair before use:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
aethyme --version
aethyme-engine-cli --version
```

Never update only one executable.

## Migrate and verify

The machine-wide update never scans for repositories. Enter each enrolled
repository and review its pure migration plan before applying anything:

```bash
cd /path/to/repo
aethyme upgrade plan --repo . --diff
aethyme upgrade plan --repo . --json
aethyme upgrade apply --repo . --confirm <plan-sha256>
aethyme deploy verify --repo .
git diff --check
git diff
```

An already-current repository produces no migration write. If generated
onboarding needs refreshing, run `aethyme enhance deploy --repo .`, review the
tracked output, and verify again. Canonical and activated local-only
deployments must retain their existing mode.

Finally smoke the installed runtime and disposable broker lifecycle:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
```

## Rollback

1. Stop the engine daemon.
2. Restore the previous router and engine binaries together using the
   installer rollback bundle, Homebrew, or a pinned source checkout.
3. Restore the broker database backup compatible with that binary.
4. Recover or revert any applied repository migration; never edit the schema
   marker by hand.
5. Verify both binary versions and run `aethyme broker quick-test`.

Binary rollback does not reverse committed repository policy.

## Known issues

- Windows and Linux ARM archives are not published.
- No background update request is made; checks and execution are explicit.
- Repository upgrades remain deliberately per-repository.
- `.aethyme/broker-advisory.md` is ignored persistence, not automatic agent
  delivery; managed hook and broker command surfaces deliver notices.
- Acknowledgment, session close, and rebase do not resolve unpublished entry
  exposure.
- An interrupted migration requires `aethyme upgrade recover --plan
  <plan-sha256>`; an in-progress marker never authorizes an implicit retry.
