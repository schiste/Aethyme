# Upgrading to Aethyme v0.5.0

Last Updated: 2026-09-01

v0.5.0 is a broker coordination and publication release. It makes submission,
promotion, publication, review, external automation, resource routing, and
cleanup evidence explicit and durable. The release does not require a
repository deployment migration, but it does migrate broker storage.

## Compatibility

| Contract | v0.5.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 24; writes schema 24 |
| Repository deployment | schema 1; no mandatory migration from v0.4.2 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- Submission and publication now retain truthful provenance and path exposure
  until verified remote evidence resolves it.
- Opt-in review and publication policy can require independent approval,
  successful gate evidence, and a clean remote-operation barrier.
- Gate scope manifests, lease routing, and authenticated external events let
  repository adapters coordinate shared services without embedding their
  semantics in Aethyme.
- Status, finish, cleanup, and garbage collection use bounded retention and
  preservation-first remediation.
- First-time repository enrollment is atomic, and protected-branch hooks fail
  closed until the broker session is active.

The new `[review]`, `[publication]`, `[leases.routing]`, and `[retention]`
configuration surfaces are opt-in. Existing repositories retain their current
behavior unless a maintainer enables them.

## Before upgrading

Finish active sessions when practical. Confirm both installed executables come
from the same installation manager. If rollback to v0.4.2 must remain possible,
make a recoverable copy of `.aethyme/broker.db` while no broker command is
running; v0.5.0 upgrades that database to schema 24 on first open.

No generated repository file must change merely to install v0.5.0. To review
whether the installed binary proposes any repository-owned update, use the
read-only plan before applying anything:

```bash
aethyme upgrade plan --repo . --diff
```

## Install or update

Homebrew users update the paired installation as one unit:

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

The broker database migration is transactional and runs automatically when a
v0.5.0 broker command first opens the repository. Repository deployment remains
schema 1, so do not run a write migration unless the upgrade plan proposes one
and its exact digest has been reviewed.

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

If the upgrade plan contains intentional repository changes, review the exact
diff and apply only its current digest using the command printed by the plan.

## Rollback

Restore both v0.4.2 binaries together through the original installation
manager. A v0.4.2 broker cannot open a database already migrated to schema 24;
restore the pre-upgrade `.aethyme/broker.db` copy before using the older broker,
or keep v0.5.0 for broker operations. Do not combine a v0.4.2 router with a
v0.5.0 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
do not need to be rolled back unless an explicit repository upgrade was
separately reviewed and applied.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Homebrew installations must be upgraded through Homebrew.
- The broker database migration is forward-only; downgrade requires the
  pre-upgrade database copy described above.
- Review, external-event, lease-routing, retention, and publication policies
  are disabled until configured by a repository maintainer.
- Complex or partially evidenced remote writes remain blocked until explicit
  operation reconciliation; Aethyme never retries an unknown outcome blindly.
