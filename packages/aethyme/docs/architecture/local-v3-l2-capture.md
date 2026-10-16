# Local v3 — L2 crash-safe capture (#658)

Last Updated: 2026-10-09

This record covers the CAP protocol: how a local capture records its intent, retains
source, commits a receipt and recovers after a crash (plan §6.4, §10.1, §10.9). It is
input to **D09** (journal and durability), **D18** (capture separate from legacy
submit), **D20** (who retains source), **D25** (reservation) and **D26** (outbox). It
builds on the state root (#656, `local-v3-l2-state.md`) and the archive (#657,
`local-v3-l2-archive.md`).

Code: `aethyme-broker::collaboration_capture`. state.db schema 3 adds
`capture_operations`, `capture_receipts`, `retention_roots` and `outbox`, as additive
tables with the compatibility floor unchanged.

## Decided here

### States

| State | Durable point | After a crash |
|---|---|---|
| `intent` | One short transaction: the operation row (pinned base and result object IDs, policy, retention, request digest), a byte reservation and a `capture_intent` retention root. | Retry the same operation, or `abort`. Nothing is promised yet. |
| `copying` | The archive copies and verifies source. Every object is flushed before it is published. | Copied objects are orphans that the intent root protects; a retry reuses them. |
| `sealed` | Every object is published and indexed, and the lineage record ID is on the operation row. | Commit without reading the source again, so it completes even if the contributor's repository is gone. |
| `committed` | One transaction holds the receipt row, a `contribution` retention root, the released intent root, the outbox row and the state. | The contribution is recoverable. A retry answers with the same receipt. |
| `acknowledged` | Set after the receipt is built for the caller. | Same as `committed` for every reader. A lost acknowledgement changes nothing. |

Failures end in a state that says what a retry can do:

| State | Meaning | Retry with the same operation |
|---|---|---|
| `incomplete` | The source was missing or did not match, the clone is partial, or a transforming attribute comes from this clone's `info/attributes`, `core.attributesFile` or system attributes rather than the commit (archive `is_incomplete`). | Yes, for example from a repaired, fuller or clean clone. The repository path is not part of the request, so a retry may come from another clone. |
| `failed` | A local error such as a full disk, or `interrupted` set by recovery. | Yes |
| `refused` | Unsupported entry, a transforming attribute the commit's own `.gitattributes` sets, malformed source, invalid snapshot, base not an ancestor, not a commit. | No: the same refusal is returned. |
| `aborted` | Explicit `abort`. | No |

Each terminal failure releases the intent root and the reservation. An operation that
is not running protects nothing. Stored failure details come from Git's stderr, which the
repository controls, so they are capped at 4 KiB.

### Preflight

Before anything is reserved, the capture refuses a partial clone and lists both trees
to estimate their size, running Git as the archive does: no lazy fetch, no prompts, and
no caller-supplied `GIT_DIR`, object-directory or configuration variables. A tree Git
cannot list is answered as `incomplete`, never as a small estimate. A new operation
answered here leaves no row; a retried one records the outcome on its row.

### Recovery

`recover` resolves every in-flight operation that no live worker holds:

- `intent` and `copying` become `failed` (`interrupted`);
- `sealed` is committed, or becomes `failed` (`corrupt_receipt`) if its retained bytes
  are gone, which releases what it held; a retry copies again;
- one operation it cannot resolve is reported in `RecoveryReport.errors` and does not
  stop the others.

