# Local v3 authority fencing — provisional (#669)

Last Updated: 2026-10-15

> Historical checkpoint: the implementation and pending-work statements below
> describe the original provisional profile. Current bounded E1, FX08, native-gate
> and appointed-controller results are recorded in the
> [Local qualification ledger](local-v3-qualification.md). Each original fixture
> and profile keeps its own evidence and limits.


This cooperative reference boundary extends the authorized provisional L5
model. Plan §15.1 allows reference transitions on L1/L4. Live executor,
publisher, signing-key and restore qualification still require L6; no FX13,
D16/D29 closure or completed external-effect evidence is claimed.

## Witness and operation boundary

An authority witness and stable ingress lock live in
`<collaboration root>/.local-authority/<project>/`, outside the project's
restorable directory and `state.db`. Preserve that namespace independently of
project backups. The generation marker is not an authentication or signing
secret. Host resource ownership stays with the existing host resource ledger.

Controller owners carry the exact witness epoch. Target, wave and budget
mutations hold a shared ingress guard through their operation. Freeze obtains
that lock exclusively, waits for already admitted library operations, then
atomically writes a fresh frozen witness before touching SQLite. Fresh calls
refuse while frozen; missing, invalid or mismatched witness/DB epochs refuse.
This is cooperative same-user fencing, not protection from arbitrary host or
SQLite writers. Bounded control latency under sustained contention is not
qualified, and a library composition already admitted may finish before freeze.

## Stop, revoke and ambiguous effects

The trusted operator's `freeze` acts as the project kill switch and revokes
all existing controller handles. Undispatched frozen waves become stopped.
Outstanding composition/verification becomes `outcome_unknown`, pending budget
attempts become `consumption_unknown`, and affected targets remain
`stop_requested`. Freeze does not prove process death, refund spending, or
undo external effects. There is no new OS cancellation or publisher route.

An exact checkpoint covers target revisions, source roots, wave ledgers,
project/objective budgets, reservation/receipt lineage and resolution attempts.
It excludes the epoch binding itself and unrelated captured/derived data.
GC cannot plan, apply or resume reclamation while authority is frozen or its
DB binding differs. This preserves source while a restored root ledger is
being reconciled.

## Announced restore and resume

1. Quiesce old collaboration clients and call `freeze`; require its durable
   checkpoint result before restoring any project data.
2. Preserve the external witness/lock namespace. Restore project data only
   while ingress remains frozen; old workers may remain alive.
3. Reconcile authority data, consumed/unknown allowances and archived receipts.
   `resume` requires an exact checkpoint match. A rewind that loses decisions,
   costs or roots refuses; no unchecked acknowledgement flag bypasses it.
4. Rotate again, bind the DB to the fresh epoch, then reopen ingress. Acquire
   fresh controller owners after dropping old target locks. Unknown work still
   needs explicit accounting reconciliation and is never automatically rerun.

This module deliberately provides no automatic merger of missing historical
receipts into a rewound DB. A qualified operator/receipt adapter must repair
that data before resume. A crash before checkpoint publication stays frozen;
`complete_interrupted_freeze` may finish quarantine only against that exact
checkpoint-free token, before an announced restore begins. It cannot replace
an existing checkpoint. Interrupted resume also remains frozen and can be
retried against its current witness token without reviving the old epoch.

Budget attempt handles carry their issuing epoch. Even an identical target
revision and colliding restored row/version cannot make an old handle current.
Owner-supplied budget reconciliation remains accounting; it is not a receipt
producer, behavior test, acceptance approval or publication authority.

Late model-attempt observations can be retained in immutable historical slots
outside restored data. Replays are idempotent; different receipts cannot replace
a slot. These are explicitly unverified references and never alter target,
budget, verification or acceptance state. Trusted producer sealing remains L6.

## Evidence and limits

Reference tests exercise freeze/revoke, exact resume, missing/corrupt witnesses,
checkpoint rewinds, old live handles, ABA attempt state, interrupted freeze,
shared mutation locks, persistent unknown liveness and blocked GC. Actual
external worker supervision, historical evidence sealing, credentials,
publication, cancellation acknowledgement and restore behind a real external
effect remain L6-dependent. Unannounced administrative rollback or restoring
the entire host including the witness is outside the guarantee.

Collaboration schema/floor 10 adds the DB epoch binding. Older binaries do not
honor this witness; stop existing clients before migration and before restore.
The floor protects fresh opens, not clients already holding a SQLite handle.
No broker schema, queue or gate flow changes. #725 remains the typed candidate
boundary prerequisite, and E1 must requalify all composition assumptions.
