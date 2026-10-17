# Broker worktree storage lifecycle

Last Updated: 2026-10-17

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

Admission checks that exact history domain and its capacity before creating a
worktree root, then holds the registry lock through root creation and recording.
A failed placement may use the reported repository fallback while history is
available. Unavailable history refuses new allocation; it never selects another
coordination domain. Git checkout, session registration and agent launch wait
for the root record to succeed. A crash or late I/O failure before that record
can leave an empty directory or ownership marker outside the index; this is
control debris, and is not a promise of atomic filesystem admission. No checkout
or cache payload is allocated in that window.

Reusing a canonical pathname cannot replace an older filesystem proof. An
existing row must still match the root, origin and ownership marker before
admission, and is rechecked before recording. Changed or unknown evidence is
retained and refuses replacement. On platforms without filesystem identity,
only byte-identical idempotent reuse is allowed; that opaque history remains
protected from automatic reclamation.

## Reclamation authority

A reviewed GC candidate binds the exact saved identity record. Apply rechecks
that record, the actual root and marker, and origin absence on the original
filesystem. A host-domain process lock excludes root registration throughout
this check and deletion. It is released by the operating system after a crash.
Contention refuses new admission promptly and leaves cleanup pending.

The claim retains the directory handle whose identity it verified; deletion
does not reopen the root by its pathname. Before the first mutation, the
deleter reads the ownership marker through that handle and checks its exact
owner fields against the reviewed record, then rechecks origin absence. This
is a point-in-time check under the cooperative-writer rules. Removal preserves its ownership marker until
the remaining contents are gone. A deadline leaves resumable
journal work. Unavailable registry or filesystem inspection and deletion I/O
errors leave the candidate pending with a visible failure. Conclusively
changed authority is retired
with a visible failure and no deletion; an operator can review a fresh plan.
Lost history requires independent recovery and may cause a fresh plan to omit
the root. Queue completion does not imply that every
candidate was removed: callers must inspect failures and removed roots.
An authority change detected by the pre-unlink callback initially leaves the
candidate pending; the next apply rechecks and retires the changed authority.

Descriptor-relative deletion refuses link redirection and changed directory
bindings. It assumes closed roots have cooperative writers; it does not freeze
directory ancestry against arbitrary concurrent hostile renames.

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
