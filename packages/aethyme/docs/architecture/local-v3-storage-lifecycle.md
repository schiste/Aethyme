# Broker worktree storage lifecycle

The broker must account for worktree storage after its repository or placement
configuration disappears. A placement rule alone is insufficient: temporary
origins and changes between configured, host-state and fallback locations can
leave roots outside the next process's search path.

## Root history

A successful root admission records its exact canonical path, marker owner,
placement source, and root and origin filesystem identities. The bounded index
lives beside the host-operation database in a separate private directory. An
explicit private host-operation database therefore gets a separate index too.
Tests must supply that private coordination domain before startup maintenance
and root admission; a temporary Git repository alone provides no isolation.

The index is discovery history, not permission to delete. Planning combines it
with the current known placement locations. Parent path aliases resolve to one
canonical root. Unmarked roots, unsupported identity platforms, unreadable
history and unavailable original filesystems remain protected and make the
inventory incomplete. A floor below the storage budget is then `unknown`, not
proof that storage is within budget.

History is bounded to 8,192 records and 8,192 bytes per record, with bounded
inventory pages and a bounded SQLite file. Maintenance saves its rotating
cursor even when capacity prevents admission, so a full index can recover
without repeatedly inspecting only its first page. Missing records are pruned
only when absence is established on the recorded mounted filesystem.

## Reclamation authority

A reviewed GC candidate binds the exact saved identity record. Apply rechecks
that record, the actual root and marker, and origin absence on the original
filesystem. A host-domain process lock excludes root registration throughout
this check and deletion. It is released by the operating system after a crash.
Contention refuses new admission promptly and leaves cleanup pending.

The removal preserves its ownership marker until the remaining contents are
gone. A deadline leaves resumable journal work. Unavailable registry or
filesystem inspection leaves the candidate pending. A deletion I/O error is
currently reported and retires that candidate from the journal; the saved root
can be discovered by a fresh plan. Conclusively changed authority is retired
with a visible failure and no deletion; an operator can review a fresh plan.
Lost history requires independent recovery and may cause a fresh plan to omit
the root. Queue completion does not imply that every
candidate was removed: callers must inspect failures and removed roots.

Existing unregistered roots whose origins are already missing cannot acquire
original-filesystem evidence retrospectively. Preserve them for an independent
ownership review rather than treating an absent mount as a deleted repository.

## Prevention and remaining limits

Preparation checks admission on its actual write volumes before work and
between install steps. Cache writers and preparation-cache reclamation use the
same root resolver and lease key. These controls reduce avoidable allocation
and protect data during cleanup.

They do not impose a host-wide disk quota. A free-space observation reserves no
bytes, concurrent commands can outgrow their estimates, and arbitrary processes
can write after admission. Open sessions also retain protected source and
history. A hard storage bound needs an enforcement mechanism covering actual
writes; a reservation ledger alone cannot provide it.

Automatic cache continuation, transactional agent-start rollback, and any
broader volume admission or log policy are separately reviewed changes. Do not
infer that they are deployed from this root-history change. The separate
storage-reclaim inventory also needs explicit integration before it can claim
coverage of historical root locations.
