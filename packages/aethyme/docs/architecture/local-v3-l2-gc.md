# Local v3 — L2 reference-safe reclamation (#659)

Last Updated: 2026-10-09

This record covers how retained collaboration data is reclaimed without removing
anything a contribution, reader, citation or recovery still needs (plan §6.3, §6.16,
§10.1, §10.9). It is input to **D20** (who retains source), **D25** (reclamation under
pressure) and **D29** (cleaning abandoned artifacts). It honours the contract in
`local-v3-l2-capture.md` (#658), and builds on the archive (#657) and the state root (#656).

Code: `aethyme-broker::collaboration_gc`. state.db schema 4 adds `reader_leases`,
`object_pins`, `reclaimed_snapshots`, `reclaimed_contributions`, `gc_plans`,
`gc_generations` and `gc_trash`, as additive tables with the compatibility floor unchanged.

## Decided here

### Roots and reachability

Kept, with everything they reach:

| Root | Reaches |
|---|---|
| A `contribution` retention root, unreleased and before its `until` | The lineage record, both snapshots, and the operation's receipt |
| A reader lease (`acquire_lease`) on a snapshot or contribution, before its `until` | That snapshot or contribution |
| An object pin (`pin_object`), class `analysis_view` or `cited_evidence` | That one object |

A snapshot reaches its manifest, its snapshot record and every blob. A contribution
reaches its lineage record and both snapshots. If a live root cannot be resolved (its index
row, manifest or any object is missing), reclamation is **blocked**, with blocker
`dangling_root`. It never shrinks what is kept to fit.

Retention classes reported: `source` (blobs and manifests), `record`, `receipt`,
`analysis_view`, `cited_evidence`, and `orphan` (named by nothing). Analysis views and
cited evidence have no producer yet; L3 and L6 will pin through `pin_object`.

Explicit release: `release_retention(operation)` releases an operation's contribution
root. A lease is refused (`not_retained`) unless its target is retained now, so a lease
never resurrects reclaimed source. A pin is refused unless the object is present.

### Grace

An unrooted item becomes reclaimable once its newest object is older than the grace
period (default 24 h, `GcOptions::grace_ms`).
- **Index entries** (snapshots, contributions) are judged as a unit, by their own
  record and manifest. A young entry keeps everything it names.
- **Unindexed objects** (the leftovers of a failed capture) are judged by their own
  modification time.
- **Reuse counts as a write.** When a capture reuses an existing object, the archive
  refreshes its time, so an old orphan that a capture is about to name is inside the
  grace period again.

The lock files of ended operations, and abandoned archive temporaries past grace, are
reclaimable too. Anything under `objects/` or `spool/archive/` that is not an archive
object or temporary is reported under `unknown` and never removed. A file whose bytes do
not hash to its name is skipped at apply (`corrupt_object`).

### Plan and apply

- **`plan`** surveys the archive, records the decision under a digest, and returns
  everything a caller needs to act:
  - retained bytes per class, protected bytes and unknown files;
  - reclaimable items and the entries that will be marked reclaimed;
  - blockers, and the exact next action.
- **Blockers** empty the reclaimable list:
  - `capture_in_progress`: a capture is running;
  - `unrecovered_capture`: an operation stopped without an outcome; run
    `collaboration_capture::recover`;
  - `live_capture_intent`: an ended operation still holds an intent root;
  - `dangling_root`: a live root cannot be resolved;
  - `interrupted_gc`: a generation was interrupted; run `resume`.
- **`apply(digest)`** takes the archive **exclusively** without waiting, and refuses
  with `archive_in_use` if anyone holds it. It recomputes eligibility and removes only
  items that the plan named and that are still eligible with the same size. Everything
  else is reported as skipped (`no_longer_eligible`, `changed_since_plan`,
  `corrupt_object`), never removed.
- **Refusals:** an unknown plan (`unknown_plan`) and one older than a day (`stale_plan`).

### No race between insertion and removal

Every archive user holds `spool/gc.lock` shared: capture (taken before its operation lock),
recovery, `reconstruct`, `acquire_lease` and `pin_object`. Apply holds it exclusively, so
for the whole apply nobody can name an object, find one "already present" or read one. A
capture that arrives during an apply waits, then writes any object the apply removed
afresh (tested).

### Generations

One apply is one generation:
1. **One transaction** records the generation (`trashing`), every file it will remove,
   and the reclaimed markers for its snapshots and contributions.
2. **Files move** into `spool/gc/<generation>/`, and the directories are flushed.
3. The generation becomes **`trashed`**, the trash is deleted, and it becomes **`done`**.

Index rows of reclaimed snapshots and contributions stay, because receipts name them. A
marker excludes them from `retained()`, `reconstruct` and leases. Retaining the same
content again clears the marker and re-points the snapshot row at the new record.

**`resume`** finishes an interrupted generation:
- It puts every moved file back first, then judges each one as if nothing had moved.
- A file that something now relies on stays: for example, content captured again while
  the generation was interrupted (tested).
- Everything else is removed.

A crash at any point therefore leaves either the decision unmade, or a generation that
`resume` completes without removing anything rooted.

### Authority

Only this module reclaims collaboration data. Legacy broker cleanup cannot reach the root
(#656) and is not wired here. The CLI arrives with #680.

## Not decided here

- **D25.** The grace period and plan lifetime are provisional. Admission under pressure
  (the capture reservation in #658) and reclamation are not yet coupled. A full disk
  does not trigger reclamation automatically, and reclamation never runs implicitly.
- **D29.** Reclamation is an explicit plan and apply; the CLI and operator report come
  with #680.
- **D20.** Remote retention (P0) and export (#679) will add root kinds; the reachability
  walk is where they plug in.
- **Retention of receipts after release.** A released operation's receipt row stays, as
  history of what was promised. Its stored record is reclaimed with the source.
- **Linux.** These tests have run on macOS (APFS) only.

## Tests

| Plan test | State |
|---|---|
| INV01 | Every live receipt reconstructs after plan and apply, after a failed apply, after a killed apply, and after resume. |
| T45 | Apply fails after three moves: retained source is intact, the decision is durable, a capture of the same content during the interruption is honoured, and resume completes. An apply killed with `SIGKILL` mid-generation is resumed the same way. |
| T46 | Expired root plus a live reader lease: the leased snapshot is kept, the rest goes. Releasing the lease lets it go. One of two roots on the same content released: nothing but that operation's receipt goes. |
| Stale plan | Content captured again after the plan is skipped (`no_longer_eligible`), never removed. |
| Concurrent reader | Apply refuses while the archive is held. `reconstruct` waits for an apply. |
| Insertion racing GC | A capture that arrives during an apply waits, then retains its source in full. |
| Unknown and young | Unrecognised files and young orphans are protected; a misnamed object is skipped. |
