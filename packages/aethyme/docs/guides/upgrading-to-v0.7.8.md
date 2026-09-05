# Upgrading to Aethyme v0.7.8

Last Updated: 2026-09-06

v0.7.8 adds repository readiness reporting with digest-confirmed remediation,
makes queued coordinated operations visible and bounded, and repairs two ways
the coordinator could stall or mislead. It also migrates broker storage from
schema 28 to schema 30.

## Compatibility

| Contract | v0.7.8 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 30; writes schema 30 |
| Repository deployment | schema 1; no mandatory migration from v0.7.7 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

### Readiness reporting and remediation

`aethyme broker readiness` reports whether a repository is conflict-only,
agent-ready, or parallel-ready. `--require <level>` exits non-zero below the
requested level, which suits CI. Remediation follows the same plan/apply
contract as the rest of Aethyme:

```bash
aethyme broker readiness plan --diff
aethyme broker readiness apply --confirm <plan-sha256>
```

`readiness recover --plan <sha256>` resumes an interrupted apply, and
`aethyme broker gates doctor [--probe]` diagnoses gate configuration.

### Bounded and visible coordinated operations

A coordinated operation is now recorded before it queues for the repository
write lock, so a waiting command appears in `aethyme broker operations list`
for the whole wait rather than only once it acquires the lock. A blocked caller
is told which operation holds the lock and for how long. Two new flags bound
the wait; waiting indefinitely remains the default:

```bash
aethyme broker gh --session <id> --repo <owner/name> --no-wait -- <gh-args>
aethyme broker git --session <id> --queue-timeout 120 -- <git-args>
```

An identical command still pending for the same session is refused rather than
queued behind the first, so a re-issued command cannot fire twice against state
the first attempt already changed.

### Host resources no longer stall on a dead holder

A holder that died without releasing used to pin its pool until the lease TTL
elapsed, which on a pool consumed entirely by one run blocked every gate on the
machine across all sessions. A provably absent holder is now quarantined
immediately and its **capacity** units stop counting toward pool occupancy.
Namespaces and exclusive keys still stay reserved until reconciliation proves
cleanup, because those name real artifacts that may hold residue.

## Before upgrading

**Upgrade every Aethyme installation that shares a repository, together.**
Broker storage moves from schema 28 to schema 30. The migration is applied in
place the first time a v0.7.8 binary opens the database, and it is
forward-only. v0.7.7 and earlier then refuse every broker command with

```
broker db schema version 30 is newer than this binary supports (28); upgrade aethyme
```

This matters wherever more than one installation touches the same repository:
several agent sessions, a second checkout, or a locally built binary alongside
the released one. Read-only inspection of the database file with an external
SQLite client is unaffected.

Finish active sessions when practical, verify the installation manager owns
both binaries, and take a copy of `.aethyme/broker.db` if you may need to
return to v0.7.7. No repository deployment migration is required.

## Install or update

Homebrew updates both binaries as one formula transaction:

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

Refresh repository-owned deployment files and confirm the coordinator is
healthy on the migrated database:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme deploy --repo .
aethyme enhance verify --repo .
aethyme broker quick-test
aethyme broker status --json
aethyme broker readiness
```

Both version commands must report `0.7.8`. `broker status` must return without
a schema error, which confirms the migration to schema 30 succeeded. If a
previously stuck host-resource pool was being held by a dead process, it clears
on the next acquisition rather than at lease expiry.

## Rollback

There is no downgrade path once the schema migration has run. Restoring the
v0.7.7 pair leaves both binaries unable to open a schema-30 database.

To return to v0.7.7, restore both binaries together through the original
installation manager **and** restore `.aethyme/broker.db` from a copy taken
before the upgrade. Repository deployment files are compatible in both
directions and need no rollback. Never combine binaries from different Aethyme
releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- Coordinated operations serialize per repository, and the lock is held across
  the wrapped command's local hooks. A slow `pre-push` gate therefore delays
  every other coordinated operation on that repository; use `--queue-timeout`
  or `--no-wait` to avoid parking behind one.
- A dead holder's namespace and exclusive-key allocations still require
  `aethyme broker resources reconcile <lease-id> --confirm <generation>`; only
  capacity units are reclaimed automatically.
- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