Every capture also resolves other crashed operations, using a non-blocking lock so a
live worker is skipped, before it checks space. One crash therefore cannot make every
later capture `insufficient_space` until someone calls `recover`. Callers (#660, #680)
should still call `recover` at startup, to commit sealed operations and to surface
errors.

### Retry key and concurrency

- **The operation ID is the retry key.** The same ID with the same request (base,
  result, policy, retention) returns the same receipt. The same ID with a different
  request is refused (`conflicting_operation`). The repository path is a locator, not
  part of the request.
- **One worker per operation.** A worker holds `spool/capture/<operation>.lock`
  (`flock`) for the whole capture, so concurrent deliveries run one after another and
  the second finds the receipt. The kernel releases the lock when a process dies, which
  is how `recover` tells a dead worker from a live one. After acquiring, the worker
  checks that the path still names the file it locked, and starts again if not: a
  waiter woken on an unlinked file must not hold a lock alongside whoever locked the new
  one (tested).
- **The commit transaction is idempotent.** It checks for an existing receipt, so
  committing a sealed operation twice adds no second receipt, root or outbox row
  (tested). Concurrent deliveries rely on the lock: without it they are not safe.
- **A different ID for the same content** is a separate operation, with its own receipt,
  retention root and outbox row. It names the same contribution, because the lineage
  record ID is derived from content. A contribution is identified by its lineage record;
  operations are attempts to retain and announce it. Releasing one operation's root
  never removes source that another root still holds.

### Receipt

An `aethyme.capture-receipt/experimental-v0` record, stored in the archive before the
commit transaction that names it. It contains the operation, `status: retained_local`,
the store's durability label, the contribution, the base and result snapshot IDs, and
the retention boundary (`until_released`, or a time). It has no timestamps, so a retry
produces the same receipt bytes and ID.

- **`retained_local`** means the bytes and record survived the store's durability
  profile on this host. It is never an off-host promise.
- **Durability label.** On an unsupported filesystem the label is `local_unverified`
  instead of `local_durable`, so the two receipts never read the same (§10.1).

### No cross-store transaction

Objects are published before the commit transaction that names them. A crash in between
leaves orphans, never a receipt without bytes. The commit re-checks that both snapshot
manifests are on disk, and refuses (`corrupt_receipt`) if one has gone. Nothing touches `broker.db`.

The outbox row is only a local intent: it is committed with the receipt and implies no
delivery. Delivery semantics (at least once, cursors) belong to L8 (#676, D26).

### Capture is separate from submit

Nothing here runs from legacy submit or changes its verdict. The policy (`advisory` or
`required`) is recorded for #660, which decides what each policy does.

### Byte reservation (D25, provisional)

At intent, the estimate is every blob of both trees before deduplication, plus 1 MiB of
overhead. It is refused (`insufficient_space`) when the filesystem's free bytes, minus
every active operation's reservation, minus the estimate, would fall below 256 MiB. The
reservation is released when the operation leaves `intent`, `copying` or `sealed`. This
is admission control, not a guarantee; a disk that fills anyway is a retryable `failed`.

## Contract for reclamation (#659)

An archive object may be reclaimed only when **all** of these hold:

- no unreleased `retention_roots` row reaches it. A `contribution` root reaches the
  lineage record, both snapshot manifests and records, every blob and the receipt
  record;
- no operation is in `intent`, `copying` or `sealed`. Their unindexed orphans are
  protected only by the operation's live `capture_intent` root;
- the operation's lock file is not held.

The objects of `incomplete`, `failed`, `refused` and `aborted` operations that no other
root reaches are reclaimable after a grace period that #659 sets.

**Lock files.** A lock file may be unlinked only by a process that holds its `flock`,
and only while the operation is in a non-retryable state: `committed`, `acknowledged`,
`refused` or `aborted`. `failed` and `incomplete` operations are retried with the same
lock path, so their lock files stay.

## Not decided here

- **D09** still needs a Linux run of these tests and a capture interrupted while
  legacy cleanup runs. The root is already proven out of cleanup's reach (#656); the
  spool and objects live under it.
- **D18 / D32:** what `required` capture blocks is #660.
- **D20:** remote retention is P0. Locally, a receipt is reconstructible until its
  retention boundary (INV01, tested below).
- **D26:** outbox delivery, cursors and acknowledgements are L8's.
- **Retention release** and the expiry of `UntilMs` roots are #659's.

## Tests

| Plan test | State |
|---|---|
| T07 | A branch that moves after the commits are pinned changes nothing. A blob missing at copy time ends `incomplete`, and a retry after the repair completes. |
| T08 | A fault at every boundary: after intent, during copy (two points), after copy, after sealed, inside the commit transaction, and after commit (lost acknowledgement). Each recovers and retries to the receipt a clean run produces, with exactly one receipt, root and outbox row, and both snapshots rebuilt. A real `SIGKILL` child keeps every receipt it reported. |
| T09 | Root and spool are under the #656 root that legacy cleanup cannot reach (tested there). |
| T10 | The repository is deleted after a receipt, and after `sealed` but before commit; both rebuild. |
| T45 | Partial: reservations refuse before anything is written, an in-flight reservation blocks a second capture until it is aborted, and ENOSPC mid-copy is a retryable failure with no partial contribution. GC under ENOSPC is #659's. |
| T46 | #659 (retention expiry while referenced). |
| Repeated delivery | 8 concurrent deliveries of one operation produce one receipt; committing a sealed operation twice adds nothing. |
