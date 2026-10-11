# Local v3 historical export and restore

Last Updated: 2026-10-11

Issue #679; §6.8, §10.8–10.9; D16/D20/D28; T36–T40/T57. This is the
provisional implementation contract. Implementation and qualification follow
in this draft; no full recovery or current authority claim is made here.

## Portable retained history

Select finite retained snapshots and canonical lineage, briefs, Local capture
receipts and advisory context explicitly. A closed profile exports the complete
selected raw source closure and canonical record bytes with their independent
identities and limitations. Native host configuration, credentials, locks,
producer bindings, active leases and database images are excluded. Exact source
can contain tracked secrets; changing those bytes would change its identity.
Selected unsupported verification, acceptance, adoption records or executable
recipes receive explicit diagnostics. This profile recovers source and advisory
history; it cannot reconstruct an authoritative accepted decision from them.

Export requires fresh Local authority and independently selected origin,
visibility and audience, with the archive reader guard held through complete
collection and publication. A source digest alone cannot authorize disclosure.

## Validation before binding

An independent receiving policy pins the exact bundle digest, expected original
project, visibility and audience. Validate bounded wire and decoded sizes,
canonical schemas, every object digest, manifest/path rules and the internal
record reference graph before creating a receiving import binding or writing
objects. CAP operation/contribution/base/result, lineage and source manifests
must agree. Missing objects, unsupported required schemas or changed scope
refuse with explicit diagnostics. Context limitations and unsupported referenced
evidence remain visible.

A private validated bundle is data, never a reconstructed native authority.
Import requires a separately appointed current receiving operator and preserves
source/record origin in dedicated historical indexes. Imported context never
populates current native applicability caches. Existing producer permissions,
source acceptance, agent assignments, cursor visibility and generation pointers
are unchanged. A receipt proves historical byte validation and retention only.

## Controlled rehearsal

Reconstruct content from sealed imported manifests and locally retained objects
into a new private receiver-owned absolute directory. Reuse archive source
materialization and canonical ancestor checks. Exclusive creation, durable seal
and fresh receiver authority through the final seal prevent partial or swapped
paths from becoming a completed reconstruction. No archived recipe, source hook
or imported policy executes automatically. An ordinary source build is exercised
only by an independently trusted qualification command in a disposable project.
The reconstructed candidate is exact content; current checks and acceptance must
be established separately.

## Recovery and qualification

CAS publication precedes the final historical import transaction. Interrupted
writes leave reclaimable unreferenced bytes, never a live partially bound project.
Idempotency binds the canonical bundle and independent receiving policy; a
historical duplicate does not refresh permissions. Explicit GC roots preserve
imported bytes. External epoch fencing and uncertain-effect reconciliation
remain the existing Local restore procedure; old exported epochs never revive.

Qualification must include an independent vector, separate-project source build
after contributor storage destruction, source/context digest and limitation
round trips, tampering, revoked authority, missing objects, schema/scope mismatch,
path swaps, actual GC, interrupted import and idempotency. Independent review and
named guard-removal failures are required. E1 remains separately owned; this work
makes no structural-composer or held-out evidence claim.
