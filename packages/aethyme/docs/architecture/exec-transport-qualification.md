# Command transport qualification

Last Updated: 2026-10-24

This record qualifies the Aethyme boundary of #713 using only synthetic
Playgrounds and the installed paired runtime, version 0.8.30 at revision
`05c55d3093a1014a31c58a24a6ccf41ecc610c9e`. No local compiler, provider
model or real build was invoked. It does not test or change Hermes.

## Observed guarantees

| Case | Observed result |
|---|---|
| Child exits zero | Command success, completed receipt, verified group cleanup |
| Child exits seven | Exact exit code seven, completed receipt, verified group cleanup |
| Broker deadline expires | Typed command_timed_out, completed receipt, verified group cleanup |
| Adapter stops waiting while broker stays alive | Running receipt discoverable; same ID later completes with success and verified cleanup |
| Broker caller is killed | Same receipt remains queryable as running, outcome unknown, process_revalidation not_revalidated |

The first three probes used real `broker advanced exec` commands and queried
their exact `execution_receipt_id` through `exec-status`. The fourth probe
deliberately stopped waiting before releasing a short bounded child; it then
queried the running receipt and recovered the eventual command report. The
fifth killed only the exact broker process created by the probe. Its synthetic
child had a natural bound and an explicit release marker. Receipt observation
alone was not used as proof that a process had stopped.

Raw command receipts and results are retained with the deployment qualification
evidence. The repository regression
`exec_guard_cli::an_early_returning_caller_can_query_the_same_receipt_through_completion`
covers the live-broker case. The existing
`disconnected_exec_keeps_a_running_receipt_queryable_without_revalidation`
covers the killed-caller observation. The new regression's native execution
is pending CI and combined local qualification.

## Remaining boundary

A terminal adapter's own deadline is separate from the broker's explicit
`--timeout`. An adapter that returns early must preserve the broker invocation
or discoverable receipt handle and query it, rather than label the wrapped
command complete. Callers can list session receipts if the final CLI report was
lost before its receipt ID was returned.

A killed broker cannot itself attest final completion or cancellation. The
current durable receipt preserves uncertainty; `exec-status` does not turn
process absence into an exit code or replay stale PIDs as authority. Existing
owned-group supervision provides the cleanup mechanism, but an observational
receipt does not independently verify its final result after caller loss.

Issue #713 therefore remains open for a durable final-outcome/cleanup observer
and its failure tests. That observer must bind the exact execution identity,
survive caller loss, distinguish unverifiable cleanup and never signal a
recycled PID. The probes above do not qualify that missing observer or the
upstream terminal adapter's requested-versus-actual timeout behavior.
