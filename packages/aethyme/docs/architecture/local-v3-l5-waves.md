# Local v3 finite candidate waves — provisional (#667)

Last Updated: 2026-10-11

This internal reference driver builds on the provisional target model in #666
and the provisional text composer in #664. It composes exact retained inputs;
it does not run candidate code, certify behavior, spend model allowance or
publish source. E1 (#650) must requalify its composition assumptions.

## Authority and finite admission

An appointed target owner calls `available` for an exact already-selected
contribution. Availability is idempotent by target revision and contribution;
duplicate notifications preserve its first observed sequence and time. It
cannot authorize another contribution, profile or target policy. Incoming
captures can propose later target revisions under #666 without changing a
frozen wave. This is a local cooperative controller, not distributed election.

The real composer planner derives conservative dependency closures, including
declared prerequisites, retained source ancestry and atomic replacements.
Only fully available groups become ready. Their last prerequisite arrival
defines their readiness sequence; repeated notifications cannot reset age.

Admission chooses the oldest ready group. The default batch width is three;
inseparable prerequisites can exceed that width up to a declared group bound.
An old or large admissible group gets a dedicated wave. Compatible ready
groups can share remaining batch slots. Compatibility here is dependency
planning, not proof that source changes have no behavioral interaction: actual
composition can still report a conflict. An oversized group receives a stored
blocked disposition, with the target awaiting an operator decision.

Debounce is measured from the oldest ready group's fixed readiness time, so
later arrivals do not slide the deadline. Exact selected inputs, baseline,
objective authorization, issuer, target revision, sequence and scheduling
policy become a canonical immutable wave record. A partial unique index allows
one active wave per target. Admission and in-wave dispositions commit together.

## Dispatch and honest results

`tick` is a library reference driver for a trusted event callback or timed
call. It derives its next action from persistent state; missed notifications
do not require a new composition request. This package introduces no daemon,
default automatic controller or public CLI. The application must configure
the appointed owner and drive its timer. Initial objective authorization and
the automatic selection of future target revisions remain integration work.

Before invoking Git, dispatch persists `composing`. The provisional composer
actually constructs the candidate and recipe, writing no ref. A candidate's
exact snapshot and recipe object are rooted under the existing target while
the shared archive lock remains held. Its result is `RequiresVerification`;
the controller holds in `waiting_for_input`, with exact resume phase
`verifying`, because the trusted verification adapter is unavailable.

Repeating a completed dispatch returns the same stored candidate. A dispatch
left `composing` after a crash is explicitly recovered as `outcome_unknown`;
it is never silently retried. The provisional composition retry allowance is
zero. Bounded retry ownership and managed resource reservations belong to
#668 and execution receipts to L6. A conflict, unsupported operation or
no-change result gets an explicit disposition, never a completed obligation.
Independent pending groups can receive a later wave after such a terminal
composition outcome. Verified acceptance remains unavailable through #666.

One timed driver test composes three already-retained inputs using the actual
composer and checks their bytes. This is plumbing evidence. It does not pass
T29's complete fixture of submissions after initial objective setup, trusted
verification, preview and required acceptance policy.

## External movement, stop and retention

`observe_target` records an explicitly supplied external commit observation.
It neither reads a provider nor changes the frozen baseline. Duplicate
observations consume no additional recheck allowance. Repeated distinct
movement reaches `blocked_target_drift` after the wave's finite allowance.
The caller must retain the existing provider guard; these observations are
not acceptance evidence. Automatic re-simulation against a newly authorized
baseline and provider integration remain pending #725/L6.

Stop prevents a frozen or verification-held wave from dispatching again. An
in-flight or interrupted compose remains unknown and active; stop is not proof
of process death. A durable active/unknown wave blocks both selecting another
revision and releasing its retention, even if a generic model transition marks
the controller stopped. Full external cancellation, authority epochs, restore
and late worker receipts belong to #669/L6.

Collaboration migration 8 stores availability and wave ledgers, with floor 8.
Schema 7 clients do not understand unknown wave retention. Quiesce all existing
collaboration clients before migration: floors protect fresh opens, not
already-open older handles. Broker schema, queue and gate files are unchanged.

## Evidence and limits

Tests exercise idempotent arrivals, fixed debounce, oldest-ready admission,
dedicated long/aged groups, active-wave protection, source prerequisites,
atomic replacement, real composition and archive reconstruction, no ref writes,
duplicate dispatch, external drift, unknown crash intent, stop and stale
revision guards, explicit dispositions and corrupt-record refusal. Wave identity
has an independent Node canonical JSON/SHA-256 vector; Node is not a product
dependency. `composition_ms` measures the actual local composer call.

These are reference-model and persistence checks. The sustained-arrival tests
repeat authorized availability notifications around finite retained catalogs;
they do not measure completed service under an unlimited ingress stream or
long-running verification. D14 stays open until FX10 demonstrates that service
and reports stale verification waste under stated resource/provider assumptions.
T29–T32, T43–T44 and full L5 qualification remain incomplete. No held-out
composition scenarios are rerun or claimed by this package.
