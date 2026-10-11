# Local v3 budget reservation model — provisional (#668)

Last Updated: 2026-10-11

This internal reference ledger extends the provisional L5 controller. The
work-package table in plan §15.1 permits L5 reference work on L1/L4; the actual
execution-dependent qualification in #668 still requires L6. The ledger does
not execute candidate code, invoke a model provider, certify behavior or issue
verification evidence. It is not a hard process limit or invoice guarantee.

## Stable authority and aggregate caps

One cooperative appointed issuer configures immutable project authorization,
a pinned price-profile reference and finite envelopes. An authorized target's
objective has one stable cap; re-registering its authorization cannot replenish
spending. Fresh objective or request IDs still share the project cap. A separate
host resource scheduler is not added: L6 must adapt the existing
`resources::HostResourceStore` for actual host capacity and resource ownership.

The reference policy protects recovery headroom from ordinary composition and
verification, and mandatory headroom from optional analysis. The issuer owns
work classification. Delegates inherit the class and recipe; they cannot turn
optional analysis into recovery to obtain protected allowance. These finite
lanes protect their declared envelopes, not unlimited future work.

Units cover execution time, memory, working disk, model request/input/output
allowance, estimated spend, slots and previews. They are conservative cumulative
envelopes: even measured memory/disk/slot usage remains charged after settlement.
Only unused remainder is released. This deliberately favors preservation over
reusing capacity without a qualified resource-cleanup receipt. Actual provider
usage and a pinned price interpretation are needed to settle monetary cost.

## Reservations, attempts and delegation

`reserve` atomically checks project, objective and priority limits before storing
a canonical reservation bound to exact target revision, wave, objective, recipe,
nonce, units, retry allowance and deadline. Replaying the nonce returns the same
reservation; different contents are refused. Neither replay nor closing and
re-registering an objective creates another allowance.

Delegation subdivides the parent allocation and preserves its root lineage.
Project accounting counts each root once. The lineage's attempt allowance is
shared by parent and descendants; every intermediate ancestor retains its own smaller attempt envelope. Descendant records are bounded independently of zero-spend recycling. A parent cannot dispatch while a child is
held; consumed child cost rolls into its parent once when that parent closes.

`begin_attempt` persists `completion_pending` before returning a non-cloneable
model attempt identity with the remaining envelope. Duplicate calls cannot
start another attempt. Retries preserve consumed units, original deadline and
finite attempt allowance. This is a reference boundary; it does not supervise
an OS process or constrain a provider credential.

## Consumption and unknowns

States are `reserved`, `partly_consumed`, `completion_pending`, `consumed`,
`released_unused` and `consumption_unknown`. A deadline or process disappearance
never releases unresolved consumption. Excess reported usage leaves the grant
unknown and held instead of pretending the declared cap was enforced. The observed overrun and receipt remain visible, and new project reservations are blocked, including fresh objectives and recovery. This provisional model cannot forgive an observed overrun with a smaller receipt; resolving a disputed observation needs a future trusted operator workflow.

Owner-supplied usage and receipt references are accounting inputs. L6 must
validate actual producer, assignment, profile, exact attempt and usage before
calling settlement. They grant no source, acceptance or publication authority.
An explicit reconciliation can account usage after a crash; stale attempts and
receipt replays cannot charge again. Receipt IDs are assigned once to an exact reservation and attempt in a persistent ledger; their contents still require the future trusted receipt adapter. A parent cannot close while descendants
remain live or unknown. Target selection and retention release are blocked while
any reservation under that target/revision remains held.

## Evidence and limits

Model tests also interleave concurrent admissions under distinct targets and fresh objectives, reopen pending work, and reject correctly rehashed delegation changes. Model tests cover immutable caps, idempotency, aggregate accounting, finite
retry lineage, delegated subdivision, protected lanes, stale settlement,
unknown consumption, deadline behavior, conservative overage and retention.
Project identity has an independent Node canonical JSON/SHA-256 vector; Node is
not a runtime dependency. These tests can qualify the reference state model,
not real external enforcement.

Collaboration migration/floor 9 adds the ledger. Schema 8 clients do not guard
reservation retention. Quiesce existing collaboration clients before upgrading;
a floor only protects fresh opens. No broker schema, queue or gate flow changes.

The module is a provisional library; it does not enable managed spending or
wire a new executor into the gate. Enforced time/memory/disk limits, trusted
usage receipts, provider billing, host-grant integration and resource saturation
against live workers remain L6-dependent. T43–T44/D15/D25 and full L5
qualification remain open. E1 must requalify composition assumptions, and #725
still owns the typed verification boundary prerequisite. Held-out composition
scenarios are not manually rerun or used to tune this module.
