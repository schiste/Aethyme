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

Breaking releases: [v0.8.44](#v0844), [v0.8.43](#v0843), [v0.8.42](#v0842), [v0.8.41](#v0841), [v0.8.40](#v0840), [v0.8.38](#v0838), [v0.8.37](#v0837), [v0.8.33](#v0833), [v0.8.31](#v0831), [v0.8.29](#v0829), [v0.8.28](#v0828), [v0.8.27](#v0827), [v0.8.26](#v0826), [v0.8.25](#v0825), [v0.8.23](#v0823), [v0.8.8](#v088), [v0.8.2](#v082), [v0.8.1](#v081), [v0.8.0](#v080),
[v0.7.25](#v0725), [v0.7.23](#v0723), [v0.7.22](#v0722), [v0.7.19](#v0719),
[v0.7.18](#v0718), [v0.7.16](#v0716), [v0.7.9](#v079), [v0.7.8](#v078),
[v0.7.4](#v074), [v0.7.2](#v072),
[v0.7.1](#v071), [v0.5.0](#v050), [v0.3.0](#v030),
[v0.2.2](#v022), [v0.2.0](#v020).

## v0.8.44

### Compatibility

The repository broker database advances to schema 58 with compatibility floor 58. This protocol fence blocks fresh opens by older binaries; it does not revoke connections they already hold. Older runtimes cannot safely coordinate the new artifact-use and unattended reclamation protocol. Optional retirement report fields are additive and may be absent in saved reports.

The default fifteen-day closed-checkout grace is unchanged. Broader in-place cleanup is default-off and uses the existing disposable ignored-directory policy; ignored directories can contain datasets or saved records, so preserve irreplaceable material outside disposable checkouts. Standalone ignored files remain protected. Protocol-dependent unattended cleanup remains blocked on unsupported platforms.

### Migrate and verify

Before the new runtime first opens a live database, take consistent backups and preserve repository configuration. For every enrolled repository affected by the shared router/engine update, temporarily set `[retention]` settings `session_abandoned_after_hours = 0`, `auto_cleanup_worktrees_on_finish = false`, and `artifact_sweep_budget_ms = 0` in `.aethyme/broker.toml`, plus `[cleanup]` settings `strict = true`, `auto_remove = false`, and `ignored_directory_sweep = false` in `.aethyme/config.toml`. Finish-time cleanup is independent of `auto_remove`. Inspect `.aethyme/gc-journal.json` before the first normal open: startup can resume a previously confirmed GC plan independently of these automatic-maintenance holds. Resolve any pending journal through the reviewed GC recovery workflow before proceeding.

Install the matching signed router and engine pair. Let every older client with an open broker database connection finish, and verify process identity plus database, WAL and SHM handles; absence of a gate PID file is insufficient. Only after the floor is committed and the old-client drain is proved, restore the reviewed retention/cleanup settings and optionally enable `ignored_directory_sweep = true`. Preserve concurrent configuration edits. Inspect typed maintenance deferrals and confirm active work remains protected. See `packages/aethyme/docs/guides/automatic-checkout-maintenance.md` for the policy details.

### Rollback

Do not lower the compatibility marker or run older binaries against the migrated live database. Keep maintenance held if any completion evidence is uncertain. Prefer the matching current runtime. Any reviewed restoration of a pre-upgrade backup must account for activity recorded since that backup; never replace a live database or erase newer work to bypass the fence.

## v0.8.43

### Compatibility

Recovery plan JSON advances to schema 3, with canonical `repository_path`, nullable `branch_tip` and `landing.state: not_evaluated_for_recovery`. The latter is not a delivery verdict. Clean recovery plans without direct ancestry proof report `unproven_provenance`, including potentially squash/replay-delivered work, rather than asserting that it is pending or landed. Its versioned content digest excludes mutable delivery observations; regenerate previously saved confirmations after upgrading. Recovery archives advance to schema 4. Older readers retain unsupported archives as unowned; current readers preserve support for archive schemas 1, 2 and 3 and their frozen digest encodings. No repository broker database migration is added by this release.

The default grace period for state-only closed checkouts is now 360 hours (15 days). The separate age-retention setting is unchanged. Explicit overrides remain effective. Age alone never authorizes deletion of unique or unverified content, and adopted checkout ownership remains protected.

### Migrate and verify

Install the matching router and engine pair and verify both released identities. Follow the v0.8.42 host operations ledger migration requirements when upgrading from an older runtime. Replan recovery actions with the new binary, inspect preserved content and archive location, and check `worktree_removed` plus `cleanup_error` before reporting a removal.

### Rollback

Earlier binaries do not understand schema-4 archives or the new confirmation digest. Preserve those archives and use a compatible runtime for verification and recovery; do not rewrite schema markers. The v0.8.42 host operations ledger rollback constraints continue to apply.

## v0.8.42

### Compatibility

The per-user host operations ledger adopts schema 2 when opened by the new operation coordinator. Existing rows and unresolved outcomes are preserved. Earlier binaries require schema 1 and refuse this newer ledger; the schema change fences writers that do not understand parent/resource crash barriers. This is separate from the host resources ledger, which already uses schema 2, and does not change the repository broker database schema.

Eligible strict Git operations use narrower locks; unknown commands retain conservative locking. Ordinary startup and sync fetches do not automatically adopt the strict fetch profile. Fetch witnesses establish a provider result only when all recorded evidence can be revalidated; process disappearance or a repeated fetch does not prove the original outcome. Added JSON evidence and nullable holder fields must not be interpreted as permission to bypass an unresolved operation.

### Migrate and verify

Before using new coordinated operations, let active old clients finish and update every installation sharing the host operations ledger to the matching v0.8.42 router and engine pair. Back up the ledger with a consistent SQLite backup after quiescing writers. Verify both binaries report v0.8.42 and the same released source commit. Changing a runtime selector does not update already-running clients. Preserve and resolve existing unknown operations through their normal inspected recovery workflow; do not delete the ledger to unblock work.

### Rollback

After the host operations ledger has adopted schema 2, older binaries cannot use it. Prefer returning to the matching current pair. A reviewed rollback requires stopping all clients and restoring a consistent pre-upgrade backup only after accounting for operations recorded since that backup; blindly replacing the ledger can erase crash barriers for real remote mutations. Do not downgrade its schema marker by hand. Other previous-release compatibility limits still apply.

## v0.8.41

### Compatibility

New broker-created worktrees record spawn witness schema 2, and new finish cleanup recovery proofs use schema 3. Both bind the checkout to a random generation marker created at spawn. Current maintenance reads legacy records but does not use records missing that marker to authorize unattended removal or finish-recovery resumption. For those unattended paths, missing, changed or unsupported generation evidence retains the checkout. Cleanup never backfills ownership onto an existing directory.

Earlier strict readers can reject the new records. Use the matching current runtime for automatic maintenance of newly created worktrees. This release adds no broker database migration or compatibility-floor change; it does not make old running maintenance processes safe or update their behavior in place.

The additive `broker commit` command uses a session-local lock while preserving hooks and before/after ownership checks. Existing `broker advanced git` commands retain their current coordination behavior. The new command's JSON field `command_ran` is true when launch is confirmed and null when launch cannot be confirmed; null must not be treated as proof that nothing ran.

### Migrate and verify

Install the matching router and engine pair and verify v0.8.41 and the same source commit at every active installation root. Restart maintenance workers on the updated pair before relying on generation-bound cleanup. Already-running old binaries remain old; switching an installation does not upgrade their mapped code. Preserve checkouts with legacy or incomplete generation evidence for reviewed recovery.

### Rollback

Stop maintenance workers before rollback. Earlier runtimes may reject new witness and recovery-proof records and do not implement the current generation checks. Preserve these records and their checkouts; return to a compatible current pair for automatic cleanup or recovery. Database rollback is not required by this release, and previous release compatibility limits still apply.

## v0.8.40

### Compatibility

`aethyme broker submit prepare --json` now reports schema version 2. For a
shared-cache rotation, `bytes_before` is null when the bounded size inventory
stops at its time or entry limit; `inventory_complete`,
`observed_bytes_lower_bound`, and `rotation_reason` explain the evidence.
Strict JSON consumers must accept null and handle both inventory-limit reasons
before using this report. Preparation configuration and saved-state schemas
remain version 1, and no broker database migration is introduced.

### Migrate and verify

Update strict wrappers to accept report schema 2, preserve the distinction
between exact `bytes_before` and its observed lower bound, and handle
`inventory_time_limit` and `inventory_entry_limit`. Verify the matching
router and engine report v0.8.40 and the same source commit.

### Rollback

Earlier runtimes continue to emit report schema 1 and do not include the new
inventory evidence. Use wrappers compatible with the installed runtime; no
database or preparation-config rollback is required.

## v0.8.38

### Compatibility

New recovery archives use manifest schema 3, which records bundle length and
SHA-256 and requires bounded bundle capture. This runtime continues to read
schema 1 and 2 archives. Earlier runtimes may classify schema 3 archives as
unowned; preserve them and use a compatible reader for inventory and retention.
No broker database migration or compatibility-floor change is added.

Closed-session artifact cleanup now takes an exclusive per-checkout guard
against participating managed producers. Busy or unknown producer state retains
the candidate. This cooperative guard does not cover unmanaged external writers
or authorize artifact reclamation from open sessions.

### Migrate and verify

Install the matching router and engine pair and verify that both report
v0.8.38 and the same source commit. Existing archives need no manual conversion.
Keep source checkouts when capture refuses its bounds, integrity checks or
storage admission. The free-space estimate is not a total archive quota or a
guarantee against concurrent disk writes.

### Rollback

Stop maintenance workers before rollback. Older runtimes do not implement the
current cooperative artifact-removal guard and may not read schema 3 recovery
archives. Keep those archives intact and return to a compatible newer reader
to manage them. The earlier v0.8.37 and v0.8.31 compatibility limits still apply.

## v0.8.37

### Compatibility

New recovery archives use manifest schema 2, which preserves protected ignored
files and binds them to the reviewed cleanup plan and policy. This runtime reads
complete schema 1 and 2 archives. Earlier runtimes, including v0.8.36, reject
schema 2 archives as unowned and cannot manage their inventory or retention.
This release adds no broker database migration or compatibility-floor change.

Cleanup now retains a checkout when patch-equivalent commits do not also carry
its exact changed-path contents, modes and types. Recovery capture refuses
oversized patches or unknown or insufficient destination capacity and leaves
the source checkout in place. Its capacity check is a known-minimum estimate,
not a total archive quota or a guarantee that the reserve remains available.

### Migrate and verify

Install the matching router and engine pair, then verify that both report
v0.8.37 and the same source commit. Preserve existing recovery archives; no
manual conversion is needed. Use the new runtime for archive inventory and
retention after it has created a schema 2 archive.

Managed Unix preparation, gates and guarded execution require durable
artifact-use records on the repository filesystem. An I/O or directory-sync
failure refuses admission rather than silently disabling that protection.
These records do not authorize automatic cleanup of an open checkout or prove
that direct external commands have stopped writing.
On non-Unix targets, this protocol reports admission unavailable; ordinary
commands continue without participating, and that result grants no artifact
cleanup authority.

### Rollback

A schema-compatible older pair can still read the broker database, subject to
the earlier v0.8.31 limits. Keep schema 2 archives intact: an older runtime's
unowned classification does not make them disposable. Return to v0.8.37 or a
compatible newer reader to manage them. Stop maintenance workers before
rollback; older clients do not implement the new artifact-use tracking or
stronger landing-proof checks. Do not delete unknown markers to force cleanup.

## v0.8.33

### Compatibility

Ignored directories are disposable by default

Every Git-ignored directory inside a broker session checkout is now eligible for
disposal when the checkout otherwise passes ownership, delivery, source-cleanliness
and liveness checks. This includes ignored datasets, saved session records and
unknown folders, regardless of their names or nested contents. Unknown standalone ignored
files outside recognized artifact or broker-runtime paths remain protected.

### Migrate and verify

To preserve every ignored leftover, set this in the primary checkout's
`.aethyme/config.toml` **before** using the new runtime:

```toml
[cleanup]
strict = true
```

Upgrade every client and background worker before relying on strict mode; older
binaries do not implement this new policy. Verify the router and engine both
report v0.8.33.

Strict mode also pauses automatic artifact sweeps; it does not prevent explicit
reviewed artifact cleanup or force-discard operations. Move irreplaceable records
outside disposable checkouts, or enable strict mode before upgrading. The primary
checkout and live work remain protected. No database migration or compatibility
floor change is introduced by this release; the v0.8.31 schema guidance still
applies when upgrading from older versions.

### Rollback

This release introduces no database migration or new compatibility floor. A prior
schema-compatible binary can still read the database, but it does not implement
strict cleanup. Before rollback, stop cleanup workers and preserve irreplaceable
ignored records outside disposable checkouts; remove the new `strict` key only
once its retention guarantee is no longer required. Follow the v0.8.31 rollback
limits for databases previously migrated or bound by that release.

## v0.8.31

This release includes the durable worktree-root and guarded-execution changes
previously documented as unreleased, plus additive incident and helper records.
Install the matching router and engine as one pair.

### Compatibility

Schema 54 introduced the per-repository worktree-root binding table and, when
needed, one unscoped `legacy_unbound` row. A normal writable open may attach
that row to the current Git common directory. The v54 migration does not
inspect the filesystem, choose a worktree root, create a repository UUID
marker, or bind a root. Existing session, operation, and queue rows are
retained.

The broker schema is version 57. The v55 additive migration creates
observational receipts for guarded `broker exec`; receipts do not authorize
process cleanup or session transitions and contain no command, environment, or
output data. This migration does not raise `MIN_COMPATIBLE_SCHEMA`. Schemas
56 and 57 add observational base-failure incidents and direct-child
helper outcomes; neither changes gate authority or raises the global floor. A
schema-52-compatible writer can continue to use a schema-57 database while its
stored compatibility floor remains 52 and no other feature has raised that
floor.
Explicitly binding a legacy root or reserving a fresh root raises the stored
floor to 54 in the same durable state transition. After that transition,
writers that support only schemas below 54 refuse the database. When
repositories share one `AETHYME_BROKER_DB`, each Git common directory has its
own binding row, but the compatibility floor applies to the shared database.

A placement or binding plan can open the database and therefore may apply
pending migrations and attach its unbound row, even though the plan does not
create or bind a worktree root. Verify the actual database version and stored
compatibility floor during each deployment; the release version alone does not
establish either value.

### Before migrating or binding

Resolve the database path for every repository, including any
`AETHYME_BROKER_DB` override. Linked worktrees use their repository's common
database; independent repositories that point at one path share a database.
Before its first writable open, stop all processes that can write each distinct
database and make a WAL-aware backup. Install the same router and engine pair
wherever that database can be opened, before applying a migration or root
binding. Preserve the repository's Git common directory, selected worktree
root, and host worktree registry; the binding workflow does not relocate them.

### Migrate and verify

The first normal writable open applies pending migrations through v57. It does
not bind a legacy root. Inspect the selected placement with
`aethyme broker advanced worktree-root --json`. For an existing legacy root,
review `aethyme broker advanced worktree-root binding plan <exact-root>
<keyed_container|direct_root> --json`, then apply only its displayed digest:

```bash
aethyme broker advanced worktree-root binding apply <exact-root> \
  <keyed_container|direct_root> --confirm <sha256> --json
```

The plan and apply refer to the root already on disk; they do not create or
move it. If a fresh reservation was interrupted, review
`aethyme broker advanced worktree-root recovery plan --json` and use the
exact reported digest with:

```bash
aethyme broker advanced worktree-root recovery apply \
  --confirm <sha256> --json
```

Recovery refuses unknown files, marker contents, registry entries, or database
sidecar states. It does not discard unknown files, reset the database, or
select a different root. Do not repair a refusal by editing markers or binding
rows by hand.

For repositories with committed graph authority, update the committed engine
pin and regenerate fragments through the new pair's reviewed `graph refresh
plan` / `graph refresh execute --confirm <sha256>` workflow. Do not rewrite
fragments solely to suppress a version mismatch. Repositories with graph
authority disabled do not need a fragment refresh.

### Rollback

Before a root-binding transition, a migrated database can be opened by an
older writer with schema-52 support that honors `min_compatible_schema` while
the stored floor remains 52, provided no other feature has raised it. Schema
54 has no down migration. After binding or fresh-root reservation raises the
floor to 54, older binaries are refused; do not lower the floor or drop the
binding table to force a downgrade.

After a binding transition, rollback requires a coordinated restore with
writers stopped: restore the database from a pre-transition backup together
with its corresponding repository UUID marker, worktree-root marker, and host
registry state. Restoring only the database is not a supported rollback. That
recovery procedure has not been run against an existing user database as part
of this release-note preparation.

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

Broker state is scoped to a repository: by default its database is
`<repo>/.aethyme/broker.db`, shared by linked worktrees. `AETHYME_BROKER_DB`
can redirect that path, and repositories pointed at the same path share one
database. A schema migration affects only the database it opens. After that
database's compatibility floor rises, older binaries refuse that database.
Before its first writable open, upgrade every installed Aethyme copy that can
access the same database. Databases at other paths are unaffected until they
migrate. Host operation and resource ledgers are separate.

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

## v0.8.29

These notes cover upgrading from v0.8.26. An earlier patch or development
installation whose broker database is already at schema 52 needs no additional
migration.

v0.8.29 migrates each repository's broker database from schema 49 to schema 52
on its first writable open. By default, this is
`<repo>/.aethyme/broker.db`; linked worktrees use their primary checkout's
database. A configured `AETHYME_BROKER_DB` may redirect it, including to a
path shared by multiple repositories. Each distinct database path migrates
independently; the host operation and resource ledgers are separate and are
not changed by this migration.

### Compatibility

The migration adds typed verification-candidate storage, establishes the
required collaboration-capture fence, and adds a gate execution-profile
digest. The minimum compatible schema is 52. After one database has migrated,
v0.8.26 and earlier cannot open that database. Upgrade every installed Aethyme
copy that can access the same database before any one of those copies opens it;
install the router and engine pair together. A repository whose database has
not migrated remains on its current schema.

### Before upgrading

Cleanup plans and new pending cleanup journals use schema 4 with a stable
retention anchor. Existing schema-3 journals remain resumable. Before rolling
back to an older development binary that reads broker schema 52, finish any
pending schema-4 cleanup with v0.8.29, or use its `broker gc abandon --confirm
<pending-digest>` command to archive the journal without deleting retained data.
Then review a fresh cleanup plan with the selected binary; do not reuse an old
plan digest across versions.

For each repository whose broker database must remain rollback-safe, stop
broker processes that can access it and make a separate, WAL-aware SQLite
backup. The SQLite `.backup` command includes committed WAL content:

```bash
sqlite3 "/path/to/repo/.aethyme/broker.db" ".backup /safe/backups/repo-broker.db.pre-0.8.29"
```

If `AETHYME_BROKER_DB` redirects that repository's database, back up the
configured file instead. Record the installed router and engine versions.
Upgrade every installed copy that can access the same database to the matching
v0.8.29 pair before resuming broker work against it.

### Migrate and verify

From each repository that has migrated, confirm the pair and broker state:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Before a repository's database has migrated, reinstalling the v0.8.26 pair
needs no database restore. After that database reaches schema 52, v0.8.26
refuses it. To roll back that repository, stop every broker process that can
access its database, reinstall the router and engine pair together, and restore
that repository's own pre-upgrade backup. Broker state recorded after the
backup is lost; Git commits and worktrees are unaffected. Repeat independently
for each migrated repository database.

## v0.8.28

The v0.8.27 tag produced no published release artifacts. These notes cover
upgrading from the last published release, v0.8.26. A development installation
whose broker database is already at schema 52 needs no additional migration.

v0.8.28 migrates each repository's broker database from schema 49 to schema 52
on its first writable open. By default, this is
`<repo>/.aethyme/broker.db`; linked worktrees use their primary checkout's
database. A configured `AETHYME_BROKER_DB` may redirect it, including to a
path shared by multiple repositories. Each distinct database path migrates
independently; the host operation and resource ledgers are separate and are
not changed by this migration.

### Compatibility

The migration adds typed verification-candidate storage, establishes the
required collaboration-capture fence, and adds a gate execution-profile
digest. The minimum compatible schema is 52. After one database has migrated,
v0.8.26 and earlier cannot open that database. Upgrade every installed Aethyme
copy that can access the same database before any one of those copies opens it;
install the router and engine pair together. A repository whose database has
not migrated remains on its current schema.

### Before upgrading

Cleanup plans and new pending cleanup journals use schema 4 with a stable
retention anchor. Existing schema-3 journals remain resumable. Before rolling
back to an older development binary that reads broker schema 52, finish any
pending schema-4 cleanup with v0.8.28, or use its `broker gc abandon --confirm
<pending-digest>` command to archive the journal without deleting retained data.
Then review a fresh cleanup plan with the selected binary; do not reuse an old
plan digest across versions.

For each repository whose broker database must remain rollback-safe, stop
broker processes that can access it and make a separate, WAL-aware SQLite
backup. The SQLite `.backup` command includes committed WAL content:

```bash
sqlite3 "/path/to/repo/.aethyme/broker.db" ".backup /safe/backups/repo-broker.db.pre-0.8.28"
```

If `AETHYME_BROKER_DB` redirects that repository's database, back up the
configured file instead. Record the installed router and engine versions.
Upgrade every installed copy that can access the same database to the matching
v0.8.28 pair before resuming broker work against it.

### Migrate and verify

From each repository that has migrated, confirm the pair and broker state:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Before a repository's database has migrated, reinstalling the v0.8.26 pair
needs no database restore. After that database reaches schema 52, v0.8.26
refuses it. To roll back that repository, stop every broker process that can
access its database, reinstall the router and engine pair together, and restore
that repository's own pre-upgrade backup. Broker state recorded after the
backup is lost; Git commits and worktrees are unaffected. Repeat independently
for each migrated repository database.

## v0.8.27

v0.8.27 migrates each repository's broker database from schema 49 to schema 52
on its first writable open. By default, this is
`<repo>/.aethyme/broker.db`; linked worktrees use their primary checkout's
database. A configured `AETHYME_BROKER_DB` may redirect it, including to a
path shared by multiple repositories. Each distinct database path migrates
independently; the host operation and resource ledgers are separate and are
not changed by this migration.

### Compatibility

The migration adds typed verification-candidate storage, establishes the
required collaboration-capture fence, and adds a gate execution-profile
digest. The minimum compatible schema is 52. After one database has migrated,
v0.8.26 and earlier cannot open that database. Upgrade every installed Aethyme
copy that can access the same database before any one of those copies opens it;
install the router and engine pair together. A repository whose database has
not migrated remains on its current schema.

### Before upgrading

For each repository whose broker database must remain rollback-safe, stop
broker processes that can access it and make a separate, WAL-aware SQLite
backup. The SQLite `.backup` command includes committed WAL content:

```bash
sqlite3 "/path/to/repo/.aethyme/broker.db" ".backup /safe/backups/repo-broker.db.pre-0.8.27"
```

If `AETHYME_BROKER_DB` redirects that repository's database, back up the
configured file instead. Record the installed router and engine versions.
Upgrade every installed copy that can access the same database to the matching
v0.8.27 pair before resuming broker work against it.

### Migrate and verify

From each repository that has migrated, confirm the pair and broker state:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Before a repository's database has migrated, reinstalling the v0.8.26 pair
needs no database restore. After that database reaches schema 52, v0.8.26
refuses it. To roll back that repository, stop every broker process that can
access its database, reinstall the router and engine pair together, and restore
that repository's own pre-upgrade backup. Broker state recorded after the
backup is lost; Git commits and worktrees are unaffected. Repeat independently
for each migrated repository database.

## v0.8.26

v0.8.26 makes `broker submit` refuse a session whose checkout has drifted from its recorded branch, and rewords the `finish` timeout message. Scripts that submitted from a drifted checkout, or matched the old timeout text, must adapt.

### Compatibility

**No schema migration.** The broker database stays at schema 49, and v0.8.25 still opens it. Changed behaviour:

- **`submit` refuses a drifted checkout.** When a session's worktree is no longer on its recorded branch (a different branch checked out, or a detached HEAD such as a stuck rebase leaves), `submit` refuses before queueing and again after planning; no queue entry is left. A session adopted on a detached checkout records `HEAD` and is unaffected. `status --refresh` reports the session with the blocked advice id `session.checkout-drift`.
- **`finish` timeout wording.** The message now starts `finish timed out after <n>s waiting on Git (` instead of `… while checking Git`, and says whether the session is still open (retry with `--timeout`) or already closed (do not retry).
- **Additive:** `status --json` and `status --summary --json` may carry a `waiters[]` array; it is omitted when nothing waits.

### Migrate and verify

Before upgrading, finish or repair sessions whose checkout has drifted: switch the worktree back to its recorded branch, or complete or abort the rebase in progress. Then `submit` again. Readers of `advice[].id` must tolerate `session.checkout-drift`, and anything matching the `finish` timeout text must match the new wording. Then install the router and engine pair together, and verify:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --refresh --json
aethyme broker advanced quick-test
```

### Rollback

Reinstall the v0.8.25 router and engine pair together. It reads the same schema-49 database, accepts drifted checkouts at `submit` again, and restores the old `finish` timeout wording.

## v0.8.25

v0.8.25 bounds broker commands that used to wait as long as they needed. Scripts that relied on an unbounded `status` inspection or an unbounded `finish` must adapt.

### Compatibility

**No schema migration.** The broker database stays at schema 49, and v0.8.24 still opens it. Changed behaviour:

- **`status` inspection budget.** `broker status`, `status doctor`, `broker advanced worktrees` and `gc plan` stop their Git inspection at 7 s (was 15 s for `status`). `AETHYME_STATUS_INSPECTION_BUDGET_MS` can only lower that budget; a larger value is ignored. What was not inspected is reported, never assumed clean: `deferred_checks`, `not_inspected_sessions[]`, rows with `state: "not_inspected"` (counted in `unique_work_count`), and `budget_verdict: "unknown"` in a partial `gc plan`.
- **`finish` deadline.** `broker finish` gives its closing Git checks 10 s by default and then stops with the timed-out check named, keeping the session. Pass `--timeout <seconds>` (1 to 86400) for a repository whose checks are slower.

### Migrate and verify

A script that raised `AETHYME_STATUS_INSPECTION_BUDGET_MS` to get a complete answer should instead treat `deferred_checks` and `not_inspected` rows as "unknown" and re-run, or use the unbounded command the notice names (for drift, `integration reconcile --upstream <ref> --dry-run`). A script that called `finish` on a very large repository should pass `--timeout`. Then install the router and engine pair together, and verify:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Reinstall the v0.8.24 router and engine pair together. It reads the same schema-49 database and restores the unbounded `status` budget override and `finish`.

## v0.8.23

v0.8.23 tightens what the broker lets through on shared branches and on `gh` writes. Scripts that relied on the old, looser behaviour must adapt before upgrading.

### Compatibility

**No schema migration.** The broker database stays at schema 47. Changed behaviour:

- **Brokered `gh` writes.** Through `aethyme broker advanced gh`, only an exact allowlist runs without acknowledgement:
  - comments on issues and pull requests;
  - review comments and replies;
  - reviews and review events;
  - labels;
  - issue edits;
  - the porcelain `pr comment`, `pr review`, `pr edit`, `pr ready` and `issue …` commands.

  `pr merge <N> --merge|--squash|--rebase [-d]` is checked against session ownership, and `--match-head-commit` is injected. Any other command that may write or delete a branch ref is refused with exit 3 unless `--destructive --ref-write-acknowledged` is given. That includes `gh api` with an unlisted endpoint, GraphQL mutations, `browse`, `config`, extensions, downloads to a chosen directory, and `pr create` without `--head`. `gh` now runs from `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` or `/bin`, with a fixed `PATH` and environment.
- **Shared branches.** `main`, resolved from `origin/HEAD`, and `aethyme/integration` can no longer be deleted, force-updated or rebased through `broker advanced git|gh`. There is no override.
- **Adopting the main checkout.** `broker start --adopt` in a repository's main checkout is refused. Use `broker start` for a fresh worktree, or `--allow-main-checkout` when the operator really means it. Re-entering an existing session there still works.

### Migrate and verify

Replace scripted `gh api` writes with allowlisted forms. Use the literal `repos/<owner>/<name>/…` path, never `{owner}/{repo}`. Otherwise add the acknowledgement flags where the operator approved the write. Then install the router and engine pair together, and verify:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Reinstall the v0.8.22 router and engine pair together. It reads the same schema-47 database and restores the earlier `gh` and shared-branch behaviour.

## v0.8.8

v0.8.8 removes deprecated broker and top-level command spellings. Scripts and
shell aliases must use the current commands before upgrading.

### Compatibility

**No schema migration.** The broker database remains at schema 43, and
v0.8.7 recovery journals remain readable. Removed spellings exit with status 2
and print their replacement. The complete old-to-current mapping is in the
[CLI reference](packages/aethyme/docs/reference/cli.md#broker-verbs-and-removed-spellings).

### Migrate and verify

Update scripts that call deprecated spellings using the migration table in
the CLI reference.
Install the router and engine pair together, then verify:

```bash
aethyme --version && aethyme-engine-cli --version
aethyme plugin status
aethyme broker status --json
aethyme broker advanced quick-test
```

### Rollback

Reinstall the v0.8.7 router and engine pair together. It supports the same
schema-43 database and restores the deprecated spellings. Do not roll back only
one binary.

## v0.8.2

v0.8.2 hardens the broker: phase 2 of the recovery plan. One change needs
operator action: repositories whose gate commands have never run on this
machine now need `aethyme broker advanced trust` once.

### Compatibility

**No schema migration.** The broker database stays at schema 42.

- Install the CLI and engine pair together.
- Scripts that pass flags a subcommand ignored now get exit 2. Remove those
  flags.
- CI that runs `aethyme broker advanced gates run` on a fresh runner has no trust
  record. Trust the policy there explicitly: this repository sets
  `AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS=1` on that step. That variable is
  meant only for tests and for CI that runs its own repository's gates.

### Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.
- For each repository whose gates have never run on this machine, be ready to
  run `aethyme broker advanced trust --repo <path>` from a terminal.

### Migrate and verify

```
aethyme --version                  # 0.8.2; build_commit matches the release tag
aethyme plugin status              # engine pair: matched
aethyme broker advanced trust status        # per repository
aethyme broker unblock            # what, if anything, blocks this repository
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
aethyme broker advanced metrics --json
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
Use `aethyme broker advanced review plan --pr <number>` to inspect a decision without
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
aethyme broker advanced review plan --pr <number>
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
aethyme broker advanced representation scan --session <id>
aethyme broker advanced representation record --session <id> --confirm <digest>
aethyme broker finish close --session <id>
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
aethyme deploy verify --repo .
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
aethyme deploy verify --repo .
aethyme broker quick-test
aethyme broker status --json
aethyme broker status readiness
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
aethyme deploy verify --repo .
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
aethyme deploy verify --repo .
aethyme upgrade plan --repo . --diff
```

To connect a delivery client, create a PR watch, then subscribe an adapter to
the returned watch ID. Adapter targets are opaque to Aethyme and interpreted
only by that delivery client:

```bash
aethyme broker advanced watch pr start --session <session-id> \
  --repo owner/name --pr <number> --events comments,reviews,checks
aethyme broker advanced deliveries subscribe --watch <watch-id> \
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
aethyme deploy verify --repo .
aethyme upgrade plan --repo . --diff
```

For a lifecycle whose owner was already closed, inspect it with `review show`,
then choose either exact-head `review reassign` or explicit `review abandon`.
For rewritten contribution checkpoints, start with
`broker advanced checkpoint plan --session <id> --json` and follow its ordered actions.

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
aethyme deploy verify --repo .
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
aethyme broker advanced operations list --status unknown --json
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
onboarding needs refreshing, run `aethyme deploy --repo .`, review the
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
