# Upgrading to Aethyme v0.2.0

Last Updated: 2026-08-23

v0.2.0 is the first release with paired prebuilt binaries, a signed release
manifest, standalone checksums, and a stable install/update channel. It also
completes the native Rust cutover: `python -m src.cli` is intentionally gone
and has no compatibility shim.

## Compatibility

| Contract | v0.2.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same archive |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 7; writes schema 7 |
| Release channel | `stable` through GitHub's latest non-prerelease release |

The manifest records these values alongside archive sizes and SHA-256 digests.
A newer broker can migrate an older supported database on first open. That is
forward compatibility, not a promise that an older binary can reopen the
migrated database.

## Before upgrading

Finish or record active work, stop any engine daemon for each repository, and
back up broker state before the first v0.2.0 broker command:

```bash
aethyme broker status
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /path/to/repo/.aethyme/broker.db.pre-v0.2.0
```

Repeat the database backup for every broker-managed repository that must be
rollback-safe. The graph store does not need a backup: `.aethyme/graph_store.redb`
is a derived local artifact rebuilt from committed `.aethyme/graph/` fragments.

## Install or update

The primary stable-channel path for early macOS and Linux users is Homebrew:

```bash
brew install schiste/tap/aethyme
# Later:
brew update
brew upgrade aethyme
```

The formula selects one platform archive and installs both required binaries
from it. `schiste/tap` is a third-party source outside `homebrew/core`. Direct
formula installation trusts only `schiste/tap/aethyme`; trusting the whole tap
is a broader choice that is unnecessary for Aethyme.

The portable installer path is:

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
```

The installer selects the native target, downloads the exact versioned
archive, verifies its standalone SHA-256, requires exactly the two expected
archive members, verifies both embedded versions, and activates a versioned
pair through one shared symlink. It installs to `~/.local/bin` unless
`AETHYME_INSTALL_DIR` or `--install-dir` is supplied.

Later installer-managed updates use an explicit review/confirm flow:

```bash
aethyme update check
aethyme update plan --channel stable
# Review the version, source SHA, compatibility, archive, and digest above.
aethyme update execute --confirm <manifest-sha256>
```

`check` and `plan` are the only steps that discover a release, and they run
only when invoked. `execute` re-downloads the exact reviewed manifest, verifies
the archive digest and members, tests the staged pair, atomically changes the
shared version link, and retains the former bundle for rollback. Use
`--channel preview` only to discover the latest published GitHub prerelease;
the saved plan pins its exact versioned manifest, and preview releases never
change the stable Homebrew formula.

For a reviewed, pinned install:

```bash
curl -fL -o install.sh https://github.com/schiste/Aethyme/releases/download/v0.2.0/install.sh
less install.sh
sh install.sh --version 0.2.0 --verify-signature
```

`--verify-signature` requires Cosign 3. It verifies the manifest bundle against
the exact Aethyme release-workflow identity and GitHub Actions OIDC issuer
before trusting the manifest. The manifest then authenticates the installer
and archive digests. Without that flag, archive checksum and contents
verification are still mandatory.

Source installation remains a contributor fallback when a prebuilt target is
unavailable:

```bash
git switch --detach v0.2.0
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Never install only one crate. The router and engine sibling are one product
version and share a daemon protocol.

## Migrate and verify

Verify the pair first, then open each managed repository:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test

cd /path/to/repo
aethyme certify
aethyme broker status
```

Opening broker state applies supported schema migrations transactionally. If
an existing redb graph store uses an incompatible file format or schema,
rebuild the derived store without changing committed graph fragments:

```bash
aethyme-engine-cli index --repo /path/to/repo
```

The index command detects incompatible redb formats, replaces only the derived
`.aethyme/graph_store.redb`, and leaves `.aethyme/graph/` untouched.

## Rollback

Binary rollback and state rollback are separate operations.

1. Stop the v0.2.0 engine daemon.
2. Restore both earlier binaries together. Installer-managed updates report
   the retained rollback bundle; otherwise use backups or check out the
   earlier tag and run both `cargo install --locked --path ...` commands.
3. If v0.2.0 opened and migrated a broker database, restore that repository's
   `broker.db.pre-v0.2.0` before running the older broker.
4. Rebuild `.aethyme/graph_store.redb` with the restored engine if graph queries
   report an incompatible store.
5. Verify both versions and run `aethyme broker quick-test`.

The v0.2.0 installer can pin any release that publishes the new manifest
contract. v0.1.x predates that contract, so rolling back to v0.1.x requires
saved binaries or a source checkout.

## Known issues

- Windows and Linux ARM archives are not published in v0.2.0.
- There is no silent or scheduled updater. Homebrew users explicitly run
  `brew upgrade aethyme`; installer users explicitly run `update check`,
  review `update plan`, and confirm `update execute` with the full manifest
  digest.
- Update execution validates the broker database in the current working
  repository when present. It does not discover every broker-managed checkout;
  retain backups for repositories whose schema rollback matters.
- `aethyme broker doctor --fix-version` repairs source-checkout drift from the
  local integration ref. It is not the public GitHub release updater.
- The first graph-store rebuild after an incompatible redb format can take as
  long as a normal full index.
- Existing generated repository guidance is not rewritten by binary install;
  rerun `aethyme enhance deploy --repo /path/to/repo` when adopting updated
  templates.
