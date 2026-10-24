# Durable repository worktree-root integration manifest

**Status:** Landed on main; rollout against existing databases remains pending
**Main commit:** `1091a23059a5ae710900737edfeb7bdc85cdaba5`
**Integration baseline:** `19b40e54a3ff1aceaad5129af55bb2fa59d78a6c`
**Last Updated:** 2026-10-24

This manifest records the source behavior now on main and the boundary between
that implementation and an operator rollout. The source is integrated and its
remote checks passed. No existing user database has been migrated or bound by
this work.

## Baseline retained

- Issue [#697](https://github.com/schiste/Aethyme/issues/697) remains resolved
  by merged PR [#882](https://github.com/schiste/Aethyme/pull/882). Its
  configured-root selection, fallback rules, and storage-layout resolver stay
  in place; the durable binding code builds on that resolver.
- The worktree registry still uses its own root ownership marker and UUID
  recovery. That identity is distinct from the repository UUID marker stored in
  the Git common directory for database binding.
- PR #991's start ordering remains in place: refresh the default branch before
  claiming the worktree lifecycle lock, then prepare the root while holding
  that lock.
- Schema 53 remains the spawned-worktree witness migration. Durable repository
  binding uses the next migration, schema 54.

## Implemented on main

- Schema 54 stores one worktree-root binding per Git common directory. A stable
  repository UUID in the Git common directory selects the repository's row,
  so distinct repositories can share one `AETHYME_BROKER_DB` without
  collapsing their root identities. The binding table does not partition the
  other ledgers stored in that shared database.
- The schema 54 migration is additive. It creates an unscoped
  `legacy_unbound` placeholder when needed; the migration does not inspect the
  filesystem or choose a worktree root. A normal writable open may attach an
  unbound row to the current Git common directory, but that does not create a
  UUID marker or grant root authority.
- Fresh root creation reserves the repository state before publishing the root
  marker and registry record. The root is bound only after the checked files,
  directory identities, and registry state are verified. The reservation and
  binding transitions raise the database's minimum compatible schema to 54.
- Existing legacy roots use an explicit digest-bound binding plan and apply.
  Apply rechecks the selected root and does not move or create it. Interrupted
  fresh binding uses a separate recovery plan and apply that accepts only
  inventoried durable prefixes; unknown data is refused, not removed.
- Submission prepares a fresh root only after submission preflight, under the
  shared lifecycle lock and before queue insertion. Storage planning and apply
  retain bound, reserved, or recovery-required empty roots; apply repeats the
  binding check under that same lock.
- The public binding and recovery commands, JSON fields, and schema behavior
  are documented in [the CLI reference](../reference/cli.md) and
  [the cross-process consumer registry](cross-process-consumers.md).

## Compatibility and rollout boundary

The current schema version is 54 and `MIN_COMPATIBLE_SCHEMA` remains 52.
Migrating an existing schema-52 database to 54 does not by itself raise its
stored floor. Writers that support schema 52 remain compatible while the
repository is unbound. Explicit binding or fresh-root reservation raises the
floor to 54 in the same durable transition, after which writers with a lower
supported schema refuse the database. Keep every writer of a shared database
on the upgraded runtime before applying a binding or starting a fresh-root
reservation.

Before the first writable open of each distinct database, stop its writers and
make a WAL-aware backup. Review the placement command, then inspect the exact
legacy root with `aethyme broker advanced worktree-root binding plan <exact-root>
<keyed_container|direct_root> --json`. Apply only the reviewed digest with
`aethyme broker advanced worktree-root binding apply <exact-root>
<keyed_container|direct_root> --confirm <sha256> --json`. The plan and apply do
not relocate an existing root. For an interrupted fresh reservation, use the
separate `worktree-root recovery plan` and digest-confirmed `recovery apply`
commands documented in the CLI reference.

The additive migration leaves an unbound database open to rollback only while
its stored floor remains 52 and no root-binding transition has happened. After
the floor reaches 54, older writers are refused; there is no in-place schema
downgrade. A rollback after binding requires a coordinated restore of the
database and its matching repository UUID marker, worktree-root marker, and
registry state. Restoring only the database or manually lowering its floor is
not a supported rollback.

## Legacy PR disposition

The replacement implementation landed in the main tree at
`1091a23059a5ae710900737edfeb7bdc85cdaba5`. The old branches are historical
implementation snapshots, not additional migrations to merge:

| PR | Old scope | Current replacement |
| --- | --- | --- |
| [#886](https://github.com/schiste/Aethyme/pull/886) | Initial single-row schema-53 binding design. | Replaced by schema-54 per-repository binding with shared-database support. |
| [#889](https://github.com/schiste/Aethyme/pull/889) | Durable root reservation and exact-prefix recovery. | Implemented by the fresh reservation, binding, and recovery plan/apply paths. |
| [#891](https://github.com/schiste/Aethyme/pull/891) | Fixture updates for bound-root and owner-scoped behavior. | Covered by the current scoped fixtures and root-binding regressions. |
| [#928](https://github.com/schiste/Aethyme/pull/928) | Stacked binding, recovery, lifecycle, and fixture changes. | Root-binding, recovery, and lifecycle behavior is represented on main. Quick-test fixture isolation remains a separate tracked follow-up before closing; do not merge the old stack separately. |
| [#963](https://github.com/schiste/Aethyme/pull/963) | Final per-repository identity and recovery implementation. | Adapted to the schema-54 migration and current lifecycle, storage, and registry contracts. |

Issue #697 remains resolved by #882. Check the current status of each legacy PR
before closing it, and link to the landed implementation rather than merging
its branch.

## Qualification status

The landed main tree's remote workspace run reported 5,752 passed, 0 failed,
and 37 skipped, with the required platform checks passing. Local native
qualification and migration or binding against an existing user database
remain pending; this manifest does not authorize or report a deployment.
