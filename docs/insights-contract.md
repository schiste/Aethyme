# Aethyme Broker — Insights Report Contract

Version: schema_version **1**
Source of truth: `rust/crates/aethyme-broker/src/insights.rs` (types and
semantics) and `src/cli/insights.rs` (the `--json` and text surfaces).
Enforcement: `rust/crates/aethyme-broker/tests/insights.rs`.

This is a separate contract from
[`events-contract.md`](events-contract.md). The event contract freezes what the
broker *records*; this one freezes what it is willing to *claim* about those
records.

```bash
aethyme broker advanced insights                          # last 30 days, text
aethyme broker advanced insights --days 0 --json          # all history, JSON
```

## Why the report is mostly absences

The broker has real history to draw on — in this repository, three months of
769 sessions and 137k events. It is easy to build a dashboard from that and
have every number mean something slightly different from what a reader assumes.
So the contract below is mostly about what the report refuses to say.

## The five rules

**1. Wall-clock is reported next to active time, never instead of it.**

`sessions.wall_clock_ms` is `closed_at - created_at`. It includes every pause
inside the session: overnight, a blocked gate, a human thinking. Over this
repository's own history it averages ~46 hours against a median inside the
1-4 hour band. Publishing it alone would be a claim about how long developers
take.

`sessions.active_ms` is the sum of recorded periods of attention, and is `null`
rather than `0` for any session with no activity history — which is every
session recorded before activity tracking existed. A missing measurement and an
instantaneous one are different facts.

`sessions.idle_ms` is `wall_clock_ms - active_ms`, and is `null` unless **both**
are known. Subtracting from an unknown does not produce the idle time.

**2. Active time is meaningless without the gap that produced it, so the gap is
in the report.**

`idle_gap_ms` is a top-level field (and the first line of the text output). A
signal within the gap extends the current period of attention; a signal after it
closes that period at the previous signal and starts a new one. Fifteen minutes
is a choice, not a constant of nature, and a reader comparing two numbers computed
at different gaps would be comparing different things.

The same gap decides how a session's last period ends when the session closes.
A close within the gap is the agent finishing its own work, and the period runs
to the close. A later close is housekeeping (cleanup, the sweep, abandonment,
often a day on), and the period ends at its last signal: the silence before a
housekeeping close is not attention.

**3. A funnel is a count. A rate needs a denominator the caller supplied.**

`funnel.registered` … `funnel.pr_merged` count sessions that reached each
stage. `funnel.per_day` is **absent** unless `--days` was passed: a rate over an
unbounded log would divide by however long the database happens to exist.
`--days 0` (all history) therefore always has no `per_day`.

**4. Durations carry their distribution, and refuse a percentile they cannot
support.**

Every duration is a `DurationSummary` with `count`, `total_ms`, `mean_ms`,
`p50_ms`, `p95_ms`, `max_ms`. Below `DISTRIBUTION_FLOOR` (5) samples, `p50_ms`
and `p95_ms` are `null` — a p95 over two observations is one of the two. An
empty summary has `mean_ms: null`, never `0`.

**5. This report describes the broker and the pipeline. It is not a measure of
any person.**

`sessions.agent_identity` exists on the session row and is deliberately **not**
selected by the query behind this report. Nothing here ranks sessions, scores
agents, or attributes a duration to an identity. `sessions.agent_identity` would
answer "how long does someone leave a worktree open", which is not work.

## One window for every section

`--days <n>` (default 30) bounds **every** figure: sessions by their creation,
pull requests by their milestones, and gate runs, cache hits, coordination events
and coordinated operations by when they happened. `--days 0` is all history for
all of them. A report never puts a windowed funnel beside lifetime gate figures.
A negative `--days` is a usage error rather than a silent all-history report.

## Stages and outcomes

`stage` is the furthest point reached, in this order:

| Stage | Reached by |
|---|---|
| `registered` | `session.registered` |
| `submitted` | `merge.submitted` |
| `verified` | `merge.verified` |
| `landed` | `merge.promoted` **or** `merge.externally_landed` |
| `pr_opened` | a linked pull request with a recorded open time |
| `pr_merged` | a linked pull request with a recorded merge time |

`merge.externally_landed` counts as landing. The work reached the default branch
by some route, and excluding it would understate the pipeline by exactly the
sessions a human had to step in for.

`outcome` is how the work ended:

