# Upgrading to Aethyme v0.4.1

Last Updated: 2026-08-29

v0.4.1 is a packaging-integrity patch for v0.4.0. It refreshes Aethyme's
checked-in generated agent protocol and deterministic onboarding metadata so
the exact release tree passes enhancement verification. Runtime behavior,
storage compatibility, and repository schema remain unchanged.

## Compatibility

| Contract | v0.4.1 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 17; writes schema 17 |
| Repository deployment | schema 1; no mandatory migration from v0.4.0 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- Generated AGENTS and CLAUDE guidance now includes the reviewed,
  preservation-first recovery sequence for unsupported session merge commits.
- Deterministic onboarding freshness metadata reflects the complete release
  input set, so `aethyme enhance verify` passes on the tagged source tree.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. v0.4.1 does not change broker storage
or repository schema, so no database or repository migration is required.

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

No schema migration is required. Existing repositories remain intentionally
independent of the machine-wide binary installation. Verify or review each
enrolled repository explicitly:

```bash
cd /path/to/repo
aethyme upgrade plan --repo . --diff
aethyme enhance verify --repo .
git diff --check
```

If a repository has customized managed policy, the planner will require an
explicit preserve, merge, or replace resolution; it will not overwrite the
customization silently.

## Rollback

Restore both v0.4.0 binaries together through the original installation
manager. Broker storage and repository schema are unchanged, so no data or
repository rollback is normally required.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Homebrew installations must be upgraded through Homebrew.
- Complex or partially evidenced remote writes remain blocked until explicit
  operation reconciliation; Aethyme never retries an unknown outcome blindly.
