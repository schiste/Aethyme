# Explicit Local command surface (#680)

Last Updated: 2026-10-12

## Contract and qualification boundary

This draft completes the command surface available from the reviewed Local
packages. Records and composition remain provisional. Installing Aethyme does
not enable collaboration, run a controller, appoint a peer or launch an agent.
The existing required/advisory policy reader remains authoritative. Disabled
commands refuse before reading argument files or opening writable state.

Commands run in the foreground. JSON output is versioned and distinguishes
historical observation, operator intent and current native authority. Serialized
input never becomes an acceptance, execution, peer or adoption capability.
No command reports success for an unavailable runtime profile.

## Intended commands

| Command | Concrete effect and boundary |
|---|---|
| `target status --target NAME` | Read existing target revision, phase and version; no state creation, migrations, epoch acquisition or controller appointment. |
| `target propose --target NAME --issuer NAME --file PATH` | Bounded strict TargetSpec input; acquire the existing native controller lock and archive an immutable proposal. Does not select it or dispatch work. |
| `target select --target NAME --issuer NAME --revision ID` | Existing exact revision selection, issuer binding and active-wave/budget refusal. |
| `target hold --target NAME --issuer NAME --version N` | Existing compare-and-swap hold; no cancellation or acceptance. |
| `target resume --target NAME --issuer NAME --version N` | Existing compare-and-swap resume of the recorded previous phase. |
| `authority status` | Read the checked native witness and ledger binding without creating files, locking, initializing or rotating authority. Distinguishes unbound, active, frozen, missing witness and drift; this is an observation, never a guard. |
| `stop --epoch EPOCH --reason TEXT` | Existing project freeze with the exact current epoch; cooperative ingress closes and authority rotates. Running processes and external effects are not assumed cancelled. |
| `export --target NAME --issuer NAME --source ID ... --record ID@SHA256 ... --audience NAME --output PATH` | Existing bounded, closed historical bundle; exclusive output creation, exact bundle digest and explicit source/advisory-only limitations. No overwrite or automatic source release. |
| `handoff show --id ID` | Existing retained-request integrity check and historical state/receipt identifiers. Observation never validates current agent authority. Opening the existing store can perform migrations and is disclosed. |
| `sync status` | Read existing binding/message/event/stream counts as local bookkeeping, without claiming remote delivery or source retention. |

Read-only status commands use the existing checked read-only snapshot. Missing
or older tables produce a typed unsupported/migration-required result; looking
does not initialize them. Mutation commands open only an already initialized
store and use the corresponding package's native guard and invariant checks.

`target run`, remote `sync` and agent handoff dispatch need an explicitly
qualified appointed adapter. This CLI must refuse those unavailable profiles
without changing state. Their implementation cannot consume serialized receipts
as authority. Native gate integration still waits for independently owned #725;
structural composition qualification still waits for E1 #650. Neither dependency
is implemented or claimed by this slice.

## Qualified finite CLI profile

The [CLI command and guard ledger](local-v3-qualification/cli-restored-2026-10-12.json)
records tested source `ece13cebd381ada85fa649a40b6b0e38eec209fb`.
Eight lifecycle unit tests and 18 built-router tests passed. Ordinary help
snapshot comparison passed separately from snapshot regeneration. Seven
deliberate guard removals each failed its intended named assertion with actual
exit 101; the three affected source files were restored byte for byte.

The restored selections record 527 passing executions, zero test failures and
13 explicit ignores. These overlapping counts include the lifecycle/router/help
cases. Strict broker Clippy (`--lib --tests`) and CLI Clippy (`--bins --tests`)
passed with `-D warnings`. One earlier CLI `--lib` invocation failed because the
package has no library target; it is retained and uncredited. The initial router
compile failure and a later shell-wrapper exit 127 are likewise preserved as
separate outcomes, rather than credited as successful runs.

The real router cases prove explicit proposal/selection/hold/resume, actual
issuer and controller ownership, read-only observation, stale-version/epoch
refusal, exact native freeze, unavailable-adapter refusal and exclusive export.
Export is verified and imported in a fresh receiver, where the exact requested
source is reconstructed from the delivered bytes. Origin source remains retained.

Historical handoff projection is covered by a unit fixture using modeled
trusted-verifier evidence. This is not a built-CLI positive retained-handoff or
real-provider qualification claim. The router covers its missing-entry refusal.
The finite current macOS/Unix profile does not qualify a daemon, an autonomous
CLI runner, remote synchronization, live GitHub/Mergify or native gate wiring.
E1 and #725 remain separate prerequisites. Independent review and final-head CI
are separate checks; neither an ignored profile nor a historical ledger grants
current authority.

## Appointment and exact epoch rules

Only a valid explicit `target propose` may appoint a new target. Parse and
validate the entire bounded proposal before any controller acquisition.
Select, hold, resume and export require an existing exact target/issuer before
acquisition; unknown targets and mismatched issuers refuse without creating a
row or initializing authority. The ambient trusted local operator authorizes
these commands; `--issuer` is an identity label, not authentication.

`authority status` observes the existing checked read-only snapshot and the
bounded external witness using the shared native authority reader. It does
not open writable state or create an ingress lock. The output names whether
the witness matches the existing ledger binding; an epoch observed here never
grants a guard. A concurrent change is caught by the existing exact-epoch
freeze check. Tests must cover observation then successful exact stop, stale
old epoch refusal and byte-identical state/witness after inspection.
