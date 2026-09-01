# Upgrading to Aethyme v0.6.0

Last Updated: 2026-09-01

v0.6.0 moves newly created broker worktrees outside the repository checkout.
This prevents broad repository scanners from discovering complete nested
checkouts while preserving the same broker session, lease, gate, submission,
and cleanup workflows.

## Compatibility

| Contract | v0.6.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 24; writes schema 24 |
| Repository deployment | schema 1; no mandatory migration from v0.5.0 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- New broker worktrees use a private per-user host-state directory keyed by
  the clone's canonical Git common directory.
- `aethyme broker worktree-root --json` explains the preferred root without
  creating it.
- `broker start` and `broker start-agent` report the selected root and whether
  it is outside the repository.
- Starts from an existing broker worktree create a sibling checkout. Roots
  inside any linked worktree are refused.
- External roots carry a private repository ownership marker. Cleanup accepts
  that marker and continues to recognize legacy `.aethyme/worktrees/` sessions.

`AETHYME_WORKTREE_ROOT` may select an alternative external base. The broker
appends its clone-specific key. An invalid explicit override fails closed. If
the normal platform host-state directory is unavailable, the broker reports
the error and falls back to the legacy repository-local root.

## Before upgrading

Finish active sessions when practical and confirm the installed router and
engine sibling come from the same installation manager. No storage or
repository migration is required. Existing sessions retain their recorded
absolute paths and can be finished or cleaned normally after the upgrade.

Inspect the new placement decision before creating a session:

```bash
aethyme broker worktree-root --json
```

## Install or update

Homebrew users update both binaries as one formula-managed unit:

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

No repository or broker-storage migration is required for v0.6.0. Verify the
paired binaries and placement contract after updating:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker worktree-root --json
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

The `preferred_outside_repository` field should be `true`. A normal
`broker start --json` should return `worktree_placement.outside_repository` as
`true` and `source` as `host_state` unless an external override is configured.

## Rollback

Restore both v0.5.0 binaries together through the original installation
manager. Broker storage and repository deployment schemas are unchanged, so no
database or repository rollback is needed. Worktrees created by v0.6.0 remain
ordinary linked Git worktrees and stay discoverable to v0.5.0, but v0.5.0 does
not recognize the external ownership marker for destructive cleanup. Finish
or remove those sessions with v0.6.0 before rolling back when practical.

Never combine a v0.5.0 router with a v0.6.0 engine sibling.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Homebrew installations must be upgraded through Homebrew.
- A host-state permission or filesystem failure activates the clearly reported
  legacy fallback; configure a writable external `AETHYME_WORKTREE_ROOT` to
  restore scanner isolation.
- Old repository-local worktrees are not relocated automatically. They remain
  cleanup-compatible and new sessions use the external root.
