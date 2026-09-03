# Upgrading to Aethyme v0.7.3

Last Updated: 2026-09-03

v0.7.3 makes integration state truthful after a pull request is merged through
the coordinated GitHub command. Aethyme refreshes upstream state, recognizes a
fully landed promotion layer, and cleans it without requiring manual Git ref
repair. Any ambiguity remains review-gated.

## Compatibility

| Contract | v0.7.3 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.2 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- Successful coordinated `gh pr merge` operations now trigger a separately
  journaled refresh of the configured upstream branch.
- Aethyme automatically reconciles the complete recorded integration layer
  only when exact ancestry, stable patch identity, cumulative squash evidence,
  or path content proves that every promotion has landed.
- Status output distinguishes safely landed stale promotions from unresolved
  integration divergence.
- Pending entries, ambiguous equivalence, partially landed layers, and
  unrecorded integration commits remain unchanged and route to a digest-bound
  reconciliation plan.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. v0.7.3 does not change broker storage,
repository deployment, or engine protocol, so no database backup or repository
migration is required solely for this update.

No generated repository file must change merely to install v0.7.3. Inspect any
repository-owned migration separately and locally:

```bash
aethyme upgrade plan --repo . --diff
```

## Install or update

Homebrew updates the paired binaries as one unit:

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

No broker-storage or repository-deployment migration is required for v0.7.3.
Verify the installed pair and the current repository contract:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

Both version commands must report `0.7.3`. The upgrade plan should report no
mandatory repository migration for a current v0.7.2 deployment.

## Rollback

Restore both v0.7.2 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol are
unchanged, so no data or repository rollback is required solely because
v0.7.3 was installed. Never combine binaries from different Aethyme releases.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
- Automatic cleanup is deliberately all-or-nothing for the recorded promoted
  layer. Mixed or ambiguous state requires reviewed reconciliation.
