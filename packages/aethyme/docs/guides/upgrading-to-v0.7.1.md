# Upgrading to Aethyme v0.7.1

Last Updated: 2026-09-01

v0.7.1 makes closed-session and checkpoint recovery explicit. It also adopts
the maintainer's patch-only default release rule: automation increments only
the third version component unless the maintainer explicitly chooses a minor
or major version.

## Compatibility

| Contract | v0.7.1 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 25; writes schema 25 |
| Repository deployment | schema 1; no mandatory migration from v0.7.0 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

## What changed

- `broker review reassign` transfers an inactive lifecycle to a live session
  only when its exact HEAD still matches the bound review commit.
- `broker review abandon` retires an inactive lifecycle without erasing its
  state, evidence, or generations, allowing a later fresh registration.
- Closed-session lease and review mutations fail before persistence or provider
  access; diagnostic review inspection stays available.
- Checkpoint plans return typed refusal codes and ordered recovery actions.
  Unsafe recovery preserves the exact session tip before graph inspection and
  clean replay; it does not prescribe a blanket integration rebase.
- `broker repair` now refuses outside its conflict-repair scope and points to
  the checkpoint planner when that is the correct recovery lane.

## Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. If rollback to v0.7.0 must remain
possible, make a recoverable copy of `.aethyme/broker.db` while no broker
command is running. The first v0.7.1 broker command upgrades it to schema 25.

No generated repository file must change merely to install v0.7.1. Inspect any
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

The broker database migration is transactional and runs automatically when a
v0.7.1 broker command first opens the repository. Repository deployment stays
at schema 1, so apply no repository write unless an upgrade plan proposes an
exact reviewed diff.

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

For a lifecycle whose owner was already closed, inspect it with `review show`,
then choose either exact-head `review reassign` or explicit `review abandon`.
For rewritten contribution checkpoints, start with
`broker checkpoint plan --session <id> --json` and follow its ordered actions.

## Rollback

Restore both v0.7.0 binaries together through the original installation
manager. A v0.7.0 broker cannot open a database already migrated to schema 25;
restore the pre-upgrade `.aethyme/broker.db` copy before using the older broker,
or keep v0.7.1 for broker operations. Never combine a v0.7.0 router with a
v0.7.1 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
need no rollback unless a separate digest-confirmed repository migration was
explicitly applied.

## Known issues

- Windows and Linux ARM archives are not published.
- Update checks are explicit; Aethyme performs no background network request.
- Homebrew installations must be upgraded through Homebrew.
- The broker-storage migration is forward-only; downgrade requires the
  pre-upgrade database copy described above.
- Review reassignment deliberately refuses a destination session at a
  different commit; use abandonment and fresh registration after reviewing
  that divergence.
