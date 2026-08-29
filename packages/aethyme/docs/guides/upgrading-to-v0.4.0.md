# Upgrading to Aethyme v0.4.0

Last Updated: 2026-08-29

v0.4.0 makes broker recovery and verification more truthful under concurrent
work. It adds reviewed checkpoint recovery, safer merge-commit remediation,
complete integration-reconciliation templates, and stricter ownership around
sessions and guarded commands. The router and engine remain one paired
installation; repository deployment stays explicitly per repository.

## Compatibility

| Contract | v0.4.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 17; writes schema 17 |
| Repository deployment | schema 1; no mandatory migration from v0.3.0 |
| Release channel | `stable` for the final `v0.4.0` release |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- Submission planning now starts from an explicit safe base, refuses owned
  merge commits with preservation-first guidance, checkpoints reviewed branch
  rewrites, and avoids queue residue when planning fails.
- Gate reports distinguish missing configuration, no matching gates, executed
  success, cache reuse, and failure instead of calling every zero-gate result
  verified.
- Closed sessions cannot authorize new coordinated operations. Guarded exec
  attributes both newly dirty paths and command changes to files that were
  already dirty.
- External-main reconciliation recognizes patch-equivalent landings even when
  local main already equals upstream. A blocked dry-run emits one complete,
  ready-to-edit schema-2 resolution template with exact evidence and invalid
  placeholders rather than requiring source inspection.
- Redacted broker reports retain the latest broker command failure without
  including command output, task text, diffs, or secrets.

## Before upgrading

Prefer finishing active sessions, then inspect unresolved remote operations:

```bash
aethyme broker status --json
aethyme broker operations list --status unknown --json
aethyme-engine-cli daemon stop --repo /path/to/repo
```

v0.4.0 does not raise the broker-storage or repository-deployment schema from
v0.3.0. A database backup is still prudent when broker history matters. Do not
use a shared stash in a multi-worktree repository.

## Install or update

Homebrew owns Homebrew-managed binary paths:

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

For a source build of a reviewed revision, install both executables from the
same checkout and verify their versions. Never update only one executable.

## Migrate and verify

The machine-wide binary update never searches for repositories. In each
enrolled repository, inspect the embedded migration plan; v0.3.0 schema-1
deployments should normally be unchanged:

```bash
cd /path/to/repo
aethyme upgrade plan --repo . --diff
aethyme upgrade apply --repo . --confirm <plan-sha256> # only if writes are proposed
aethyme deploy verify --repo .
git diff --check
```

Then verify the installed pair and disposable broker lifecycle:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
```

## Rollback

1. Stop the engine daemon.
2. Restore both v0.3.0 binaries together through Homebrew, the installer
   rollback bundle, or a reviewed source checkout.
3. Restore a database backup if the newer binary wrote state you do not want
   the older binary to observe.
4. Revert any repository migration commit separately; never edit a schema
   marker by hand.
5. Verify both binary versions and run `aethyme broker quick-test`.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme makes no background network request.
- Repository review remains deliberately per repository.
- Homebrew installations must be upgraded through Homebrew; the native updater
  will not modify the Cellar.
- Complex or partially evidenced remote writes remain blocked until explicit
  operation reconciliation; Aethyme never retries an unknown outcome blindly.
