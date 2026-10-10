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
   inspection. The directory is flushed in this case too, and every directory the archive
   uses is re-flushed in its parent even when it already exists. Another writer may have
   created either entry and not flushed it yet, and an index row must never become durable
   before the entries for the objects it names. (Fault injection would be needed to test
   this ordering; it is not tested.)

The archive never hard-links into a checkout and never uses Git alternates: it holds its own
copy. An interrupted publication leaves at most a temporary or an unreferenced object. Both
are harmless, and a retry reuses the object (#659 reclaims leftovers).

### Capture reads Git objects, never the worktree

- The commit is a full object id (`CommitOid`). A ref name is refused, and `pin_commit`
  resolves one once. A branch that moves afterwards cannot change what is retained.
- The commit, every tree and every blob are read through `git cat-file --batch`. Each is
  hashed with the repository's object format (SHA-1 or SHA-256) while it is read, and
  must equal its object id. Git does not verify loose objects on read, and `git ls-tree`
  would list a damaged tree as genuine, so trees are walked here rather than listed.
- Git runs with replace refs, grafts (`GIT_GRAFT_FILE=/dev/null`) and lazy fetching
  (`GIT_NO_LAZY_FETCH=1`) off. `core.fsmonitor` is forced off and hooks point nowhere.
  Inherited repository variables (`GIT_DIR`, `GIT_COMMON_DIR`, alternates, ...) and
  injected configuration (`GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT/KEY_*/VALUE_*`) are
  cleared. Loading the index starts a configured fsmonitor, and `git check-attr` loads
  it: without the override, a repository's configuration ran a program during capture
  (found and tested).
- A partial clone (`extensions.partialClone`, a promisor remote or a partial-clone filter)
  is reported `partial_clone`, which is incomplete: objects it omits would surface as
  missing part-way through, and capture never fetches.

### Results: retained, incomplete or refused

| Result | Codes | Retry helps? |
|---|---|---|
| Retained | — | — |
| **Incomplete** (`is_incomplete()`) | `source_unavailable` (missing commit, tree or blob, including one that disappears mid-copy), `source_mismatch` (a commit, tree or blob that does not match its id), `history_unavailable` (missing parent, any shallow clone), `partial_clone` | Possibly, from a fuller source |
| **Refused** | `unsupported_entry` (submodule or another mode #652 refuses), `unsupported_filter`, `malformed_source`, `source_too_large`, `invalid_snapshot`, `base_not_ancestor`, `not_a_commit`, `not_an_object_id` | No |

None of these writes an index row, so none can be mistaken for a complete capture.

**Transforming attributes.** A path with `filter` (Git LFS, any clean/smudge driver),
`working-tree-encoding` or `ident` set is refused. Git would not check that path out as
the committed bytes, and replay would depend on an external driver. Attributes are read
per path with `git check-attr --source=<commit>`, so they come from the commit's own
`.gitattributes` (not the worktree), `.git/info/attributes` and `core.attributesFile`.
`check-attr` reads files and configuration only. Combined with the forced-off fsmonitor
and hooks, it runs nothing; a test configures an fsmonitor, filter drivers, textconv and
hooks and checks none ran. A declaration that matches no path is accepted. End-of-line
attributes are accepted too: the archive restores the committed bytes, and line endings
are a materialization choice.

### Lineage

A contribution (`retain_contribution(base, result)`) retains the **complete base and result
snapshots**. It refuses any shallow clone first, as incomplete. There, Git answers "not an
ancestor" when it cannot see the history, which must read as unknown, never as a rewrite.
It then requires `base` to be an ancestor of `result`. Replay needs only the two snapshots, so the commits in
between are recorded as provenance and not retained. The record holds the base and result
commit ids, the object format and the first-parent commit count.

### Rebuild

`reconstruct(snapshot, dest)` works from the archive alone:
- It accepts only a snapshot the archive retained: one with an index row, whose manifest
  re-encodes to exactly its stored bytes. A committed file whose bytes happen to parse as
  a manifest is an ordinary blob, and a reordered manifest is not the snapshot it claims.
- It verifies the manifest and every blob before writing anything, so a damaged or missing
  object writes nothing.
- It requires an absent or empty destination that is not a symbolic link.
- It restores bytes, the executable bit and symlinks.
- It detects collisions the destination filesystem causes, as #652 decided. Every file is
  created exclusively and every directory is tracked. A file or directory that "already
  exists" under another spelling (letter case or Unicode composition) is refused with
  `materialization_collision`, never merged or overwritten. The rule needs no case-folding
  table: the filesystem reports exactly what it folds.

## Assumptions

- **Bounded work for hostile repositories.** The repository decides object sizes and tree
  shapes, so both are bounded before they cost anything. A commit or tree over 64 MiB is
  refused (`source_too_large`) before it is read into memory. The tree walk keeps its own
  stack and refuses a path as soon as it passes the 4096-byte snapshot limit
  (`invalid_snapshot`), so a deeply nested tree cannot overflow the thread stack and abort
  the process. Blob contents are streamed, never held whole. The total size of a capture
  is bounded by #658's byte reservation.

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
| T07 | A moved branch does not change a pinned capture. Rewritten history is `base_not_ancestor`, and a graft file cannot hide it. Missing history (shallow clone, both ends as shallow boundaries), a partial clone and a blob removed mid-copy are incomplete. A blob or tree object that does not match its id is incomplete. |
| T08 | An interrupted copy leaves orphans and no index row; a retry completes. Per-CAP-state crashes are #658's. |
| T09 | State-root cleanup reach: #656. |
| T10 | Retain, delete the repository, rebuild: equal to an independent reading of a Git checkout (bytes, modes, symlinks, ID); same for both ends of a contribution. |
| Refusals | Submodule; `filter`, `working-tree-encoding`, `ident` from the tree, `info/attributes` and `core.attributesFile`; EOL attributes accepted; configured programs never run; damaged and missing archive objects; unretained or non-canonical manifests; non-empty or symlinked destination; case/composition collisions. |
