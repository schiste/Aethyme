# Bounded native source filesystem pressure (#682 / D25)

Last Updated: 2026-10-12

## Proposed qualification contract

This is a macOS test profile, pending implementation, independent review and
explicit execution. It supplies no current execution or acceptance authority.
Existing Docker workload tmpfs/cgroup evidence remains a separate profile.

Use one private temporary APFS image with a fixed 512 MiB logical capacity.
The native image client mounts it at an exact private directory outside the
repository. Never fill the host filesystem or a Docker daemon store. Preserve
the production capture reservation floor (256 MiB); no free-space override or
success-shaped simulated disk error qualifies this profile.

Before pressure, perform actual retained capture and verify independently
declared source bytes. Freeze an exact execution snapshot using the existing
native target APIs. Source Git repositories and the recovery journal live
outside the pressure volume. Record image identity, mountpoint and ownership
before any pressure write.

Bound pressure writes to the image's capacity plus one block. Observe an actual
kernel ENOSPC error. With the same physical full filesystem, require:

- A new capture refuses or fails with its actual typed storage/admission error,
  without an acknowledged receipt or loss of earlier retained source.
- A direct retained-object publication receives the native storage failure;
  it cannot publish a valid partial object as successful evidence.
- Exact source materialization fails without returning usable inputs or an
  artifact. This uses the existing materializer and native controller guard.
- Earlier acknowledged source remains reconstructable outside the pressure
  volume with its original byte identity and retention roots.

After removing only the owned pressure file, retry/recover with the existing
capture APIs, materialize the exact snapshot, and run actual retained CAS GC.
Check source bytes, absence of a false receipt, bounded temporary objects and
recovery idempotence. Each deliberate guard removal must fail its named
assertion; restore exact source before a final positive profile.

## Native client and cleanup boundaries

Native clients have finite deadlines and bounded output. Creating, attaching or
ejecting an image can have an ambiguous outcome if interrupted. Reconcile the
exact image path and native inventory before retrying or declaring it absent.
Never infer ownership from a device number or volume label alone.

Eject only the device freshly observed as belonging to this exact image and
mountpoint. Do not force eject, run global sweeps, or remove unrelated mounts.
Retain the independent recovery journal and image on an unconfirmed cleanup;
the profile remains failed/unqualified. A verified successful cleanup records
native absence. No failure erases the evidence needed to reconcile it.

## Claim limits

This profile targets physical filesystem exhaustion at Local retained capture,
object publication and materialization boundaries. It does not qualify a full
host disk, Docker daemon disk, arbitrary storage backend, mid-copy power loss,
or an unbounded stress campaign. Capture journal errors and reservation-floor
refusals must remain distinguishable from actual object/materialization IO.
Every scheduled attempt, compiler failure, assertion failure and cleanup
outcome stays in the denominator. No Q3/Q4 or whole Local completion claim
follows from a contract, an ignored test, or an isolated successful write.
