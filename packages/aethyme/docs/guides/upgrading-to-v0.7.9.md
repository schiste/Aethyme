# Upgrading to Aethyme v0.7.9

Last Updated: 2026-09-06

v0.7.9 makes promote commits credit the agent that produced the change, not
only the broker that applied it. It migrates broker storage from schema 30 to
schema 31.

## Compatibility

| Contract | v0.7.9 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 31; writes schema 31 |
| Repository deployment | schema 1; no mandatory migration from v0.7.8 |
| Graph cache schema | 1; derived and safe to remove |
| Release channel | `stable` |

The signed release manifest binds these contracts and every archive digest to
the exact source SHA.

## What changed

A promote commit used to name only the broker as both author and committer, so
the agent that did the work disappeared from the record. The three claims are
now split:

- the **human** stays the commit author,
- the **broker** becomes the committer,
- the **agent** is named in a `Co-Authored-By` trailer, with the broker repeated
  as a trailer because the committer field is invisible in most log views.

Identity comes from `--agent "<Name> <email>"` on `start` or `adopt`, or from
the `AETHYME_AGENT` environment variable, and is stored per session:

```bash
aethyme broker start --task "..." --agent "Your Name <you@example.com>"
aethyme broker adopt --task "..." --agent "Your Name <you@example.com>"
```

It is resolved once, in the agent's own process, because promotion may later run
from a different one -- another session's `submit`, or a queue drain -- where
the environment would credit whoever triggered the merge. A session that
identifies no agent is omitted from that credit rather than guessed at.

## Before upgrading

**Upgrade every Aethyme installation that shares a repository, together.**
Broker storage moves from schema 30 to schema 31. The migration is applied in
place the first time a v0.7.9 binary opens the database, and it is
forward-only. v0.7.8 and earlier then refuse every broker command with

```
broker db schema version 31 is newer than this binary supports (30); upgrade aethyme
```

This applies to any second installation that touches the same repository,
including a locally built binary alongside the released one -- building from
source and running one broker command is enough to migrate the shared database
for everything else on the machine.

Finish active sessions when practical, verify the installation manager owns both
binaries, and take a copy of `.aethyme/broker.db` if you may need to return to
v0.7.8. No repository deployment migration is required.

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

```bash
aethyme --version
aethyme-engine-cli --version
aethyme deploy --repo .
aethyme enhance verify --repo .
aethyme broker quick-test
aethyme broker status --json
```

Both version commands must report `0.7.9`. `broker status` must return without a
schema error, which confirms the migration to schema 31 succeeded. To see the
new attribution, start a session with `--agent` and inspect the promote commit
it produces:

```bash
git log -1 --format='%an <%ae>%n%cn <%ce>%n%b' aethyme/integration
```

## Rollback

There is no downgrade path once the schema migration has run. Restoring the
v0.7.8 pair leaves both binaries unable to open a schema-31 database.

To return to v0.7.8, restore both binaries together through the original
installation manager **and** restore `.aethyme/broker.db` from a copy taken
before the upgrade. Promote commits already written keep their trailers; they
are ordinary commit metadata and need no rollback. Never combine binaries from
different Aethyme releases.

## Known issues

- Graph support remains opt-in. Cold refresh and one-file refresh are still
  expensive on very large repositories; Aethyme itself remains graph-disabled.
- A large graph may use hundreds of MiB for committed fragments, each private
  redb, and the optional host cache. Cache retention is not yet automatic.
- Schema migration is implicit: the first v0.7.9 binary to open the database
  migrates it, with no confirmation and no warning to other installations.
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
