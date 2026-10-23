# Durable repository worktree-root integration manifest

**Status:** Integration in progress
**Baseline:** `19b40e54a3ff1aceaad5129af55bb2fa59d78a6c` (main, 2026-10-23)
**Last Updated:** 2026-10-23

This manifest records which parts of the durable worktree-root PR stack are
already present on main, which behavior remains to integrate, and when the
older PRs can be closed. It is an implementation record, not a qualification
report.

## Already covered on main

- Issue [#697](https://github.com/schiste/Aethyme/issues/697) is closed as
  completed by merged PR [#882](https://github.com/schiste/Aethyme/pull/882).
  Its configured-root planning, root fallback, and storage-layout resolution
  are present. Do not reopen #697 or copy the #882 resolver over current code.
- Main already has repository worktree-root UUID marker planning and recovery
  in `worktree_registry` and the storage GC command. This repository-root
  recovery remains separate from broker database identity and binding state.
- PR #991 is part of the baseline. In `Broker::start_worktree`, the default
  branch refresh runs before the worktree lifecycle claim, and root preparation
  runs under that claim. Keep this ordering and its lifecycle coordination
  hooks when integrating older root code.
- Broker database schema version 53 is occupied by the spawned-worktree
  instance witness migration. The new binding migration must follow it; do not
  reuse schema version 53 or replace that migration.

## Behavior still to integrate

The current broker store has no per-repository worktree-root binding table or
repository-binding modules. It therefore cannot durably associate multiple
repositories with one explicitly shared `AETHYME_BROKER_DB` while preserving
independent repository identity and root state. The missing package includes
stable identity through the Git common directory, binding lookup and
confirmation, safe recovery of interrupted identity publication, and durable
fresh-root reservation/recovery.

The single-binding design from #886 is incompatible with the documented
shared-database override. Use the per-repository identity and binding design
from the final #963 tree, including the explicit existing-database recovery
path and fresh-root recovery hardening where they fit the current interfaces.
Port the required behavior onto current main rather than importing #963's
whole branch history.

## PR disposition

Keep these PRs open until one replacement integration PR is merged and its
required source CI is green:

| PR | Role in the old stack | Replacement coverage required before closure |
| --- | --- | --- |
| [#886](https://github.com/schiste/Aethyme/pull/886) | Original single-binding design | Per-repository bindings with shared-database compatibility; do not retain its incompatible single-row model. |
| [#889](https://github.com/schiste/Aethyme/pull/889) | Durable-root binding and recovery follow-up | Root reservation, binding persistence, and recovery behavior integrated on current main. |
| [#891](https://github.com/schiste/Aethyme/pull/891) | CLI fixture and owner-scoping follow-up | Preserve the still-relevant owner-scoped fixture assertions in the replacement tests; do not cherry-pick the branch wholesale. |
| [#928](https://github.com/schiste/Aethyme/pull/928) | Stacked root and fixture changes | Verify the replacement covers its independently required behavior; the stacked branch itself is not the integration unit. |
| [#963](https://github.com/schiste/Aethyme/pull/963) | Per-repository identity and recovery implementation | Port the needed final behavior, adapt migrations and lifecycle code to current main, and qualify the replacement head. |

Issue #697 remains resolved by #882 regardless of the disposition of these
later PRs. Close the old PRs only after the replacement lands, with a link to
the replacement and this manifest. Until then, this table is a closure
checklist, not a claim that the older PRs have been superseded in GitHub.

## Integration boundary

The replacement is based on the baseline above. It must preserve the #882
configured-root and storage resolver, current worktree-root UUID recovery, and
#991 lifecycle claim ordering. Its database migration starts at schema 54.
Tests and docs should cover per-repository bindings in a shared database,
linked and nested worktree identity, interrupted identity publication, fresh
root reservation and recovery, and the relevant CLI/fixture behavior.

No local Cargo qualification is recorded here. Merge readiness depends on
required CI for the replacement PR's exact head.
