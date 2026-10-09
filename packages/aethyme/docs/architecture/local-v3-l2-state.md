# Local v3 — L2 collaboration state root (#656)

Last Updated: 2026-10-09

This record covers where retained collaboration state lives, who owns it, and what a
commit there survives (plan §6.3, §10.1 CAP). It is input to **D09** (journal and storage
root), **D18** (compatibility with old binaries), **D20** (who retains source) and **D25**
(reclamation). It builds on L0 slice C (`local-v3-l0/c-state-cleanup-reach.md`, PR #704),
which read every deleter in the broker.

Code: `aethyme-broker::collaboration_state`. Tests: the module's unit tests and
`aethyme-broker/tests/collaboration_state_cleanup.rs`.

## Decided here

### Location

```text
<host state>/collaboration/
  .aethyme-collaboration-root.json
  <project key>/
    state.db
    objects/  spool/  generations/  exports/   (added by #657-#659)
```

`<host state>` is the existing host state directory: `AETHYME_HOST_STATE_DIR`, else
`$XDG_STATE_HOME/aethyme`, else `~/Library/Application Support/Aethyme` (macOS) or
`~/.local/state/aethyme`. L0 slice C §3 found that no deleter enumerates this directory
itself; every consumer joins a fixed subpath. It is the only candidate that an old binary
cannot reach without a new fence. The others were rejected:

| Candidate | Why not |
|---|---|
| Inside a worktree container | The orphan sweep and storage reclaim enumerate its children. |
| `<repo>/.aethyme/` | Lost when the clone moves or is deleted (§6.3). |
| A session worktree | Removed with the checkout. |
| Host cache (`~/Library/Caches`) | Gate-cache GC and the operating system purge it. |
| `host-operations.db` / `host-resources.db` | Exact-version schemas: one change locks every older binary out host-wide. |

The project key is the directory name: opaque, `[a-z0-9-]`, at most 64 bytes. It will be
the enrolled ProjectId (#652, stored as `proj-<base32>`). It must never be the
path-derived repository key, which changes when a checkout moves.

### Refusals

Opening checks the location before creating anything. A refusal names the setting to
change; nothing is moved or repaired implicitly.

| Code | When |
|---|---|
| `overlaps_cleanup_root` | The root equals, contains or lies inside a worktree container, a host cache or the repository checkout. Containers are listed for **every** host state directory a process could resolve: the environment's, `XDG_STATE_HOME`'s and the platform default. A shell with different settings sweeps its own container (L0 §4.1). |
| `inside_git_worktree` | Any ancestor holds `.git`: a clean or checkout there can remove it. |
| `ephemeral_repository` | The repository is under the system temporary directory and the host state directory is implicit. This is the rule worktree placement already uses, so a fixture never writes into real host state. |
| `insecure_permissions` | A directory is not owned by the caller, or grants group or other access. |
| `foreign_root` | The root holds files but no root marker. |
| `project_mismatch` | `state.db` records another project: a directory was renamed or copied into place. |
| `not_a_collaboration_database`, `schema_too_new` | See *Compatibility*. |

### Topology: one database per project, no cross-store transaction

`state.db` is the single authority for one project's collaboration state: capture intents,
record indexes, retention roots, operations and outbox (§6.3). `broker.db` is unchanged
and keeps session and process authority. A collaboration record may name a broker session
as a plain value. It never depends on a `broker.db` write landing in the same transaction,
and the store attaches no other database (tested: `pragma_database_list` is `main` only).
A record and its outbox row can commit together. Object files, `broker.db` and remote Git
cannot, and nothing will claim otherwise.

### Permissions

Directories are created `0700` and `state.db` `0600`; SQLite gives its `-wal` and `-shm`
files the database's mode. On every open the root and project directories must be owned
by the caller with no group or other access. A looser directory is refused rather than
tightened, because something other than Aethyme changed it.

### Durability profile

`state.db` uses `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=ON` and
`checkpoint_fullfsync=ON`. With these settings a commit has returned only after its WAL
frame was flushed: on macOS with `F_FULLFSYNC`, which also empties the drive cache
(plain `fsync` does not). New directories and the root marker are synced the same way
(`File::sync_all`, which is `F_FULLFSYNC` on macOS).

| Profile | Filesystems | Receipt label |
|---|---|---|
| Supported | macOS: local `apfs`, `hfs`. Linux: local `ext4`, `xfs`, `btrfs`. All four settings read back as set. | `local_durable` |
| Anything else | Network, FUSE, `tmpfs`, `exfat`, or a setting SQLite refused | `local_unverified`, with the reason |

An unsupported filesystem still opens. It reports why, and any receipt carries the
weaker label (§10.1: "do not label its receipt identically").

**Assumptions:**
- **Process crash: tested.** A writer is `SIGKILL`ed while committing in a loop; every
  acknowledged commit is present, in order, and `integrity_check` passes.
- **Power loss or kernel panic: assumed, not tested.** The guarantee rests on SQLite's WAL
  protocol plus the flush above. A test cannot cut power. `synchronous=NORMAL` would pass
  the crash test and still lose the last commits on power loss, so the settings are pinned
  by read-back instead.
- **External drives.** A USB enclosure may acknowledge a flush it has not performed. The
  default root is on the boot disk. A host state directory on an external drive is
  outside the power-loss assumption even when its filesystem reads as supported.

### Backup and retention boundary

- A local receipt covers this disk only. It does not promise survival of the disk itself
  or anything off-host (§6.4).
- `Application Support` is included in Time Machine by default; `Caches` is not, which is
  another reason the root is not in the cache.
- A file-level copy of a live database can catch `state.db` without its `-wal`. A
  consistent backup copies all three files while no writer is open, or uses SQLite's
  backup API. The portable recovery package (`exports/`) is #659's.
- How long each class of data is kept is #659's (retention classes, GC).

### Compatibility (D18, D47)

`meta` records `schema_version` and `min_compatible_schema`. A binary opens a newer
database until a release raises the floor past it (`schema_too_new`), so a schema change
does not lock out every older binary, as `host-operations.db`'s exact match does. Old
binaries never open `state.db`, and an old binary's deleters cannot reach the root
(next section).

The floor was raised once, to 4, by reclamation (#659). A schema 3 binary captures
without the archive lock, the reuse refresh or clearing reclaimed markers, so in a store
that reclamation manages it could name an object that is being removed. Every open
raises a lower floor after migrating. No release shipped schema 2 or 3, so this locks out
no binary that exists.

## Old binaries and cleanup (T09, T35)

#656 changes no deleter, so this binary's deleters are the ones an older binary runs.
The integration tests run them through the CLI with a collaboration root present and
compare every byte and mode under it:

- **Default layout** (`<host state>/worktrees` is the container, a sibling of the root):
  finish with the checkout kept, `gc sweep`, `gc plan`/`apply` with the orphan sweep,
  `gc storage attribute --apply`, `gc storage plan`/`apply`, `gc reclaim plan`/`apply`.
  The planted orphan root is removed, which proves the sweep ran. The collaboration tree
  is unchanged.
- **Hostile layout** (`AETHYME_WORKTREE_ROOT` is the host state directory, so the root is
  one of the container's children): opening refuses. The same deleters still leave the
  root untouched. `gc plan` reports it as `unmarked_worktree_root`, and attribution
  does not adopt it.
- A state directory inside another shell's container is refused, and so is implicit state
  for a scratch repository.

## Not decided here

- **D09** closes with #658's fault-injected capture (crash at each CAP state while old
  cleanup and GC run) and a run of these tests on a Linux filesystem. This slice pins the
  settings and tests process crash only.
- **D18** needs T33–T35 command comparisons and a capture failure that stays separate
  from the legacy submit verdict (#658).
- **D20** needs the archive (#657): delete the contributor's storage after a receipt and
  reconstruct from the retained bytes.
- **D25** needs reservation and GC under ENOSPC (#659, T45).
- **Split roots.** Two shells with different `AETHYME_HOST_STATE_DIR` values resolve two
  collaboration roots for the same project. Overlap is refused, but a split is not
  detected. Nothing is lost, but a receipt written in one is invisible from the other. A
  per-repository pin of the resolved root would detect it; it is left for the capture
  slice, which will be the first writer.

## Tests

| Plan test | State |
|---|---|
| T07 | #657/#658 (moving branch during capture). |
| T08 | State layer: killed writer loses no acknowledged commit. Per-CAP-state crash points: #658. |
| T09 | Every legacy deleter, default and hostile layouts: collaboration bytes unchanged. Interleaving with collaboration GC: #659. |
| T10 | #657 (reconstruct after contributor storage is destroyed). |
| T35 | Old-binary deleters cannot reach the root (same code, unchanged). |
| T45, T46 | #659 (ENOSPC/OOM during GC; retention expiry while referenced). |
