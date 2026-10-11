# Local v3 L8 controlled generation handoff

Last Updated: 2026-10-11

Status: implementation contract for #678, based on provisional Local L4.

## Boundary

GEN_PREPARED is a historical offer. Adoption requires the appointed Local
controller and an actual controlled executor dispatch at the exact new absolute
root. Metadata offers, caller supplied acknowledgments and stored receipts do
not authorize execution. Unmanaged editors keep the old path and receive the
new path for explicit manual adoption.

The first profile uses a trusted native Unix command wrapper. The wrapper
acknowledges its actual physical working directory over its private output
pipe, before executing the operator appointed command. The parent owns the
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
paused until existing controlled commands have settled, or explicitly retain
those owned commands as attachments to their old generation.

Before launch, validate current controller and native acceptance, the complete
prepared checkout and the request mapping. After the actual wrapper reports its
physical root, repeat current authority and expiry checks before committing the
adoption receipt and session's current generation. No shared filesystem pointer
is swapped. Old workspaces, pending contributions and old child attachments
remain retained. Retirement and cleanup require a separate reviewed operation.

Only the current private handle may submit current agent results. Changed
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
attachments for explicit reconciliation under fresh authority.

## Required evidence

Use real native children to demonstrate initial physical root, open descriptors,
late old-root writes and watcher reads. Exercise pause between commands,
explicit old attachment, wrong owner/session, stale source/revision/epoch,
expiry, duplicate requests, damaged prepared bytes, and interruption before and
after native acknowledgment. Independent review and guard-removal checks are
required. E1 remains junior-owned; no complete composition qualification is
claimed by this contract.
