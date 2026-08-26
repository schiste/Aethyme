# Upgrading to Aethyme v0.2.2

Last Updated: 2026-08-24

v0.2.2 adds host-wide resource leases for concurrent validation, an opt-in
pre-push adapter, and explicit embedded migrations for Aethyme-owned
repository policy. It is the first release that distinguishes updating the
machine-wide binary pair from updating each enrolled repository.

If upgrading from v0.1.x, first read the broader
[v0.2.0 migration guide](upgrading-to-v0.2.0.md), which covers the native Rust
cutover and paired-binary installation model.

## Compatibility

| Contract | v0.2.2 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same archive |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 8; writes schema 8 |
| Repository deployment | schema 1; explicit migration required for older enrolled repositories |
| Release channel | `stable` through GitHub's latest non-prerelease release |

The signed release manifest records these compatibility values, the exact
source SHA, and every archive size and SHA-256 digest.

## Before upgrading

Finish or record active broker work, stop the engine daemon, and back up each
broker database whose history must survive rollback:

```bash
aethyme broker status
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /safe/location/broker.db.v0.2.1
```

Opening a repository with v0.2.2 migrates broker storage from schema 7 to 8.
v0.2.1 cannot reopen schema 8, so its database backup is required for a full
rollback.

Commit or stash repository changes before migrating repository policy.
`aethyme upgrade apply` deliberately refuses a dirty worktree.

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
curl -fL -o install.sh https://github.com/schiste/Aethyme/releases/download/v0.2.2/install.sh
less install.sh
sh install.sh --version 0.2.2 --verify-signature
```

Source installation remains a contributor fallback:

```bash
git switch --detach v0.2.2
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Never update only one binary; the router and engine daemon are one product
version.

## Migrate and verify

The binary updater never searches for repositories or changes them
implicitly. Enter every enrolled canonical repository and review its embedded
migration:

```bash
cd /path/to/repo
aethyme upgrade plan
aethyme upgrade apply --confirm <plan-sha256>
git diff --check
git diff
aethyme deploy verify --repo .
```

Review and commit the migration output, including
`.aethyme/repository.json`, with the generated policy files. Until that commit
lands, other clones remain on the previous repository contract and v0.2.2
broker commands will direct them to migrate.

For an activated local-only bridge, keep the migration clone-local:

```bash
aethyme upgrade plan --local-only
aethyme upgrade apply --local-only --confirm <plan-sha256>
aethyme deploy verify --local-only --repo .
```

Finally verify the installed pair and disposable broker workflow:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
```

## Rollback

1. Stop the engine daemon.
2. Restore both v0.2.1 binaries together through Homebrew, the installer's
   retained rollback bundle, or a detached v0.2.1 source checkout.
3. Restore the v0.2.1 broker database backup before running broker commands.
4. For canonical deployment, revert the repository migration commit before
   using v0.2.1 in that repository.
5. Verify both versions and run `aethyme broker quick-test`.

Binary rollback does not automatically reverse a committed repository
migration. Do not hand-edit the repository schema marker to simulate a
rollback.

## Known issues

- Windows and Linux ARM archives are not published.
- There is no silent or scheduled updater. Homebrew and native update checks
  run only when explicitly requested.
- Repository upgrades are intentionally one repository at a time; Aethyme
  does not maintain or scan a machine-wide repository registry.
- An interrupted repository migration requires the matching rollback journal
  and explicit `aethyme upgrade recover --plan <plan-sha256>`. Recovery rolls
  back; it never retries. An in-progress marker alone is not authority.
- Host-resource fallback allocation belongs to the consuming repository.
  Aethyme refuses broker-managed allocation failures rather than falling back
  to a fixed port or shared Docker name.