| Outcome | Meaning |
|---|---|
| `landed` | reached `landed` or beyond — **wins over every stopping event** |
| `rejected` | `merge.rejected`, and nothing later landed |
| `superseded` | `merge.superseded`, and nothing later landed |
| `conflicted` | `merge.conflict`, and nothing later landed |
| `in_flight` | live, with queue work that has not landed |
| `unsubmitted` | never submitted |

Two decisions here are worth stating because they cut against a naive reading:

- **A recovery still counts as a landing.** A session rejected once and landed on
  a later attempt is `landed`. Counting it as rejected would understate the
  pipeline on precisely the sessions whose first attempt was wrong.
- **Among stopping events, the earliest wins.** Work that conflicted and was then
  superseded never got as far as being superseded.

`funnel.unsubmitted` counts sessions that never reached `submitted`. It is the
funnel's own view and is not the complement of the outcome counts — a rejected
session did reach the queue.

## Coverage: what was measured and what was not

`coverage` exists because backfill is incomplete and a number without it lies.

| Field | Meaning |
|---|---|
| `sessions` | sessions the funnel counted |
| `sessions_with_activity` | sessions with at least one recorded activity signal |
| `sessions_without_activity` | sessions with **no** history — active time unknown, not zero |
| `prs_with_open_time` | pull requests with a recorded open time |
| `prs_without_open_time` | pull requests seen without one — time-to-merge unknown |

Two capture gaps are permanent for existing history and are reported rather than
papered over:

- **Activity intervals** only exist from the release that introduced them.
- **Pull request open times** only exist once `gh pr view` was asked for
  `createdAt`. A pull request that opened and closed between two watches has no
  local record of ever existing.

## Pull request lifetime

`pull_requests[].to_merge_ms` is `merged_at - opened_at`, and is `null` unless
both are known. Milestones are first-seen-wins: a later poll cannot make a
recorded instant later, and a poll that reports a merge does not erase a
recorded opening.

## Gate reliability

`gates` and `gates_by_name` separate three durations that were previously
conflated into "the gates took N seconds":

| Field | Meaning |
|---|---|
| `execute_ms` | the command itself |
| `wait_ms` | waiting for an owner lock, host resources, or cache prep |
| `first_output_ms` | spawn to first byte |

`wait_duration_ms` and `first_output_ms` have been written on every
`gate_results` row since schema v14 and read by nothing; this report is their
first consumer. Without it, a 90-second gate is indistinguishable from a gate
that waited 89 seconds for a lock and ran for one.

`pass_rate` is `passed / runs` over **executed** runs only. A cache hit avoided
the command entirely, so folding it into either side would move the rate for a
reason unrelated to whether the code passes. `None` when nothing ran.

## Coordination

`coordination` counts the events that mean two sessions or a gate got in each
other's way: `overlaps_warned` (`lease.overlap`), `conflicts_caught_pre_gate`
(`merge.conflict`), `out_of_lease_writes` (`guard.out_of_lease_write`), and gate
failures classified `environment`, `resource_contention`, or `timeout` — failures
that are not the code under test.

`operations.outcome_unknown` deserves attention when non-zero: a coordinated
write exited non-zero after possibly applying partial effects. Those operations
should be reconciled with `aethyme broker advanced operations reconcile` before
the counts are trusted.

## Truncation

`sessions`, `sessions_total`, `sessions_truncated` (and the matching three for
pull requests) are always present. A capped list that did not say it was capped
would read as a complete one. The funnel itself is **never** truncated by these
limits.

## Privacy

The session projection behind this report selects `id`, `origin`, `created_at`,
`closed_at`, and a derived in-flight flag. It does not select task text,
worktree paths, branch names, commit SHAs, agent identities, or commands, so
none of them can reach the JSON even by accident. `tests/insights.rs` asserts
this at the serialized-report boundary.

For an export intended to leave the machine, the stricter allowlist in
[`packages/aethyme/docs/pilot/metrics-export.md`](../packages/aethyme/docs/pilot/metrics-export.md)
still applies and is not weakened by this command existing.

## Change policy

Same rule as the event contract, applied to this report:

- Adding a field, or a new stage or outcome, is **additive**. Fine at v1.
- Renaming or removing a field or a stage, changing a type, or changing what a
  figure *means* requires bumping `INSIGHTS_SCHEMA_VERSION`.

Procedure: bump `INSIGHTS_SCHEMA_VERSION` in `src/insights.rs`, update
`docs/insights-contract.md` and `tests/insights.rs` in the same commit, and keep
a short note on what v1 said differently so a mixed-version reader stays sane.
