# Upgrading Aethyme

This file has one section for each release that needs more than installing
the new binary pair: a one-way broker database migration, a removed or changed
command, flag, or exit code, or a new install requirement. Every other release
is a drop-in upgrade with unrestricted rollback, and its
[CHANGELOG](CHANGELOG.md) entry is all there is to read.

A CHANGELOG entry that opens with `**Breaking:**` has a section here, and no
other entry does. The `release_contract` test enforces that pairing, and the
GitHub release body for a version is its CHANGELOG entry followed by its
section here, when there is one.

Upgrading across several releases: read every section between your installed
version and the target, oldest first. Each schema migration runs on first open
and is one-way, so rollback is limited to the oldest binary that still reads
the newest schema you have opened.

Breaking releases: [v0.8.2](#v082), [v0.8.1](#v081), [v0.8.0](#v080),
[v0.7.25](#v0725), [v0.7.23](#v0723), [v0.7.22](#v0722), [v0.7.19](#v0719),
[v0.7.18](#v0718), [v0.7.16](#v0716), [v0.7.9](#v079), [v0.7.8](#v078),
[v0.7.4](#v074), [v0.7.2](#v072),
[v0.7.1](#v071), [v0.5.0](#v050), [v0.3.0](#v030),
[v0.2.2](#v022), [v0.2.0](#v020).

## Installing or updating (every release)

The `aethyme` router and `aethyme-engine-cli` are one release unit. Install,
update, and roll back both together, never one alone.

```bash
brew upgrade aethyme                                   # Homebrew
aethyme update check && aethyme update plan            # installer-managed pair
aethyme update execute --confirm <manifest-sha256>
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli     # from a checkout
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Then confirm the pair and review each enrolled repository separately:

```bash
aethyme --version && aethyme-engine-cli --version      # same version
aethyme plugin status                                  # engine pair: matched
aethyme upgrade plan --repo . --diff
```

The broker database is machine-wide. After a release that migrates it, every
older `aethyme` on the machine, including the copies agent plugin hooks run,
refuses with `broker db schema version N is newer than this binary supports`.
Upgrade every installed copy (`which -a aethyme`) before the next session
touches the broker.

## Writing a section (maintainers)

A release needs a section here when it migrates the broker database one way,
removes or changes a command, flag, exit code, or output contract, or adds an
install requirement. Add `## vX.Y.Z` above the previous newest section, with
at least `### Compatibility`, `### Migrate and verify`, and `### Rollback`
(`### Before upgrading` when there is preparation), and start the matching
CHANGELOG entry with:

```markdown
**Breaking:** see [UPGRADING.md](UPGRADING.md#vXYZ).
```

where the anchor is the version with the dots removed. A non-breaking release
changes only CHANGELOG.md. See
[the release guide](packages/aethyme/docs/guides/releasing.md).

Until v0.8.3 every release had its own `upgrading-to-vX.Y.Z.md` guide. The
sections below keep the Compatibility, Before upgrading, Migrate and verify,
and Rollback parts of the breaking ones. The full per-release guides, including
the non-breaking ones, remain in Git history under
`packages/aethyme/docs/guides/`.

## v0.8.2

v0.8.2 hardens the broker: phase 2 of the recovery plan. One change needs
operator action: repositories whose gate commands have never run on this
machine now need `aethyme broker trust` once.

### Compatibility

**No schema migration.** The broker database stays at schema 42.

- Install the CLI and engine pair together.
- Scripts that pass flags a subcommand ignored now get exit 2. Remove those
  flags.
- CI that runs `aethyme broker gates run` on a fresh runner has no trust
  record. Trust the policy there explicitly: this repository sets
  `AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS=1` on that step. That variable is
  meant only for tests and for CI that runs its own repository's gates.

### Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.
- For each repository whose gates have never run on this machine, be ready to
  run `aethyme broker trust --repo <path>` from a terminal.

### Migrate and verify

```
aethyme --version                  # 0.8.2; build_commit matches the release tag
aethyme plugin status              # engine pair: matched
aethyme broker trust status        # per repository
aethyme broker blockers            # what, if anything, blocks this repository
```

### Rollback

Unrestricted. Reinstall v0.8.1; it reads the same schema-42 database. Trust
records under host state are ignored by 0.8.1.

## v0.8.1

v0.8.1 fixes two regressions that v0.8.0 introduced. Both were found in real
use within hours of the release.

### Compatibility

**No schema migration.** The broker database stays at schema 42. v0.8.1,
v0.8.0 and v0.7.25 binaries can open the same database.

Install the CLI and engine pair together. Scripts that treated exit 4 as
"gate failed" now see 6 for host failures. Retry those after freeing the
resource instead of changing code.

### Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.

### Migrate and verify

Nothing to migrate.

```
aethyme --version          # 0.8.1; build_commit matches the release tag
aethyme plugin status      # engine pair: matched
```

### Rollback

Unrestricted. Reinstall v0.8.0. It reads the same schema-42 database, and the
exit codes and refusal text revert with the binary.

## v0.8.0

v0.8.0 is a correctness and integrity release. It changes exit codes and what
a submission is judged by, hence the minor version. No command or flag was
removed, and the broker database schema is unchanged.

### Compatibility

**No schema migration.** The broker database stays at schema 42, so v0.8.0 and
v0.7.25 binaries can open the same database.

Install the CLI and engine pair together. Agents or scripts that branch on exit
code 1, or that declare `--effect read` for unrecognized commands, need the
changes above.

### Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.
- If a session's branch edits `.aethyme/gates.toml` and expects its own
  submission to run the new gates, land that policy change separately first.

### Migrate and verify

Nothing to migrate.

```
aethyme --version          # 0.8.0; build_commit matches the release tag
aethyme plugin status      # engine pair: matched
aethyme broker status      # opens the database normally
```

### Rollback

Unrestricted. Reinstall v0.7.25; it reads the same schema-42 database. The
exit codes and the base-tree policy revert with the binary.

## v0.7.25

### Compatibility

**Broker database schema 41 → 42.** This release adds the `session_scopes`
table. Migrations are append-only and there is no downgrade: once a database has
been opened by this release, an older `aethyme` binary cannot read it and will
refuse rather than guess.

Install the CLI and engine pair together, and before other agent sessions or
plugin hooks on the same machine next use the broker.

No command, flag or output was removed. `--claim` is new; `--scope` continues to
name the resource of a coordinated operation.

### Before upgrading

- Finish or close in-flight sessions, or be ready to reinstall before they next
  run a broker command.
- Confirm no other agent on this machine is mid-submission:
  `aethyme broker status --json`.

### Migrate and verify

The migration runs the first time the new binary opens the database.

```
aethyme --version          # build_commit should match the release tag
aethyme broker status      # opens the migrated database
```

A session started after upgrading reports its scope, and `status --json`
carries `scope_overlaps` once two live sessions name one target.

### Rollback

The schema migration cannot be undone. To return to v0.7.24, reinstall that
version **and** restore a copy of `.aethyme/broker.db` taken before the upgrade;
an older binary will refuse a migrated database rather than operate on it.

Take that copy before upgrading if a rollback path matters to you.

## v0.7.23

This release strengthens cleanup and installation safety, isolates gate
databases, and improves navigation when an optional graph is unavailable.

### Compatibility

- There is no new broker database migration relative to v0.7.22: schema 41
  remains current. Upgrades from older versions must also follow the
  [v0.7.22 migration guidance](#v0722).
- The shell installer now requires jq. Signature verification additionally
  requires cosign; use a reviewed installer file with --verify-signature.
- Install the router and engine together from the same release. Let running
  gates finish before changing the installed pair.
- No repository graph enrollment or graph materialization is required to use
  the new navigation hints.

### Before upgrading

Let running gates finish, record both installed binary versions, and keep a
backup of broker coordination state. If upgrading from v0.7.21 or earlier,
review the one-way schema migration described in the v0.7.22 guide first.

### Migrate and verify

No migration command is needed from v0.7.22. After installing the pair:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker status
aethyme certify
```

Both version commands should report 0.7.23. Check that existing sessions and
their worktrees remain visible; do not recreate or delete broker state to
silence a diagnostic.

### Rollback

v0.7.22 and v0.7.23 use the same broker database schema. If rollback is
necessary, stop active gates and reinstall both v0.7.22 binaries together.
This also removes the cleanup and gate-isolation fixes in this release.
Do not downgrade to v0.7.21 against a schema-41 database: restore a compatible
pre-upgrade backup or remain on a schema-41-capable binary.

## v0.7.22

v0.7.22 is the first release since v0.7.19 that migrates the broker database.
Read its Compatibility notes below before installing it on a machine with
live sessions: the migration is one-way and older binaries cannot open a
migrated database.

### Compatibility

- **The broker database moves from schema 39 to 41**, applying two migrations:
  - **40** rebuilds `gate_results` to admit the new `build_failure` class.
    Every row and index is carried over.
  - **41** adds `repository_name`, `tab_name` and `ai_provider` to `sessions`.
    All three are nullable and existing rows are left untouched.
- **The migration runs on first open.** The first v0.7.22 binary to touch a
  broker database migrates it, and that binary may be a gate's build rather
  than an installed one.
- **v0.7.21 binaries then refuse that database** with
  `broker db schema version 41 is newer than this binary supports (39)`. Plugin
  hooks fail the same way, and a hook that swallows the exit code goes quiet
  rather than erroring. A binary built between the two migrations reports the
  intermediate number; the remedy is the same, which is to install the pair
  from this release rather than to touch the database.
- The engine protocol remains version 1.
- Repository layout and `.aethyme/` configuration are unchanged from v0.7.21.

Unlike the v0.7.20 to v0.7.21 window, live sessions **are** affected here.

### Before upgrading

1. Let running gates finish. A gate interrupted mid-migration is not dangerous,
   but its session will need the new binary before it can continue.
2. Note which machines share this repository's broker database. Upgrading one
   agent upgrades the database for all of them.
3. Record the version you are on, so rollback is a decision rather than a guess:

   ```bash
   aethyme --version
   ```

### Migrate and verify

The migration needs no command; it runs when the broker next opens its
database. Confirm the result:

```bash
aethyme --version
aethyme broker status
aethyme certify
```

`broker status` returning normally means the database is on schema 41 and the
installed binary speaks it. To see that gate history survived the rebuild:

```bash
aethyme broker metrics --json
```

### Rollback

**Rollback is restricted in this release.** Reinstalling v0.7.21 gives you a
binary that cannot open a database v0.7.22 has already migrated — the usual
"check out the previous tag and reinstall" leaves you with a broker that
refuses to start.

If you have not yet run any v0.7.22 binary against a database, rollback is
ordinary:

```bash
git checkout v0.7.21
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

If the database has already been migrated, you must either stay on v0.7.22 or
restore the database from a backup taken before the upgrade. There is no
downgrade migration. Committed work is never at risk either way: sessions,
branches and worktrees are Git objects, and only the broker's own coordination
state lives in the database.

## v0.7.19

v0.7.19 is the broker reliability and graph-indexing follow-up release. It
gives abandoned sessions a terminal path, makes cleanup budget-aware, keeps
review facts and waivers explicit, and strengthens gate and GitHub-write
protection. It also fixes JavaScript and TypeScript graph indexing for both
quoted fetch-worker registrations and avoids false middleware facts from bare
mentions.

This release migrates the broker database from schema 35 to 39. Read
**Rollback** before upgrading.

### Compatibility

The broker database migrates from schema 35 to 39:

- v36 records the base commit needed for per-dimension review freshness.
- v37 adds the explicit `waived` review state.
- v38 separates review request facts from completion facts and records the
  completion verdict and reviewer identity.
- v39 adds release tracking for closed-session pins and an `expired` state for
  old publication exposures.

The migration runs automatically when a v0.7.19 binary first opens the
database, and it is one-way. The broker database is machine-wide: upgrade
every installed copy of both `aethyme` and `aethyme-engine-cli`, including
copies found earlier on `PATH`, before using the migrated database.

### Before upgrading

Record in-flight work and back up the broker database:

```bash
aethyme broker status
cp .aethyme/broker.db .aethyme/broker.db.pre-0.7.19
which -a aethyme aethyme-engine-cli
```

If the database is shared by several checkouts, make the backup from the
checkout that owns the machine-wide broker state.

### Migrate and verify

Migration happens on first open. Verify the pair and the repository after
installation:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme certify
aethyme broker status
```

Review routing remains inert unless the repository opts into its review tables.
Use `aethyme broker review plan --pr <number>` to inspect a decision without
performing a review or writing to GitHub.

### Rollback

**Constrained.** A v0.7.18 binary cannot open a database at schema 39, and
there is no down-migration. Restore the pre-upgrade database together with the
older binary pair:

```bash
aethyme broker status
cp .aethyme/broker.db.pre-0.7.19 .aethyme/broker.db
brew install schiste/tap/aethyme@0.7.18
# or install both 0.7.18 binaries from the checkout
```

Rows and operations recorded after the migration are lost when the backup is
restored. If no backup exists, keep every binary on v0.7.19 rather than
mixing an older binary with the schema-39 database.

## v0.7.18

v0.7.18 is the review-routing release. A repository can now decide which
reviews a change needs, hand each one to an agent in its own throwaway
checkout, show the state on the pull request, and close the row when the
reviewer reports. Every part of it is off by default.

It also carries the fix for a `git` wrapper that made every clean worktree on
a machine read dirty, which had been silently disabling cleanup.

This release migrates the broker database from schema 32 to 35. Read
**Rollback** before upgrading.

### Compatibility

**The broker database migrates from schema 32 to 35** — `review_requests`
(v33), its rebuild with the uniqueness constraint that makes an interrupted
`review run` re-runnable (v34), and `pull_request_observations` (v35).

The migration runs automatically the first time a v0.7.18 binary opens the
database, and it is one-way. The broker database is machine-wide, so **the
moment any v0.7.18 binary opens it, every older `aethyme` on the machine is
locked out** with a schema-too-new error — including the copies the agent
plugin's hooks invoke. Upgrade every copy on the machine, not just the first
one `PATH` resolves.

Nothing else changes behaviour. With the review tables absent or disabled, the
router performs no GitHub writes, spawns nothing, and leaves pull requests
untouched.

### Before upgrading

Account for every copy of the pair on this machine. A tap install and a
`cargo install` shadow each other, and only the one earliest on `PATH` runs:

```bash
which -a aethyme aethyme-engine-cli
```

Back up the broker database, because the schema migration is one-way and this
is the only way back to 32:

```bash
cp .aethyme/broker.db .aethyme/broker.db.pre-0.7.18
```

Finish or record any in-flight sessions first:

```bash
aethyme broker status
```

### Migrate and verify

Migration happens on first open. Confirm the version and that the database
came up:

```bash
aethyme --version
aethyme certify
aethyme broker status
```

Review routing stays inert until you opt in. To see what it would do without
performing anything:

```bash
aethyme broker review plan --pr <number>
```

That command takes no session and writes nothing. It names both the tree it
read policy from and the tree it read the change from — policy comes from the
main checkout, never from the branch under review, so a pull request cannot
alter the rules that judge it.

### Rollback

**Constrained.** The schema migration is one-way: a v0.7.17 binary cannot open
a database at schema 35, and there is no down-migration. Rolling back the
binary alone leaves every `aethyme` command failing against the machine's
broker state.

To roll back, restore the database alongside the binary:

```bash
aethyme broker status                      # note anything in flight
cp .aethyme/broker.db.pre-0.7.18 .aethyme/broker.db
brew install schiste/tap/aethyme@0.7.17    # or cargo install --version 0.7.17
```

Sessions, operations, and review rows recorded under 0.7.18 are lost in that
restore. If you have no backup, the remaining option is to stay on 0.7.18 for
the broker and pin the older binary out of `PATH`.

The agent plugin can stay installed across a rollback; only the review
subcommands disappear.

## v0.7.16

v0.7.16 lets a session whose work landed through a provider-side squash or
rebase merge be recorded and closed. It carries a broker schema change, 31 to
32, and no change to the generated agent policy.

### Compatibility

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

### Before upgrading

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

### Migrate and verify

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

### Rollback

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

## v0.7.9

v0.7.9 makes promote commits credit the agent that produced the change, not
only the broker that applied it. It migrates broker storage from schema 30 to
schema 31.

### Compatibility

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

### Before upgrading

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

### Migrate and verify

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

### Rollback

There is no downgrade path once the schema migration has run. Restoring the
v0.7.8 pair leaves both binaries unable to open a schema-31 database.

To return to v0.7.8, restore both binaries together through the original
installation manager **and** restore `.aethyme/broker.db` from a copy taken
before the upgrade. Promote commits already written keep their trailers; they
are ordinary commit metadata and need no rollback. Never combine binaries from
different Aethyme releases.

## v0.7.8

v0.7.8 adds repository readiness reporting with digest-confirmed remediation,
makes queued coordinated operations visible and bounded, and repairs two ways
the coordinator could stall or mislead. It also migrates broker storage from
schema 28 to schema 30.

### Compatibility

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

### Before upgrading

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

### Migrate and verify

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

### Rollback

There is no downgrade path once the schema migration has run. Restoring the
v0.7.7 pair leaves both binaries unable to open a schema-30 database.

To return to v0.7.7, restore both binaries together through the original
installation manager **and** restore `.aethyme/broker.db` from a copy taken
before the upgrade. Repository deployment files are compatible in both
directions and need no rollback. Never combine binaries from different Aethyme
releases.

## v0.7.4

v0.7.4 makes broker storage account for itself. Garbage collection now reports
the disk it is holding rather than only the bytes it will free, reclaims
git-ignored build caches independently of commit provenance, and sweeps
worktree roots whose owning repository no longer exists. Every reclaim remains
digest-confirmed, and the one unconfirmed path is opt-in and limited to build
caches.

### Compatibility

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

### Before upgrading

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

### Migrate and verify

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

### Rollback

Restore both v0.7.3 binaries together through the original installation
manager. Broker storage, repository deployment, and engine protocol are
unchanged, so no data or repository rollback is required solely because v0.7.4
was installed. Remove any v0.7.4 GC journal first: schema 2 is not readable by
v0.7.3. Never combine binaries from different Aethyme releases.

## v0.7.2

v0.7.2 adds durable pull-request activity observation and a provider-neutral
delivery outbox. Delivery clients such as Chau7 can notify the exact agent
session without becoming responsible for GitHub polling, deduplication,
authorization, or retry state.

### Compatibility

| Contract | v0.7.2 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 28; writes schema 28 |
| Repository deployment | schema 1; no mandatory migration from v0.7.1 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

### Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. If rollback to v0.7.1 must remain
possible, make a recoverable copy of `.aethyme/broker.db` while no broker
command is running. The first v0.7.2 broker command upgrades it to schema 28,
which v0.7.1 cannot open.

No generated repository file must change merely to install v0.7.2. Inspect any
repository-owned migration separately and locally:

```bash
aethyme upgrade plan --repo . --diff
```

### Migrate and verify

The broker database migration is transactional and runs automatically when a
v0.7.2 broker command first opens the repository. Repository deployment stays
at schema 1, so apply no repository write unless an upgrade plan proposes an
exact reviewed diff.

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

To connect a delivery client, create a PR watch, then subscribe an adapter to
the returned watch ID. Adapter targets are opaque to Aethyme and interpreted
only by that delivery client:

```bash
aethyme broker watch pr start --session <session-id> \
  --repo owner/name --pr <number> --events comments,reviews,checks
aethyme broker deliveries subscribe --watch <watch-id> \
  --adapter <adapter> --target <opaque-target> --policy notify
```

### Rollback

Restore both v0.7.1 binaries together through the original installation
manager and restore the pre-upgrade `.aethyme/broker.db` copy. A v0.7.1 broker
cannot open storage already migrated to schema 28. Never combine a v0.7.1
router with a v0.7.2 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
need no rollback unless a separate digest-confirmed repository migration was
explicitly applied.

## v0.7.1

v0.7.1 makes closed-session and checkpoint recovery explicit. It also adopts
the maintainer's patch-only default release rule: automation increments only
the third version component unless the maintainer explicitly chooses a minor
or major version.

### Compatibility

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

### Before upgrading

Finish active sessions when practical and confirm both installed executables
come from the same installation manager. If rollback to v0.7.0 must remain
possible, make a recoverable copy of `.aethyme/broker.db` while no broker
command is running. The first v0.7.1 broker command upgrades it to schema 25.

No generated repository file must change merely to install v0.7.1. Inspect any
repository-owned migration separately and locally:

```bash
aethyme upgrade plan --repo . --diff
```

### Migrate and verify

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

### Rollback

Restore both v0.7.0 binaries together through the original installation
manager. A v0.7.0 broker cannot open a database already migrated to schema 25;
restore the pre-upgrade `.aethyme/broker.db` copy before using the older broker,
or keep v0.7.1 for broker operations. Never combine a v0.7.0 router with a
v0.7.1 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
need no rollback unless a separate digest-confirmed repository migration was
explicitly applied.

## v0.5.0

v0.5.0 is a broker coordination and publication release. It makes submission,
promotion, publication, review, external automation, resource routing, and
cleanup evidence explicit and durable. The release does not require a
repository deployment migration, but it does migrate broker storage.

### Compatibility

| Contract | v0.5.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 24; writes schema 24 |
| Repository deployment | schema 1; no mandatory migration from v0.4.2 |
| Release channel | `stable` |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

### Before upgrading

Finish active sessions when practical. Confirm both installed executables come
from the same installation manager. If rollback to v0.4.2 must remain possible,
make a recoverable copy of `.aethyme/broker.db` while no broker command is
running; v0.5.0 upgrades that database to schema 24 on first open.

No generated repository file must change merely to install v0.5.0. To review
whether the installed binary proposes any repository-owned update, use the
read-only plan before applying anything:

```bash
aethyme upgrade plan --repo . --diff
```

### Migrate and verify

The broker database migration is transactional and runs automatically when a
v0.5.0 broker command first opens the repository. Repository deployment remains
schema 1, so do not run a write migration unless the upgrade plan proposes one
and its exact digest has been reviewed.

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
aethyme enhance verify --repo .
aethyme upgrade plan --repo . --diff
```

If the upgrade plan contains intentional repository changes, review the exact
diff and apply only its current digest using the command printed by the plan.

### Rollback

Restore both v0.4.2 binaries together through the original installation
manager. A v0.4.2 broker cannot open a database already migrated to schema 24;
restore the pre-upgrade `.aethyme/broker.db` copy before using the older broker,
or keep v0.5.0 for broker operations. Do not combine a v0.4.2 router with a
v0.5.0 engine sibling.

Repository deployment and engine protocol are unchanged, so repository files
do not need to be rolled back unless an explicit repository upgrade was
separately reviewed and applied.

## v0.3.0

v0.3.0 completes the broker stabilization, transactional repository-upgrade,
cross-clone coordination, durable advisory, and reproducible onboarding work
that followed v0.2.2. The router and engine remain one paired installation;
repository policy is still upgraded explicitly in each enrolled repository.

### Compatibility

| Contract | v0.3.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same build |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 17; writes schema 17 |
| Repository deployment | schema 1; generated policy may require regeneration |
| Release channel | `stable` for a final `v0.3.0` release |

The signed release manifest binds these values to the exact source SHA and to
the sizes and SHA-256 digests of both binaries in every archive.

### Before upgrading

Finish active sessions and inspect unresolved operations before replacing the
binary pair. Back up broker history when rollback matters:

```bash
aethyme broker status --json
aethyme broker operations list --status unknown --json
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /safe/location/broker.db.v0.2.2
```

Opening a repository with v0.3.0 may migrate broker storage through schema 17.
Older binaries cannot reopen a newer database, so restoring the older pair
also requires restoring its compatible database backup.

Do not stash shared multi-worktree state. Commit through an eligible pinned
session, finish active sessions, or leave unrelated dirty work in place while
reviewing an exact repository migration plan.

### Migrate and verify

The machine-wide update never scans for repositories. Enter each enrolled
repository and review its pure migration plan before applying anything:

```bash
cd /path/to/repo
aethyme upgrade plan --repo . --diff
aethyme upgrade plan --repo . --json
aethyme upgrade apply --repo . --confirm <plan-sha256>
aethyme deploy verify --repo .
git diff --check
git diff
```

An already-current repository produces no migration write. If generated
onboarding needs refreshing, run `aethyme enhance deploy --repo .`, review the
tracked output, and verify again. Canonical and activated local-only
deployments must retain their existing mode.

Finally smoke the installed runtime and disposable broker lifecycle:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
```

### Rollback

1. Stop the engine daemon.
2. Restore the previous router and engine binaries together using the
   installer rollback bundle, Homebrew, or a pinned source checkout.
3. Restore the broker database backup compatible with that binary.
4. Recover or revert any applied repository migration; never edit the schema
   marker by hand.
5. Verify both binary versions and run `aethyme broker quick-test`.

Binary rollback does not reverse committed repository policy.

## v0.2.2

v0.2.2 adds host-wide resource leases for concurrent validation, an opt-in
pre-push adapter, and explicit embedded migrations for Aethyme-owned
repository policy. It is the first release that distinguishes updating the
machine-wide binary pair from updating each enrolled repository.

If upgrading from v0.1.x, first read the broader
[v0.2.0 migration guide](#v020), which covers the native Rust
cutover and paired-binary installation model.

### Compatibility

| Contract | v0.2.2 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same archive |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 8; writes schema 8 |
| Repository deployment | schema 1; explicit migration required for older enrolled repositories |
| Release channel | `stable` through GitHub's latest non-prerelease release |

The signed release manifest records these compatibility values, the exact
source SHA, and every archive size and SHA-256 digest.

### Before upgrading

Finish or record active broker work, stop the engine daemon, and back up each
broker database whose history must survive rollback:

```bash
aethyme broker status
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /safe/location/broker.db.v0.2.1
```

Opening a repository with v0.2.2 migrates broker storage from schema 7 to 8.
v0.2.1 cannot reopen schema 8, so its database backup is required for a full
rollback.

Commit or stash repository changes before migrating repository policy.
`aethyme upgrade apply` deliberately refuses a dirty worktree.

### Migrate and verify

The binary updater never searches for repositories or changes them
implicitly. Enter every enrolled canonical repository and review its embedded
migration:

```bash
cd /path/to/repo
aethyme upgrade plan
aethyme upgrade apply --confirm <plan-sha256>
git diff --check
git diff
aethyme deploy verify --repo .
```

Review and commit the migration output, including
`.aethyme/repository.json`, with the generated policy files. Until that commit
lands, other clones remain on the previous repository contract and v0.2.2
broker commands will direct them to migrate.

For an activated local-only bridge, keep the migration clone-local:

```bash
aethyme upgrade plan --local-only
aethyme upgrade apply --local-only --confirm <plan-sha256>
aethyme deploy verify --local-only --repo .
```

Finally verify the installed pair and disposable broker workflow:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test
```

### Rollback

1. Stop the engine daemon.
2. Restore both v0.2.1 binaries together through Homebrew, the installer's
   retained rollback bundle, or a detached v0.2.1 source checkout.
3. Restore the v0.2.1 broker database backup before running broker commands.
4. For canonical deployment, revert the repository migration commit before
   using v0.2.1 in that repository.
5. Verify both versions and run `aethyme broker quick-test`.

Binary rollback does not automatically reverse a committed repository
migration. Do not hand-edit the repository schema marker to simulate a
rollback.

## v0.2.0

v0.2.0 is the first release with paired prebuilt binaries, a signed release
manifest, standalone checksums, and a stable install/update channel. It also
completes the native Rust cutover: `python -m src.cli` is intentionally gone
and has no compatibility shim.

### Compatibility

| Contract | v0.2.0 |
| --- | --- |
| Supported platforms | Apple Silicon macOS, Intel macOS, x86-64 Linux |
| Required executables | `aethyme`, `aethyme-engine-cli` from the same archive |
| Minimum Git | 2.38 |
| Engine daemon protocol | 1 |
| Broker storage | reads/migrates schemas 1 through 7; writes schema 7 |
| Release channel | `stable` through GitHub's latest non-prerelease release |

The manifest records these values alongside archive sizes and SHA-256 digests.
A newer broker can migrate an older supported database on first open. That is
forward compatibility, not a promise that an older binary can reopen the
migrated database.

### Before upgrading

Finish or record active work, stop any engine daemon for each repository, and
back up broker state before the first v0.2.0 broker command:

```bash
aethyme broker status
aethyme-engine-cli daemon stop --repo /path/to/repo
cp /path/to/repo/.aethyme/broker.db /path/to/repo/.aethyme/broker.db.pre-v0.2.0
```

Repeat the database backup for every broker-managed repository that must be
rollback-safe. The graph store does not need a backup: `.aethyme/graph_store.redb`
is a derived local artifact rebuilt from committed `.aethyme/graph/` fragments.

### Migrate and verify

Verify the pair first, then open each managed repository:

```bash
aethyme --version
aethyme-engine-cli --version
aethyme broker quick-test

cd /path/to/repo
aethyme certify
aethyme broker status
```

Opening broker state applies supported schema migrations transactionally. If
an existing redb graph store uses an incompatible file format or schema,
rebuild the derived store without changing committed graph fragments:

```bash
aethyme-engine-cli index --repo /path/to/repo
```

The index command detects incompatible redb formats, replaces only the derived
`.aethyme/graph_store.redb`, and leaves `.aethyme/graph/` untouched.

### Rollback

Binary rollback and state rollback are separate operations.

1. Stop the v0.2.0 engine daemon.
2. Restore both earlier binaries together. Installer-managed updates report
   the retained rollback bundle; otherwise use backups or check out the
   earlier tag and run both `cargo install --locked --path ...` commands.
3. If v0.2.0 opened and migrated a broker database, restore that repository's
   `broker.db.pre-v0.2.0` before running the older broker.
4. Rebuild `.aethyme/graph_store.redb` with the restored engine if graph queries
   report an incompatible store.
5. Verify both versions and run `aethyme broker quick-test`.

The v0.2.0 installer can pin any release that publishes the new manifest
contract. v0.1.x predates that contract, so rolling back to v0.1.x requires
saved binaries or a source checkout.
