# Upgrading to Aethyme v0.7.16

Last Updated: 2026-09-09

v0.7.16 lets a session whose work landed through a provider-side squash or
rebase merge be recorded and closed. It carries a broker schema change, 31 to
32, and no change to the generated agent policy.

## What is new

A pull request merged with squash produces a commit with a new SHA and no
ancestry link to the session's own commits. Ancestry therefore reports that the
work never landed, and the session stays blocked at `broker finish` forever --
its work is on the default branch, and nothing can prove it.

Representation answers from content instead:

```bash
aethyme broker representation scan --session <id>
aethyme broker representation record --session <id> --confirm <digest>
```

`scan` is read-only. It takes the paths the session changed, walks the commits
the default branch gained since the session branched, and reports the earliest
commit whose content matches every one of them. That bound is a fact rather
than a budget: work cannot have landed before the session branched.

`record` stores the commit it found. `broker finish` then treats a recorded
representation as delivery evidence, and the session closes normally.

Recording is also attempted opportunistically after a merge performed through
`broker gh`, so most sessions never need the manual lane.

## Why content, and not the obvious alternatives

**Not ancestry.** A squash rewriting the SHA is precisely the thing that breaks
ancestry, so the check that fails is not the one to lean on harder.

**Not the branch tip.** Comparing a session's content against the current tip
decays the moment an unrelated commit rewrites a shared file: work that was
represented yesterday stops being represented today, through no change of its
own. The commit that carried the work is a fact that stays true, which is why
the representing commit is stored rather than recomputed.

**Not operator assertion.** A human declaring the work landed is the claim being
verified, not evidence for it.

## Compatibility

The broker database migrates from schema 31 to 32 on first write, adding the
`session_representations` table. The migration is additive: no existing table
or column changes, and no row is rewritten.

The `session.finished` handoff payload gains a `representing_commit` field.
This is an additive change to the frozen v1 event contract, so
`EVENTS_SCHEMA_VERSION` is unchanged and existing readers keep working.

A binary older than v0.7.16 cannot open a migrated database and refuses with
`broker db schema version 32 is newer than this binary supports (31)`. This is
the one way this release can interrupt you, so upgrade every local install
before the first write.

## Before upgrading

Account for every copy of the pair on this machine. A tap install and a
`cargo install` shadow each other, and only the one earliest on `PATH` runs:

```bash
which -a aethyme aethyme-engine-cli
```

If more than one appears, upgrade all of them or unlink the ones you do not
want. A stale copy is harmless until the database migrates, and broken
immediately afterwards -- including for anything non-interactive, such as a
launchd PR-monitoring job.

Sessions already blocked by the problem this release fixes need no preparation.
Their state is intact and recoverable after upgrading.

## Install or update

The router and its engine sibling are one release unit -- never install one
without the other.

```bash
cargo install --path packages/aethyme/rust/crates/aethyme-cli --force
cargo install --path packages/aethyme/rust/crates/aethyme-engine --force
# or, from the tap
brew update && brew upgrade aethyme
```

## Migrate and verify

The migration runs on the first write, not on install. Confirm both halves
report the new version, then confirm the broker still reads its state:

```bash
aethyme --version
aethyme broker status
```

Recover any session that was blocked before the upgrade. `scan` is read-only,
so it is safe to run first and read before recording anything:

```bash
aethyme broker representation scan --session <id>
aethyme broker representation record --session <id> --confirm <digest>
aethyme broker close --session <id>
```

Confirm the schema moved only after a write has occurred:

```bash
sqlite3 .aethyme/broker.db "SELECT value FROM meta WHERE key='schema_version'"
```

## Rollback

Unlike recent releases, rollback here is constrained: a 0.7.15 binary cannot
open a database that has already migrated to schema 32. Reinstalling the older
pair alone is not sufficient.

If no representation has been recorded, the migration is an empty additive
table and rolling the schema back is safe. Back up first, then:

```bash
cp .aethyme/broker.db .aethyme/broker.db.bak
sqlite3 .aethyme/broker.db "DROP INDEX IF EXISTS session_representations_head;
                            DROP TABLE IF EXISTS session_representations;
                            UPDATE meta SET value='31' WHERE key='schema_version';"
```

Then reinstall the previous pair. Verified non-destructive: no other table is
touched and no session row is rewritten.

If representations **have** been recorded, rolling back discards them. The
sessions they unblocked become unclosable again, exactly as before the upgrade.
Nothing else is lost, but prefer staying on 0.7.16 and reporting the problem.

## Known issues

`cargo test --workspace` migrates the developer's live `.aethyme/broker.db`,
because the broker tests invoke the freshly built binary and its metric
recording resolves the main checkout rather than the session worktree. On a
branch that adds a migration this locks the installed binary out mid-
development. This affects contributors, not users of a released build. Tracked
as [#163](https://github.com/schiste/Aethyme/issues/163); the rollback above is
the recovery.

## Fixed

- `broker finish` no longer refuses a session solely because ancestry cannot
  see its work.
