# Read-only resource-pressure diagnostics

Last Updated: 2026-10-25

`aethyme broker advanced resources pressure --json` explicitly diagnoses
inactive recorded session owners. Routine status, successful resource admission
and acquisition retries do not run this report. A final resource-contention
refusal runs it once as best-effort advice. It never signals a process, terminates an agent, releases a
lease, deletes a worktree, or changes a session's status.

## Wire contract

The report schema is `aethyme.resource-pressure/v1`. All fields are present.
`session_count` is the observed live-session inventory, not historical rows.
`groups` contains at most 20 inactive-owner groups, ranked by known process-tree
RSS descending and then oldest inactivity. `omitted_groups` counts groups
outside the display. Each group includes at most eight
`inactive_session_ids`, with `omitted_inactive_sessions` counting the rest.

A recorded holder's PID, start time and complete normalized command must all
match the process sample before memory is attributed. Sessions sharing that
identity form one group. `active_owner_count` includes its active sessions;
such a group explicitly recommends reviewing individual sessions. Active-only
recorded holder roots also stop attribution to an inactive ancestor.

`agent_tree_rss_bytes` sums process RSS assigned to the nearest matching
recorded holder. It is not private memory, available memory, or a promise of
reclaimable bytes. Shared resident pages can occur in several RSS values.
`swap_bytes` is always null: this sampler does not measure swap. Unrecorded,
unavailable or mismatched identity yields unknown memory, represented by null,
not zero. No command text, environment or holder start time is emitted.

## Bounds and limitations

A process-table invocation has a two-second deadline and four-MiB cap on each
stream. Nonzero status, timeout, truncation, invalid UTF-8, malformed or duplicate
rows, overflow, more than 16,384 rows, incomplete ancestry, cycles, or ancestry
deeper than 128 produce unavailable memory rather than partial totals. At more
than 200 live sessions the report returns empty groups with
`memory_observation = session_inventory_over_budget`; it skips process sampling.
The inventory count is still read from the repository database.

The report joins recorded session holders, activity metadata and one process
sample. It does not run the ordinary per-session PID/group liveness probes or
start a separate fallback `ps` for each owner. Inactivity uses the newest
recorded command or Git metadata timestamp and the ordinary idle-age threshold;
recorded exited owners are inactive. Fresh activity protects an owner even
when its stored status says idle or stale. A recently active owner whose process
disappeared stays protected until the inactivity threshold. These are
observational recommendations, never a session-state transition. These are observations made at different instants, not an atomic host
snapshot. Missing holder records cannot establish sharing, and foreign agents
or repositories are outside this repository's owner inventory. Output cannot
authorize cleanup or establish that an idle agent can safely be terminated.
Owned-service registration, verified service/container teardown and actual
memory-pressure admission remain separate work under #765.

## Final admission refusal

`resources acquire`, `run` and `artifact-run` emit one JSON line on stderr
when final acquisition fails with `resource_contention`, in human and JSON
modes. The event uses `type = resource_contention_advice` and schema
`aethyme.resource-contention-advice/v1`. Stdout, the existing failure payload,
exit status and retryability are unchanged. Other refusal codes do not sample.
There is no extra sampling during the acquisition wait loop or on success.

Every event contains `reason = resource_contention_memory_pressure_unproven`,
`cleanup_authorized = false`, the explicit diagnostic `next_action`,
`observation` and `report`. A readable repository produces `available` with the
existing ranked pressure report; a command outside a broker repository or a
failed diagnostic produces `unavailable` and a null report. Diagnostic errors
never replace the acquisition error. An available report can still contain
unknown memory or an over-budget inventory; consult its own observation fields.

This does not establish that memory caused resource contention, and no process,
container, lease or session is stopped. Diagnostics execute after acquisition
has refused; their time is additional to the acquisition wait budget. The
existing process-sampling limits apply, but the complete repository read is not
a hard wall-clock bound. Long-wait proactive pressure notification remains
separate from this final-refusal advice.
