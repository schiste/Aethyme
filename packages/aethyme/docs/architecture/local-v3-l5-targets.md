# Local v3 L5 target revisions — provisional (#666)

The user authorized L5 against provisional L4. This internal library model
freezes Local targets and appoints one local controller; it does not qualify
the structural engine, execute candidate code, verify behavior or publish
source. Re-check its composition assumptions when E1 (#650) lands.

## Frozen inputs and authority (D12/D13)

`local_target::acquire_controller` is a trusted local operator entry point.
The first appointment persists the issuer. A different issuer is refused,
including after the original process exits. A non-clonable owner guard holds
an existing broker OS file lock for one project and target. Duplicate listeners
cannot acquire another controller; another target remains independently usable.
The guard cannot mutate another project's store or another target's revision.

This is cooperative authority within one host and OS user. It is not distributed
leader election or authentication against direct SQLite/filesystem writers.
No default controller, automatic spending or public CLI is introduced. A stopped
process releases the OS lock; it does not release retention or create acceptance.

`propose_revision` validates exact recorded lineage, selected prerequisite
closure, atomic groups, competing revisions and cycles. It pins the objective
authorization, exact baseline, selected contribution revisions, inherited
requirement record references, composition profile and acceptance policy under
`aethyme.target-revision/provisional-v0`. Its ID uses the existing canonical
record encoding and domain. Catalog, requirement and prerequisite sets are
normalized; selected application order stays significant. Duplicate aliases
for the same lineage are refused.

The trusted issuer supplies policy and requirement references. This package
checks their canonical IDs and freezes them; it does not attest those policy
records, resolve external authorization, or certify their behavioral evidence.
Those are later verifier/provider responsibilities. Missing retained source
refuses the entire proposal before any revision or root is committed.

A proposal can be stored while another revision runs. Explicit selection is
allowed only when disabled, idle, stopped or blocked by requirements/target
movement. It cannot mutate the target being composed, checked or reviewed.
The canonical record is re-read and its digest checked before use.

## State and truthful accounting

The model describes the plan's complete state relation:

```text
disabled -> idle -> admitting -> wave_frozen -> composing -> verifying
 -> ready_for_acceptance -> awaiting_provider_or_review -> accepted
 -> publication_confirmed -> idle
```

The current runtime supports only transitions through `verifying`. Entering
acceptance phases returns `VerificationUnavailable`: no caller can mark an
unverified candidate accepted through this API. Trusted gate wiring waits for
#725 and execution/evidence integration waits for L6.

Running states can hold for input, capacity or budget, block on requirements or
target drift, request stop, or report an unknown outcome. A resumable hold saves
its exact previous phase and resumes only there. An unknown outcome cannot be
resumed as success. Every transition compares the caller's observed version,
so duplicate or delayed listener work cannot advance the same observation twice.

Each requested contribution has a visible `pending` or `excluded` disposition
at proposal creation. The vocabulary also reserves `in_wave`, `blocked` and
`unsupported` for the scheduler. None means delivered, verified or accepted.
Accounting is neither convergence nor behavioral qualification.

Stop state here records scheduling intent, not proof that external processes
have terminated. Supervising external workers, epochs, late completion fencing
and restore recovery belong to #669/L6. Finite waves/fairness belong to #667;
budget reservations and unknown consumption belong to #668.

## Persistence and retention

Collaboration schema **7**, floor **7**, adds targets, immutable revision bodies,
per-contribution dispositions and target roots. It follows context migrations
5/6. Schema 6 reclamation cannot follow these roots, so older binaries must
refuse this store. The broker DB schema and gate flow are unchanged.

Migration 7 adopts the lazy `resolution_attempts` table from #665 with
`CREATE TABLE IF NOT EXISTS`; it preserves previously charged attempts.

Proposing stores the revision, dispositions and permanent baseline/contribution
roots in one SQLite transaction, while holding the existing shared archive
lock. GC follows these roots through the existing source/lineage/brief closure
and rechecks an old plan at apply. A missing rooted object blocks reclamation.

`retain_artifacts` atomically roots exact candidate snapshots and archived
manifest/request/recipe objects after checking their bytes. The caller must
supply all source snapshots that its records reference: this is an explicit
root API, not recursive interpretation of arbitrary record formats. Later
scheduler integration must retain artifacts before relinquishing its archive
read lock. A crashed worker, an expired lease or a stop does not end retention.

Only an appointed owner can explicitly release an **inactive** revision.
Its historical body remains readable; released revisions cannot be selected or
silently resurrected. Active revisions retain their inputs even when stopped.

## Evidence and remaining qualification

The tests use disposable Git fixtures and the real retained archive and GC.
They cover immutable target movement, dependency refusals, duplicate listeners,
issuer/project isolation, exact resume, stale transitions, refusal of premature
acceptance, old GC plans, explicit release, artifact pinning, digest corruption,
schema-floor/adoption behavior, and an actual child process killed while holding
the controller lock. The identity vector uses independently generated Node
canonical JSON and SHA-256 bytes; Node is not a product dependency.

These are model and persistence evidence for portions of T30/T32/T41. They are
not completion of T29's autonomous verified-positive fixture, T31 fairness,
T43/T44 managed budgets, D16 restore fencing or Q2/Q4. E1 structural evidence,
the actual verification candidate boundary and L6 remain required for those
claims. No held-out composition scenarios are rerun or claimed here.
