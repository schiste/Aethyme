# Local v3 qualification (#682)

Last Updated: 2026-10-11

## Scope and status

This package assembles Local qualification against plan v3 §15.1/§15.2. It
records exact source, execution environment, fixture profile, command, output
digest, denominator, failure classification, limitations and independent review
for each claim. It does not grant remote acceptance, adoption or publication.
The shared records and all composition-dependent runtime profiles remain
experimental/provisional until E1 (#650) replaces the provisional evidence.

The current checkpoint is a contract and evidence inventory. Q3 and Q4 are not
closed by this document. A failed or missing required positive blocks its
associated claim; independent legacy Local operations remain available.

## Qualification rows

| Local surface | Reviewable implementation | Qualification still to assemble |
|---|---|---|
| Revision-bound identity, records, briefs and analysis | L0/L1 (#704–#710) | Exact source and independent vector artifacts; AQ0 baseline only |
| Retained source, crash capture, roots and GC | L2 (#714–#719) | Cleanup/crash traces and resource-pressure preservation |
| Useful bounded contribution context | L3 (#718/#730), X2 fresh views (#731) | FX08/FX17 matched correct/absent/misleading briefs with full denominator |
| Candidate composition and bounded resolution | Provisional L4 (#733–#736) | E1 replacement and T16–T23 requalification; historical held-out trials are not rerun |
| Target revisions, finite waves, budgets and external epoch | L5 (#737–#740) | Q4 progress, stop, arrival and restore trace bundle |
| Exact execution, supervisor, trusted verifier, resources | L6 (#741–#744) | Q3 negatives/positives and actual bounded ENOSPC/OOM with GC/concurrent work |
| Source transport and provider observation | L7 (#745/#746) | Native gate join after #725; actual GitHub/Mergify needs designated Playground binding |
| Prepared generation, exchange and controlled agent handoff | L8 (#747–#749) | Final restored test/guard artifacts and real owned process receipts |
| Retained export and isolated source rehearsal | Export (#750) | Final guard artifacts; actual independent receiver build after origin deletion |
| Disable/version/old binary/offline/rebind behavior | Compatibility (#751) | Full current CLI parity and actual independently pinned old process runs |

## Runtime and pressure protocol

Use disposable Playground source fixtures and only owned temporary roots,
containers, volumes and child PIDs. Pin native executable, daemon, image,
execution and verifier policy identities. Keep finite candidate/job/byte/time
budgets. Preserve unacknowledged and unknown effects until native reconciliation.

Actual ENOSPC qualification may fill only a small owned tmpfs mount inside an
owned bounded Linux container. It proves workload-filesystem exhaustion, not
Docker daemon-storage exhaustion or global host-disk exhaustion. Pair it with
real retained CAS GC and an independent concurrent build; verify exact retained
bytes, rejected pass/artifact classification, and the owned cleanup inventory.
The same preservation/cleanup assertions apply to bounded cgroup OOM. No
condition may produce a verifier pass. A nonzero workload exit is reported as
CodeFailed by the current supervisor; cgroup OOM has a native runtime witness
and is InfrastructureDeferred. These two classifications are not equivalent.

All scheduled trials count, including infrastructure-deferred and unsupported
ones. A named test must actually run; compiler failures and zero-test runs are
not evidence. Guard-removal trials count only when the intended assertion fails
and exact source restoration is followed by passing checks. Reviewers use
separate read-only snapshots; review clearance is distinct from runtime evidence.

## Claim records

The evidence ledger will store independent per-command records with source
commit/tree, fixture/profile, host/runtime identities, exact argv and environment
names, exit status, output SHA-256, passed/failed/ignored counts, and explicit
claim boundaries. No credentials or private exploit details belong in public
artifacts. A transcript digest alone is not a test result or authority receipt.
A later parent fix requires applicable rows to be rerun or explicitly retained as
historical evidence of their old source version.

## Required rechecks

E1 must requalify all composition-dependent L1–L9 claims. #725 must land before
native verification candidate/gate integration. The operator must supply the
intended disposable GitHub/Mergify repository, branch and policy before live
provider qualification. Existing source receipts, provider observations,
metadata receipts, prepared roots, READY/adoption receipts and imported history
retain their separate meanings throughout these rechecks.
