# Foreground watch tick provider budget

Last Updated: 2026-10-25

`aethyme broker advanced watch pr tick` shares one provider budget across its PR
polling pass, repository listings and automatic PR-watch initialization. The
existing tick lock still covers both passes. No daemon or scheduler is installed.

## Limits and reporting

Each tick permits at most eight GitHub invocations: four reserved for existing
PR watches and four shared by repository listings and automatic initialization.
One busy inventory cannot consume the other phase's reserved call allowance.
Each invocation retains its three-second maximum and four-MiB per-stream cap.
A monotonic thirty-second deadline starts immediately before the two passes;
each invocation receives the smaller of three seconds and the remaining budget.
No new provider dispatch starts after exhaustion. Calls that fail to spawn still
consume a slot, preventing an unbounded retry loop.

This bounds provider work, not total command wall time. Opening broker state,
waiting for the tick lock, SQLite work, parsing, output and subprocess cleanup
have their own costs. The shared deadline does not interrupt a database commit.
The generic library tick methods retain caller-selected providers; this CLI
budget does not wrap arbitrary third-party providers or single explicit polls.

JSON includes `provider_budget` with schema version, admitted calls, maximum
calls, per-phase allowance, deadline milliseconds and refused dispatch count.
Refused dispatch count is not a count of unique PRs or all outstanding work.
The PR result disposition `deferred_by_budget` is unattempted and included in
`deferred_watch_count`. Repository results add `budget_deferred`; their report
adds `deferred_watch_count`. A partially processed repository can have both a
genuine provider error and deferred initialization; both remain visible.
Consumers matching all disposition variants must accept the new variant.

## Resumption and authority

A listing refused before dispatch leaves its watch cursor, error and next poll
unchanged. No incomplete listing is interpreted as closed PRs or no activity.
PR watches refused before dispatch similarly preserve their persisted state.
The CLI's next tick includes both active PR and repository due times.

Repository events and delivery entries commit before automatic watch creation.
If creation exhausts the allowance, that recorded intent remains pending and is
retried on the next due repository poll without replaying events. A real provider
failure remains a failure; budget deferral never overwrites that result. Due
repository listings sort by their next due time and ID, allowing unpolled watches
to precede ones just processed. Successful prior auto-watch enrollments are
skipped on retry. Existing explicit pauses, stops and foreign owners remain
protected.

This change does not provide the built-in service or delivery transport still
requested by #887. It grants no merge, dispatch-to-agent or transcript authority.
