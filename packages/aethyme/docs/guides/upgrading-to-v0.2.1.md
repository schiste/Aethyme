# Upgrading to Aethyme v0.2.1

Last Updated: 2026-08-24

v0.2.1 is a patch release that keeps broker submission ownership stable across
session reuse and makes already-integrated, patch-equivalent submissions
truthful no-content outcomes. It also produces an audit-clean Homebrew formula.

If upgrading from v0.1.x, first read the broader
[v0.2.0 migration guide](upgrading-to-v0.2.0.md), which covers the native Rust
cutover and paired-binary installation model.

## Compatibility

| Contract | v0.2.1 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same archive |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 7; writes schema 7 |
| Release channel | `stable` through GitHub's latest non-prerelease release |

There is no protocol or storage migration from v0.2.0. The signed release
manifest records these compatibility values and the exact source SHA, archive
sizes, and SHA-256 digests.

## Before upgrading

Finish or record active broker work before replacing the binary pair:

```bash
aethyme broker status
aethyme-engine-cli daemon stop --repo /path/to/repo
```

v0.2.1 does not change the broker schema, but retaining a recent copy of
`.aethyme/broker.db` remains prudent when rollback continuity matters.

## Install or update

Homebrew users update through Homebrew so it remains the sole owner of both
binary paths:

```bash
brew update
brew upgrade aethyme
```

Installer-managed users can review and confirm the signed release plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

For a reviewed pinned install:

```bash
curl -fL -o install.sh https://github.com/schiste/Aethyme/releases/download/v0.2.1/install.sh
less install.sh
sh install.sh --version 0.2.1 --verify-signature
```

Source installation remains a contributor fallback:

```bash
git switch --detach v0.2.1
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Never update only one binary; the router and engine daemon are one product
version.

## Migrate and verify

No repository migration is required from v0.2.0. Verify the installed pair and
then inspect each actively managed repository:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test

cd /path/to/repo
aethyme certify
aethyme broker status
```

Existing generated repository guidance does not need redeployment for the
v0.2.1 fixes.

## Rollback

1. Stop the engine daemon.
2. Restore both v0.2.0 binaries together through Homebrew, the installer's
   retained rollback bundle, or a detached `v0.2.0` source checkout.
3. Verify both versions and run `aethyme broker quick-test`.

Because v0.2.1 does not migrate broker storage, a database restored from a
v0.2.1 run remains readable by v0.2.0. A backup is still the safest boundary
for repositories with irreplaceable operation history.

## Known issues

- Windows and Linux ARM archives are not published.
- There is no silent or scheduled updater. Homebrew and native update checks
  run only when explicitly requested.
- Existing generated repository guidance is not rewritten by installation;
  rerun `aethyme enhance deploy --repo /path/to/repo` only when intentionally
  adopting newer generated templates from a future release.
- `aethyme broker doctor --fix-version` repairs a source checkout from its
  local integration ref; it is not the public release updater.
