# Upgrading to Aethyme v0.4.2

Last Updated: 2026-08-29

v0.4.2 is a broker lifecycle patch. It adds an inspectable, explicit way to
reclaim broker-spawned worktrees after sessions are finished, including large
per-worktree build directories. It does not change repository deployment,
broker storage, or the engine protocol.

## Compatibility

| Contract | v0.4.2 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 17; writes schema 17 |
| Repository deployment | schema 1; no mandatory migration from v0.4.1 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `aethyme broker cleanup --all-cleaned` produces a read-only plan for retained
  broker-spawned worktrees, including eligibility and estimated bytes.
- Add `--apply` only after reviewing the plan. Every worktree is revalidated
  immediately before removal.
- Cleanup recognizes work represented by main, integration, or configured
  upstream. Adopted worktrees and unsafe candidates are excluded.
- `broker status` reports eligible retained worktrees and the exact review and
  apply commands.

`finish` still deliberately leaves a worktree in place for review or reuse. It
does not remove the current shell's directory automatically.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. No database or repository migration
is required.

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

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme broker cleanup --all-cleaned
```

Review the listed sessions, dispositions, paths, and byte estimates. To remove
only candidates that still pass all safety checks:

```bash
aethyme broker cleanup --all-cleaned --apply
```

## Rollback

Restore both v0.4.1 binaries together through the original installation
manager. Broker storage and repository schema are unchanged, so no data or
repository rollback is normally required. Worktrees already removed by an
explicit cleanup cannot be restored by downgrading; submitted commits remain
available through their Git refs and integration history.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Homebrew installations must be upgraded through Homebrew.
- Complex or partially evidenced remote writes remain blocked until explicit
  operation reconciliation; Aethyme never retries an unknown outcome blindly.
