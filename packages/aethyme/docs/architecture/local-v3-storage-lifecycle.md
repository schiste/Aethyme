# Broker worktree storage lifecycle

Last Updated: 2026-10-27

The broker must account for worktree storage after its repository or placement
configuration disappears. A placement rule alone is insufficient: temporary
origins and changes between configured, host-state and fallback locations can
leave roots outside the next process's search path.

## Root history

A successful root admission records its exact canonical path, marker owner,
placement source, and root and origin filesystem identities. On macOS, new
records identify each directory by its inode and the current filesystem UUID;
the mount-scoped device and `f_fsid` values remain available for runtime
pin-time checks. A remount can change those mount-scoped values without
changing the volume UUID or directory inode. Linux keeps its existing
device, `f_fsid`, and inode behavior.

The bounded index lives beside the host-operation database in a separate
private directory. An explicit private host-operation database therefore gets
a separate index too. Tests must supply that private coordination domain before
startup maintenance and root admission; a temporary Git repository alone
provides no isolation.

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

A new ownership marker can be created only in an empty directory. Selecting an
existing nonempty unmarked path refuses before changing its permissions or
contents; there is no implicit adoption. Existing markers are type-checked and
read with an 8 KiB limit. Unix opens also prohibit following a replaced symlink;
other platforms do not claim that handle-level race protection. Marker creation
is exclusive and never
overwrites an existing witness. Interrupted creation can leave a bounded invalid
marker that refuses admission and requires independent recovery.

Reusing a canonical pathname cannot replace an older filesystem proof. An
existing row must still match the root, origin and ownership marker before
admission, and is rechecked before recording. Changed or unknown evidence is
retained and refuses replacement. On platforms without filesystem identity,
only byte-identical idempotent reuse is allowed; that opaque history remains
protected from automatic reclamation.

When a legacy macOS row still matches the live root and origin by inode, device,
and raw `f_fsid`, ordinary admission may record the live UUIDs forward. The
shared registry lock remains held through owner, root, origin, and replacement
record validation. If any saved raw identity differs, the row stays fenced for
reviewed recovery. A missing origin cannot acquire a UUID: its absence is
accepted only when the nearest existing ancestor matches the saved raw volume
identity, and the retained row remains unchanged. A raw mismatch leaves absence
unknown.

Recover one mismatched legacy row with
`aethyme broker gc storage identity plan <host-operation-db> <exact-root>`, then
review its exact old and replacement payload hashes and apply them with
`aethyme broker gc storage identity apply <host-operation-db> <exact-root>
--confirm <old-sha256:new-sha256:owner-marker-sha256>`. The marker digest binds
the exact bytes observed during planning; apply rechecks those same bytes after
the payload update and rolls back if they changed. Marker validation accepts
the compact encoding and the broker's production pretty-JSON-plus-newline
encoding, without accepting arbitrary whitespace. Recovery takes the same
host-domain registry lock as admission and removal. It requires the recorded
root and origin inodes, owner marker and repository binding to match, and
nonzero current volume UUIDs for both directories. It explicitly does not
prove the historical volume UUIDs; the operator reviews that gap before
applying. An older binary with the strict pre-UUID record decoder may refuse a
row after recovery, so retain the host state until using a compatible version.

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
between install steps. During a running command, the heartbeat also observes
those volumes every second, independently of the ten-second lease renewal.
Low or unreadable headroom returns a typed storage refusal before renewal and
requests recovery through the existing failure path. This is cooperative
pressure detection: an observation can stall, and writes between observations
can cross the reserve. Descendant shutdown and an unsettled writer lease must
also be handled before cleanup can safely use that cache.
Gate finalization also probes the command process group's absence before
releasing host resources. A shell exit and output EOF do not establish that
its descendants have stopped. Unconfirmed cleanup fails the gate, leaves
cache-size observations unsettled and quarantines the exact resource grant.
This probe never signals a group after its leader was reaped. It protects the
named cache from another cooperative writer or collector; it does not itself
stop a remaining writer. Deliberate process-group escape is outside this
contract.

Cache writers and preparation-cache reclamation use the
same root resolver and lease key. These controls reduce avoidable allocation
and protect data during cleanup.

B1 wires internal per-checkout shared/exclusive admission locks and durable
per-invocation producer markers under the primary repository runtime directory.
Managed preparation holds one shared scope lock from before cache-root
initialization through synchronous cache and record writes, every supervised
step, postprocessing, and the existing host-resource settlement. Each
dispatched preparation step has its own durable marker. Gates create a marker
before managed-cache preparation and hold its shared lock through command
quiescence and gate-result recording. Guarded execution also admits before
dispatch; supervised commands retain their marker through the owned helper's
exit and original process-group absence. Legacy unsupervised guarded execution
is deliberately left as an unknown producer.

Clearing a running marker requires both the owned child to have exited and its
original process group to be observed absent. This proves quiescence only for
that group; descendants that escape it are outside the proof. Preparation's
outer scope lock protects its synchronous writes while the broker is alive,
while per-step markers protect any supervised child that outlives a crashed
broker. A dropped or unsettled producer guard leaves its marker for conservative
review. No cleanup path consumes these locks or markers and B1 grants no
deletion authority.

Clients predating this protocol do not honor these locks and remain unknown.
The admission protocol currently supports Unix targets with directory sync;
non-Unix targets return `ProtocolUnavailable` and their reclaimer remains
blocked regardless of inventory, keeping automatic artifact cleanup disabled.
Unix filesystem sync errors fail admission and leave cleanup unknown; callers
must not dispatch after an admission error. Process start tokens are currently
available on macOS and Linux; markers without a token remain unknown after
supervisor death. B2 must establish compatibility with older and unavailable
clients and verify complete process use before cleanup can treat artifact-use
admission as deletion evidence; an empty marker inventory alone is not enough.

The opt-in `managed_storage` profile currently describes intended preparation
growth but does not enforce a bound. A minimal worktree-local declaration is:

```toml
schema_version = 1

[[steps]]
name = "deps"
command = ["tool", "install"]
outputs = ["vendor/"]
required_for_hooks = false

[managed_storage.steps.deps.output_growth_bytes]
"vendor/" = 4096
```

The broker validates exact step and output coverage, positive byte bounds,
non-overlapping paths, and the captured worktree/cache roots. Until reservation,
existing-root adoption and old-writer fencing are integrated, an otherwise
valid, trusted `prepare_session` request with an enabled profile returns typed
`ManagedStorageNotReady` before starting a preparation command, including this
valid example. This is an explicit fail-closed staging contract, not a quota or
an active reservation. The preparation tests pin the real broker call path with
a command-created sentinel and a guard-removal mutation; see
`preparation::tests::prepare_session_opt_in_guard_prevents_command_start`.

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
