# Upgrading to Aethyme v0.7.4

Last Updated: 2026-09-04

v0.7.4 makes broker storage account for itself. Garbage collection now reports
the disk it is holding rather than only the bytes it will free, reclaims
git-ignored build caches independently of commit provenance, and sweeps
worktree roots whose owning repository no longer exists. Every reclaim remains
digest-confirmed, and the one unconfirmed path is opt-in and limited to build
caches.

## Compatibility

| Contract | v0.7.4 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.3 |
| GC plan schema | 2 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `broker gc plan` reports `estimated_retained_bytes` and
  `estimated_blocked_bytes` beside `estimated_reclaimable_bytes`. A plan that
  frees little now says how much it is deliberately holding back and why.
- Build caches inside retained worktrees are reclaimable independently of the
  worktree's cleanup disposition. Blocked dispositions protect commits; a
  build cache holds none and is recovered by rebuilding.
- Orphaned host worktree roots are swept when their
  `.aethyme-worktree-root.json` breadcrumb names a repository that no longer
  exists. Roots without a readable breadcrumb are reported, never removed.
- Repositories under the system temporary directory no longer anchor worktrees
  in the implicit platform host-state directory, so test fixtures and scratch
  clones stop leaving permanently unowned trees behind.
- `retention.retained_bytes_budget` adds a soft, non-blocking storage budget.
  It produces warnings only and never authorizes deletion.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. v0.7.4 does not change broker storage,
repository deployment, or engine protocol, so no database backup or repository
migration is required solely for this update.

The GC plan schema moves from 1 to 2. An outstanding
`.aethyme/gc-journal.json` written by an earlier version is refused rather than
misapplied. Finish or discard it first:

```bash
aethyme broker gc plan
aethyme broker gc apply --confirm <digest>
```

## Install or update

Homebrew updates the paired binaries as one unit:

```bash
brew update
brew upgrade aethyme
```

If the formula was pinned, `brew upgrade` reports the pin instead of updating.
Release `brew unpin aethyme` before upgrading.

Installer-managed users should review and confirm the signed manifest plan:

```bash
aethyme update check
aethyme update plan --channel stable
aethyme update execute --confirm <manifest-sha256>
```

## Migrate and verify

No broker-storage or repository-deployment migration is required for v0.7.4.
Verify the installed pair and the current repository contract:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

Both version commands must report `0.7.4`. The upgrade plan should report no
mandatory repository migration for a current v0.7.3 deployment.

## Rollback

Restore both v0.7.3 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol are
unchanged, so no data or repository rollback is required solely because v0.7.4
was installed. Remove any v0.7.4 GC journal first: schema 2 is not readable by
v0.7.3. Never combine binaries from different Aethyme releases.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
- The autonomous build-cache sweep is disabled by default. When enabled it
  reclaims without per-run confirmation, so review `artifact_reclaim_days`
  before opting in.
- Whole-worktree cleanup and GC apply different age gates to the same bytes.
  A worktree that GC reports as blocked by retention age may still be
  reclaimable through `broker cleanup --all-cleaned`.
