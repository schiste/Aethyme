# Local v3: resumable peer metadata exchange

Last Updated: 2026-10-11

Issue #676; plan sections 6.2, 10.2, 10.6 and 15.1 L8; D23/D26/D27.
This records the proposed Local adapter contract. Implementation and qualification
remain pending; it does not implement or qualify the Coordination service.

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
