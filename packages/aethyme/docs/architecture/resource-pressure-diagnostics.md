# Read-only resource-pressure diagnostics

Last Updated: 2026-10-25

`aethyme broker advanced resources pressure --json` explicitly diagnoses
inactive recorded session owners. Routine status and resource admission do not
run this report. It never signals a process, terminates an agent, releases a
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

The report joins recorded session holders, derived activity and one process
sample. These are observations made at different instants, not an atomic host
snapshot. Missing holder records cannot establish sharing, and foreign agents
or repositories are outside this repository's owner inventory. Output cannot
authorize cleanup or establish that an idle agent can safely be terminated.
Owned-service registration, verified service/container teardown, automatic
contention diagnostics and actual memory-pressure admission remain separate
work under #765.
