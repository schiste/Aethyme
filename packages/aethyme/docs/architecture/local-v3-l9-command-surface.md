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
| `stop --epoch EPOCH --reason TEXT` | Existing project freeze with the exact current epoch; cooperative ingress closes and authority rotates. Running processes and external effects are not assumed cancelled. |
| `export --target NAME --issuer NAME --source ID ... --record ID@SHA256 ... --audience NAME --output PATH` | Existing bounded, closed historical bundle; exclusive output creation, exact bundle digest and explicit source/advisory-only limitations. No overwrite or automatic source release. |
| `handoff show --id ID` | Existing retained-request integrity check and historical state/receipt identifiers. Observation never validates current agent authority. Opening the existing store can perform migrations and is disclosed. |
| `sync status` | Read existing binding/outbox/inbox/cursor counts as local bookkeeping, without claiming remote delivery or source retention. |

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

## Validation required before implementation delivery

The draft contract is not runtime evidence. Required tests cover disabled and
malformed policy before file access, read-only state/epoch byte preservation,
strict finite input parsing, stale versions/epochs, issuer/controller ownership,
active-wave refusal, actual historical export and retained request integrity,
exclusive output creation, unsupported adapter refusal with no side effects,
versioned JSON and help snapshots. Each fix needs a named negative and restored
positive. Use disposable Playground repositories only. An independent reviewer
must check the implementation in a separate read-only exact snapshot.
