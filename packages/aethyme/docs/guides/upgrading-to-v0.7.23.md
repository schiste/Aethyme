# Upgrading to Aethyme v0.7.23

Last Updated: 2026-09-19

This release strengthens cleanup and installation safety, isolates gate
databases, and improves navigation when an optional graph is unavailable.

## Changes since v0.7.22

- Storage cleanup binds preparation-cache deletion to the reviewed plan,
  rechecks liveness, and preserves caches when liveness cannot be established.
  Shared preparation caches are included in storage inventory and reclaim.
- Gate subprocesses use disposable broker databases, protecting the operator's
  shared database from unreviewed migrations. Deployment test fixtures also
  isolate their database environment from the enclosing gate.
- Coordinated operations persist heartbeat and progress information.
  Diagnostics distinguish unknown liveness from stale heartbeats and retain
  heartbeats through reconciliation.
- Installation checks archive digests against the release manifest, rejects
  missing or ambiguous artifacts, and parses the manifest with jq. Signature
  verification binds the archive to the signed manifest; unsigned mode warns
  that a checksum alone does not authenticate a release.
- Readiness validates review configuration with the runtime policy loaders.
- Explore provides bounded source-navigation hints without a graph. These are
  verification targets, not graph-backed semantic or impact-analysis answers.
- CI runs full workspace tests on PRs and combined gates on main. The duplicate
  binary-only workflow remains manually available. Narrow script and workflow
  contracts protect installer, pilot-reporting, and scheduling changes.
- Privacy-safe pilot cost snapshots and comparisons, an external pilot
  protocol, and operational recovery guidance are included. This tooling does
  not claim that external adoption trials have already been completed.

## Compatibility and upgrade notes

- There is no new broker database migration relative to v0.7.22: schema 41
  remains current. Upgrades from older versions must also follow the
  [v0.7.22 migration guidance](upgrading-to-v0.7.22.md).
- The shell installer now requires jq. Signature verification additionally
  requires cosign; use a reviewed installer file with --verify-signature.
- Install the router and engine together from the same release. Let running
  gates finish before changing the installed pair.
- No repository graph enrollment or graph materialization is required to use
  the new navigation hints.

## Install or update

For Homebrew installations:

```bash
brew update
brew upgrade schiste/tap/aethyme
aethyme --version
aethyme-engine-cli --version
```

Both binaries should report 0.7.23. For a first installation, use
`brew install schiste/tap/aethyme`.

For source installations, check out v0.7.23 and install the locked pair:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

See [operational recovery](operational-recovery.md) for paired-runtime and
integration recovery, and [external pilots](external-pilots.md) for the
privacy-safe measurement protocol.

## Before upgrading

Let running gates finish, record both installed binary versions, and keep a
backup of broker coordination state. If upgrading from v0.7.21 or earlier,
review the one-way schema migration described in the v0.7.22 guide first.

## Migrate and verify

No migration command is needed from v0.7.22. After installing the pair:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker status
aethyme certify
```

Both version commands should report 0.7.23. Check that existing sessions and
their worktrees remain visible; do not recreate or delete broker state to
silence a diagnostic.

## Rollback

v0.7.22 and v0.7.23 use the same broker database schema. If rollback is
necessary, stop active gates and reinstall both v0.7.22 binaries together.
This also removes the cleanup and gate-isolation fixes in this release.
Do not downgrade to v0.7.21 against a schema-41 database: restore a compatible
pre-upgrade backup or remain on a schema-41-capable binary.

## Known issues

- Storage planning on large primary checkouts can remain slow (#235).
- Resource-reaping diagnostics can omit holder identities (#234).
- Graph-free Explore hints are navigation-only and require source verification;
  they are not a replacement for graph-backed impact analysis.
- Source installs must use --locked. Unsigned shell installation does not
  authenticate the release; use --verify-signature when that assurance is needed.
