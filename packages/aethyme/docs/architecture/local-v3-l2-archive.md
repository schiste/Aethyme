# Local v3 — L2 retained source archive (#657)

Last Updated: 2026-10-09

This record covers how a contribution's exact source is retained so it can be rebuilt after
the contributor's worktree, branch and Git objects are gone (plan §6.3-6.4, §10.1 CAP). It
is input to **D08** (materialization) and **D20** (who retains source), and builds on the
state root (#656, `local-v3-l2-state.md`), `SourceSnapshotId` (#652) and the record layer
(#653).

Code: `aethyme-broker::collaboration_archive`. Tests: the module's unit tests.

## Decided here

### Layout and naming

```text
<project dir>/
  objects/sha256/<2 hex>/<62 hex>   immutable; the name is the SHA-256 of the bytes
  spool/archive/                    temporaries only
```

| Object | Bytes | Name |
|---|---|---|
| Blob | The raw committed content | SHA-256 of the content (the #652 entry digest) |
| Snapshot manifest | The #652 manifest itself | The `SourceSnapshotId` digest |
| Record | Canonical bytes of an `aethyme.retained-snapshot` or `aethyme.contribution-lineage` record | SHA-256 of those bytes (its `RecordId` is separate) |

A snapshot's manifest is stored as-is rather than as a JSON record. The canonical JSON
profile caps containers at 4096 entries and allows only Unicode strings, while snapshots
have any number of files and raw-byte paths. Storing the manifest also means its object name
*is* the snapshot ID: finding and checking a snapshot needs nothing else.

`state.db` gains `retained_snapshots` and `retained_contributions` (schema 2, an additive
migration; the compatibility floor stays at 1). A row is written only after every object it
names is published. Rows are an index: the objects are the authority.

### Publication

1. Write the bytes to a temporary in `spool/archive/` and flush it (`F_FULLFSYNC` on macOS).
2. Re-read the file and hash what actually reached it.
3. Publish without replacing anything (`persist_noclobber`), then flush the directory.
4. An object that already exists under that name is accepted only if it hashes the same.
   Otherwise the archive is refused as corrupt and the existing file is left for
   inspection.

The archive never hard-links into a checkout and never uses Git alternates: it holds its own
copy. An interrupted publication leaves at most a temporary or an unreferenced object. Both
are harmless, and a retry reuses the object (#659 reclaims leftovers).

### Capture reads Git objects, never the worktree

- The commit is a full object id (`CommitOid`). A ref name is refused, and `pin_commit`
  resolves one once. A branch that moves afterwards cannot change what is retained.
- Trees come from `git ls-tree -r -z`, blobs from `git cat-file --batch`. Neither runs
  hooks, clean/smudge filters or textconv. Replace refs are disabled, and inherited
  `GIT_DIR`/alternates variables are cleared.
- Each blob is hashed with the repository's object format (SHA-1 or SHA-256) while it is
  copied, and must equal its object id. Git does not verify loose objects on read, so a
  damaged source is caught here.

### Results: retained, incomplete or refused

| Result | Codes | Retry helps? |
|---|---|---|
| Retained | — | — |
| **Incomplete** (`is_incomplete()`) | `source_unavailable` (missing commit, tree or blob, including one that disappears mid-copy), `source_mismatch`, `history_unavailable` (missing parent, shallow clone) | Possibly, from a fuller source |
| **Refused** | `unsupported_entry` (submodule or another mode #652 refuses), `unsupported_filter`, `invalid_snapshot`, `base_not_ancestor`, `not_a_commit`, `not_an_object_id` | No |

None of these writes an index row, so none can be mistaken for a complete capture.

**Filters.** A `.gitattributes` anywhere in the tree that sets `filter=` (Git LFS, any
clean/smudge driver) or `working-tree-encoding=` is refused. A Git checkout of that tree
would not produce the committed bytes, and replay would depend on an external driver.
End-of-line attributes are accepted: the archive restores the committed bytes, and line
endings are a materialization choice.

### Lineage

A contribution (`retain_contribution(base, result)`) retains the **complete base and result
snapshots**. It requires `base` to be an ancestor of `result` and refuses shallow clones,
where the boundary hides history. Replay needs only the two snapshots, so the commits in
between are recorded as provenance and not retained. The record holds the base and result
commit ids, the object format and the first-parent commit count.

### Rebuild

`reconstruct(snapshot, dest)` works from the archive alone:
- It verifies the manifest and every blob before writing anything, so a damaged or missing
  object writes nothing.
- It requires an absent or empty destination.
- It restores bytes, the executable bit and symlinks.
- It detects collisions the destination filesystem causes, as #652 decided. Every file is
  created exclusively and every directory is tracked. A file or directory that "already
  exists" under another spelling (letter case or Unicode composition) is refused with
  `materialization_collision`, never merged or overwritten. The rule needs no case-folding
  table: the filesystem reports exactly what it folds.

## Assumptions

- **Source trust.** The bytes are whatever the contributor's repository holds under the
  pinned commit. Proving who authored them is not this layer's job.
- **Durability** is the state root's profile (#656). Object and directory flushes follow
  it, and an unsupported filesystem weakens the receipt label, not the archive layout.
- **Memory.** Blobs stream to disk while they are copied. `reconstruct` holds one blob at a
  time.

## Not decided here

- **Receipts, retention roots and the capture state machine** (CAP_INTENT…ACKNOWLEDGED) are
  #658's. That includes when a retained snapshot is promised to anyone, and idempotency
  by operation id.
- **Reclamation** of orphans and expired snapshots is #659's. Nothing here deletes an object.
- **D08** also needs #670's execution identity: equal source is not equal build inputs.
- **D20 closes** with #658's receipt plus these rebuild tests, run against a retention
  boundary.
- **Submodules, LFS and other filters** stay unsupported. Supporting them would mean
  retaining what the driver produces, under a named profile.

## Tests

| Plan test | State |
|---|---|
| T07 | A moved branch does not change a pinned capture. Rewritten history is `base_not_ancestor`. Missing history (shallow clone) and a blob removed mid-copy are incomplete. A source object that does not match its id is incomplete. |
| T08 | An interrupted copy leaves orphans and no index row; a retry completes. Per-CAP-state crashes are #658's. |
| T09 | State-root cleanup reach: #656. |
| T10 | Retain, delete the repository, rebuild: equal to an independent reading of a Git checkout (bytes, modes, symlinks, ID); same for both ends of a contribution. |
| Refusals | Submodule, LFS/`filter=`, `working-tree-encoding=`; EOL attributes accepted; damaged and missing archive objects; non-empty destination; case/composition collisions. |
