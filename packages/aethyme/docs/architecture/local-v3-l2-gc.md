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
reaches its lineage record, both snapshots, and its attached decision brief (#661, class
`record`). A brief replaced by a later one is named by nothing and goes as an orphan. A
reclaimed contribution's `contribution_briefs` row stays, like its other index rows,
behind the reclaimed marker, so the foreign key always holds (tested with
`foreign_key_check` after apply). If a live root cannot be resolved (its index
row, manifest or any object is missing), reclamation is **blocked**, with blocker
`dangling_root`. It never shrinks what is kept to fit.

Retention classes reported: `source` (blobs and manifests), `record`, `receipt`,
`analysis_view`, `cited_evidence`, and `orphan` (named by nothing). Analysis views and
cited evidence have no producer yet; L3 and L6 will pin through `pin_object`.

Explicit release: `release_retention(operation)` releases an operation's contribution
root. A lease is refused (`not_retained`) unless its target is retained now, so a lease
never resurrects reclaimed source. A pin is refused unless the object is present.

### Grace and the clock

An unrooted item becomes reclaimable once its newest object is older than the grace
period.
- **Index entries** (snapshots, contributions) are judged as a unit, by their own
  record and manifest. A young entry keeps everything it names.
- **Unindexed objects** (the leftovers of a failed capture) are judged by their own
  modification time.
- **Reuse counts as a write.** When a capture reuses an existing object, the archive
  refreshes its time, so an old orphan that a capture is about to name is inside the
  grace period again.

**Grace and plan lifetime are coupled.** Grace (`GcOptions::grace_ms`, default 24 h)
must be at least the plan lifetime (`PLAN_TTL_MS`, 24 h); anything shorter is refused
(`invalid_grace`). Otherwise an object a plan named could be reused by a new index entry
after the plan and still look old when the plan is applied. As a second guard, apply
skips (`named_by_index`) any object that an index entry names unless that entry is marked
reclaimed in the same generation.

**The real clock only.** There is no option to judge expiry or grace "later": a future
time would reclaim live `until` roots, leases and pins early.

**Unrecognised content.**
- Anything under `objects/` or `spool/archive/` that is not an archive object or
  temporary is reported under `unknown` and never removed.
- A symlinked or non-directory component anywhere on those paths, or on `spool/capture/`,
  is also reported as unknown. It is never followed, so reclamation never lists or moves
  files outside the store. Integrity checks and pins never hash through a link either.
- A file whose bytes do not hash to its name is skipped at apply (`corrupt_object`).

**Lock files.** The lock file of an ended operation is reclaimable only when no retry
can resume that operation (`committed`, `acknowledged`, `refused` or `aborted`). Lock
files of `failed` or `incomplete` operations, which are retryable, are kept. Apply
unlinks a lock file only while holding its flock, and re-checks the operation's state
under it; a held lock is skipped (`lock_held`). Abandoned archive temporaries past grace
are reclaimable too.

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
- **How long the archive is held.** Apply holds the exclusive lock while it does three
  things: re-surveys the store, streams each planned object through SHA-256 to check it
  (nothing is read whole), and moves files. One generation removes at most
  `MAX_ITEMS_PER_GENERATION` (10,000) files; the rest are skipped as
  `deferred_to_next_generation` for the next plan. Captures and readers wait, they do not
  fail, while it runs.

### No race between insertion and removal

Every archive user holds `spool/gc.lock` shared: capture (taken before its operation lock),
recovery, `reconstruct`, `acquire_lease` and `pin_object`. The archive's writers
(`put_object`, the retain functions) are crate-private, and only capture calls them in
production, so nothing writes the archive without the lock. Apply holds it exclusively, so
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

A receipt answered again (a retried capture, or `receipt()`) reports its current
standing:
- `retained_local` while its contribution root is live;
- `released` once that root is released or past its boundary;
- `reclaimed` once reclamation marked its contribution or snapshots.

It never claims `retained_local` for source that no longer has a root.

**Compatibility.** Reclamation raised the store's compatibility floor to 4 (see
`local-v3-l2-state.md`). A schema 3 binary would capture without the archive lock, the
reuse refresh or clearing markers.

**`resume`** finishes an interrupted generation:
- It puts every moved file back first, then judges each one as if nothing had moved.
- When a later capture wrote a file again, so both copies exist, resume hashes both and
  keeps the one that matches its name. A copy that does not match is set aside as
  `<name>.corrupt-<generation>-<original|trashed>`, never deleted.
- Before re-trashing anything it makes apply's integrity check again.
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
| Coupled grace | A grace shorter than a plan's lifetime is refused. An index entry created after the plan keeps the object it reused. |
| Symlinks | A symlinked `objects/sha256`, fan-out directory or `spool/archive` is reported and never followed; files outside the store survive. A pin never hashes through a link. |
| Resume copies | A corrupt original written during the interruption is set aside, and the good trashed copy is restored for the receipt that relies on it. |
| Receipt standing | A retried capture reports `retained_local`, then `released`, then `reclaimed`. |
| Lock files | A lock GC cannot take is skipped; a retryable operation's lock is never planned. |
| Floor | New and migrated stores have floor 4. |
