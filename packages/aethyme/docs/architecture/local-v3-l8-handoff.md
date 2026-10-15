# Local v3 L8 controlled generation handoff

Last Updated: 2026-10-15

> Historical checkpoint: the implementation and pending-work statements below
> describe the original provisional profile. Current bounded E1, FX08, native-gate
> and appointed-controller results are recorded in the
> [Local qualification ledger](local-v3-qualification.md). Each original fixture
> and profile keeps its own evidence and limits.


Status: reviewed implementation for #678 with qualified controlled Unix schedules, based on provisional Local L4.

## Boundary

GEN_PREPARED is a historical offer. Adoption requires the appointed Local
controller and an actual controlled executor dispatch at the exact new absolute
root. Metadata offers, caller supplied acknowledgments and stored receipts do
not authorize execution. Unmanaged editors keep the old path and receive the
new path for explicit manual adoption.

The first profile uses a trusted native Unix command wrapper. The wrapper
acknowledges its actual physical working directory over its private output
pipe, then waits for GO on its private input pipe. The operator appointed
command receives GO only after the guarded adoption commit. The parent owns the
native Child and the private agent handle. Serialized data cannot recreate
that handle. This records the wrapper's initial dispatch root; it does not
confine a workload that subsequently changes directory, opens arbitrary paths,
or creates detached children. This executor is separate from the Linux
verification supervisor and carries no verification or publication permission.

## Lifecycle

An immutable handoff request binds controller, target revision, Local authority
epoch, exact prepared source and root, intended agent session, operation and
expiry. Persist the request and a dispatch intent before spawning anything.
The intended agent is private process state bound to that request. Keep dispatch
paused until existing controlled command leaders have completed, or explicitly retain
those owned commands as attachments to their old generation.

Before launch, validate current controller and native acceptance, the complete
prepared checkout and the request mapping. After the actual wrapper reports its
physical root, repeat expiry and source checks before committing the
adoption receipt and session's current generation. Hold the native acceptance
and external epoch guards continuously through launch, acknowledgment, commit
and GO. Final adoption and GO admission recheck exact target revision, phase
and version; the database writer reservation remains held through GO. Stop or
drift cannot cross that boundary, including a stop/resume ABA. The wrapper cannot start the appointed workload before that boundary. No shared filesystem pointer
is swapped. Old workspaces, pending contributions and old child attachments
remain retained. Leader completion leaves descendant liveness unknown and
never proves tree quiescence. Retirement and cleanup require a separate reviewed operation.

Only the current private handle may authorize consumption of current agent data.
A `CurrentAgentGuard` borrows the store mutably and holds an immediate SQLite
transaction plus the native epoch guard through consumption. Another handoff or
target transition cannot cross it. Completion uses that same transaction and
checks expiry at classification commit; leader accounting remains independent of
result authority, so an expired result is historical without stranding recovery. Changed
session generation, revision or Local epoch makes an old handle stale. Stored
results and adoption receipts remain historical. This API does not claim to
fence the existing ordinary broker submit/gate path; that integration waits for
#725 and the later compatibility qualification.

## Recovery

A request before dispatch is an alternative that can be inspected. A persisted
dispatch intent after an interrupted launch has unknown liveness and must not
be automatically relaunched, inferred stopped from a PID, or converted to an
adoption receipt. Acknowledged execution does not authorize deleting old work.
Historical inspection never reconstructs a live agent handle. Retain unknown
attachments for explicit reconciliation under fresh authority. The operator may
append a retained-unknown decision only while the old native agent's OS lock is
released. It pauses the session without certifying old process death, deleting
its root or replaying the original operation. Fresh appointment rotates the
nonce; subsequent execution needs a new request and fresh source/acceptance proof.
An adoption receipt proves the native wrapper's READY root and guarded dispatch
admission. A crash after that commit but before GO does not prove the appointed
workload ran; preserved uncertainty requires explicit reconciliation.

## Required evidence

Use real native children to demonstrate initial physical root, open descriptors,
late old-root writes and watcher reads. Exercise pause between commands,
explicit old attachment, wrong owner/session, stale source/revision/epoch,
expiry, duplicate requests, damaged prepared bytes, and interruption before and
after native acknowledgment. Independent review and guard-removal checks are
required. E1 remains junior-owned; no complete composition qualification is
claimed by this contract.

## Implementation checkpoint

Profile `aethyme.handoff.native-unix-command/provisional-v0`; collaboration schema
and compatibility floor 16. The appointed agent holds its native per-session OS
lock and nonce. Durable intent precedes spawning, READY is bounded and private,
and the workload cannot begin before guarded adoption and GO. Per-session
adoption does not mutate a prepared generation's global materialization state.
Old roots and descendant attachments remain retained; direct leader completion
never proves descendant quiescence.

All 18 handoff schedules passed in the final restored selection (the one ignored
handoff test is an owned child helper invoked by its parent schedule). These
include actual controlled commands; target stop/drift/ABA at READY and stop after
adoption; physical root acknowledgment; SQLite writer reservation; native
freeze; finite command limits; interrupted intent/spawn/READY/adoption; explicit
unknown retention; expiry and fresh appointment. A real parent exits while a
grandchild preserves its old open descriptor and watcher path through a later
new adoption.

Ten deliberate guard removals each produced a named failing assertion and exit
101, followed by exact source restoration. The first private-lock mutation did
not compile and is excluded; its corrected independent trial failed the intended
assertion. The restored regression selection passed 443 test executions with
seven ignored helpers; these counts include overlapping module selections, not
443 unique scenarios. Strict Clippy passed. Independent adversarial review is
clear and the corrected implementation/parent fixture head has green CI.
The independent Ruby adoption vector is
`sha256:1a23942180fad840e1880c207986122de4ea127e2d31aaeff0e5a80755a2dc4f`.

This profile is not a verification sandbox or a process tree cleanup promise.
READY/adoption records dispatch admission, not proof the appointed workload ran.
Native broker/gate integration still waits for separately owned #725. E1 must
requalify composition-dependent assumptions before the profile is qualified
beyond its current provisional Local boundary.
