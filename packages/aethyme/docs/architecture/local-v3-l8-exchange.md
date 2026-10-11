# Local v3: resumable peer metadata exchange

Last Updated: 2026-10-11

Issue #676; plan sections 6.2, 10.2, 10.6 and 15.1 L8; D23/D26/D27.
This records the provisional Local adapter implementation contract. Controlled
Local qualification is recorded below; the Coordination service is separately owned.

## One local store and explicit peer binding

The existing collaboration `state.db` and capture outbox remain the durable
owners. Extend that outbox to bounded experimental metadata records while
preserving capture rows and their operation identity; leave legacy PR delivery
and broker.db unchanged. Bind each peer independently to its service identity,
project, audience, adapter profile and explicit local authority epoch. An operator
selects a trusted adapter; a received record cannot create such a binding.

## Delivery and source promises

Before delivery retain the exact canonical metadata and its local outbox intent
atomically with roots in the existing archive. Peer acknowledgment binds the exact
binding, message identity and current visibility generation. A lost response can
retry the same idempotency identity within a finite persisted allowance. Offline
or exhausted delivery preserves the retained message and typed uncertainty.

A metadata receipt never asserts source retention, verification, provider
acceptance or workspace adoption. Source availability is attributed separately:
local-only, provider-reachable, or an audience-specific remote retention claim.
The receiver must acquire and retain exact source separately before executing.

## Inbox, cursors and visibility

Treat incoming events as untrusted advisory metadata under an explicitly trusted
transport/authentication adapter. Streams are reader/scope/visibility-specific;
contiguous positions describe that stream, not global hidden project events.
Deduplicate exact identities, reject conflicting sequence reuse, buffer bounded
reordered events, and never advance across a missing stream position. Inbox
receipt, visible projection and cursor advance commit in the same SQLite
transaction. Notifications are hints and cannot certify processing.

Authorization is checked on each adapter operation and before local commit.
Visibility changes invalidate cached access; offline authorization never grants
cached access. An expired cursor obtains a bounded consistent complete snapshot
under the current visibility generation; snapshot projection and its resume
cursor replace the old projection atomically. Replayed old events or snapshots
cannot resurrect revoked access. This profile does not infer broad membership
from possession of a source digest, old token or serialized receipt.

## Required qualification

Controlled peer schedules exercise duplicate requests/events, missing and
reordered events, restart, lost acknowledgments, offline peers, finite attempts,
conflicting identities, cursor expiry, incomplete snapshot refusal, visibility
revocation during I/O, stale snapshot replay and atomic consumer rollback.
Adversarial review and deliberate guard-removal checks qualify the Local contract.
Real C1/C2 integration remains the separately owned Q5 path. No remote service,
credential rotation, native provider migration or handoff execution is implemented
here; #678 owns controlled executor acknowledgment.

## Review constraints fixed before implementation

Delivery attempts and acknowledgments are per `(peer binding, outbox message)`;
the old capture row's global `delivered_ms` is not remote delivery authority.
Generalized rows preserve existing capture operations without synthetic captures.
Cursor and snapshot witnesses bind reader, requested scope, visibility generation,
peer stream incarnation and authority epoch. A consistent snapshot pins identity,
item count and watermark; neither a short page nor a notification implies
completeness. Same-stream watermark rollback and mixed pages are refused.

The operator-appointed trusted adapter supplies a native current authorization
lease. The manager's witness is private and not deserializable; the native lease
must remain held and valid through local commit. Cached projection reads acquire
fresh authorization too. Known revocation atomically invalidates that binding's
projection, pending events and partial snapshots. Independently retained Local
source stays owned locally when the remote peer is offline or access is revoked.

## Implementation checkpoint

The profile is `aethyme.exchange.scoped-metadata/provisional-v0`. Opening the
store appoints no peer. Trusted operator code pins a native adapter, independent
service/project/audience/reader/scope, and the actual Local authority epoch.
Serialized bindings, events, cursors and acknowledgments are historical data.
The native authorization lease is held through each local commit; the exact local
binding is rechecked in that transaction, so an operator disable during native
I/O cannot restore current projection or acknowledge delivery.

Collaboration schema/floor 15 generalizes the existing outbox while preserving
capture operation constraints and every original row value. A real capture row
may receive an immutable typed metadata envelope checked against its capture
receipt. Its capture operation, sequence, payload receipt and global delivered
marker stay intact; native peer ACKs bind the envelope identity independently.
Staging a capture pins its authenticated receipt CAS object and immutable envelope
in the same transaction. Releasing captured source retention can reclaim source
bytes while those metadata records remain readable and deliverable.
Typed metadata and incoming event objects use existing CAS/object-pin GC roots.
Those pins retain metadata only, never offered source bytes or permissions.

Limits are 16 KiB per canonical record, 64 bindings, 1024 staged messages,
64 events/page, 256 buffered gaps, 1024 visible snapshot items, 4096 historical
events per binding and 1024 explicitly named observation operations. Each peer
message or observation operation has 1–20 durable attempts reserved before I/O.
The 1–30 second deadline is checked before and after trusted adapter operations; this
library does not forcibly interrupt arbitrary adapter code or claim a hard
process/memory boundary. Full snapshots stay private memory until all explicit
counted pages pass. A crash retries under the remaining allowance from the last
complete durable cursor.

`Delivery::AlreadyAcknowledged` reports retained historical acknowledgment
without refreshing native remote authorization. It promises no current source
access or visibility. Cached peer projections always require fresh native access.
Twenty-six controlled schedules passed, including real capture delivery,
schema 14 migration, lost ACK/restart, native and Local revocation, reorder/gaps,
forged ACKs, mixed/incomplete snapshots, buffer exhaustion and actual metadata GC.
Capture readiness, archive/index integrity, optional brief omission and expected
epoch are checked. The capture-retention schedule releases and reclaims source
bytes while proving the receipt and envelope remain deliverable as metadata.

Independent adversarial review is clear. Nineteen production guard removals each
caused a named test failure (exit 101); compilation failures and zero-test runs
were not credited. Exact source was restored. Separately, an attached in-memory
database fault proved the isolation fixture still rejects an additional database;
SQLite's transient empty-filename `temp` schema is permitted. Restored regressions
passed 423 selected tests with six ignored helper/live-profile tests, including
all 26 exchange schedules, 149 collaboration tests, existing execution and
publication checks, and discarded-result/spelling/documentation ratchets. Strict
Clippy for broker library and tests passed with warnings denied. These are Local
controlled-adapter results; live remote peer interoperability is the Q5 path.
