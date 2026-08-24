# Broker Follow-Up Workflows

Last Updated: 2026-08-23

This guide covers broker workflows that matter after the basic
start-edit-submit loop: reusing a session worktree, choosing fresh or cached
gate evidence, understanding normalized submission planning, inspecting
advisory semantic gate suggestions, planning leases before claiming them, and
leaving or retrieving a durable finish handoff. For the complete flag
inventory, see the [CLI reference](../reference/cli.md).

For concurrent validation that needs ports, Docker namespaces, databases, or
host-capacity slots—including the opt-in repository-owned pre-push adapter—see
[Concurrent Host Resource Coordination](host-resource-coordination.md).

## Safety Model

Each workflow separates observation from mutation:

| Need | Inspect first | Explicit mutation | Refusal boundary |
| --- | --- | --- | --- |
| Continue in an existing worktree | `broker integration status` | `adopt --reuse`, optionally with `--sync-integration` | synchronization requires a clean, fast-forwardable worktree |
| Prove the current tree | default gate run and its tree provenance | rerun with `--no-cache` | a bypass never substitutes an older cached result |
| Inspect graph-derived gate hints | `gates semantic` | none; suggestions remain advisory | only changed-path triggers reach `gates run` and `submit` |
| Recover external main movement | `integration reconcile --dry-run` | `--apply --confirm <plan-digest>` | every unrecorded SHA needs a reviewed disposition; drift invalidates confirmation |
| Reserve paths | `leases plan` | `leases claim` | the claim, not the plan, decides whether a conflict exists now |
| End or recover a session | `finish` report | successful `finish` closes and records the handoff | dirty or unsubmitted work refuses the finish |

These commands coordinate local repository state. Submission promotes only to
the local `aethyme/integration` branch. Use the separate
`broker ship plan`/`broker ship execute` lane when publication is authorized.

## Recover External Main Movement

Status warns as soon as the configured upstream contains the first commit that
broker-managed integration does not contain. This commonly happens when a
release or deploy workflow writes changelogs directly to main. Do not merge or
reset refs by hand: update the remote-tracking ref through the coordinated Git
lane, then inspect a reconciliation plan:

```bash
aethyme broker git --session 111 --reason "inspect upstream for recovery" -- \
  fetch origin main
aethyme broker integration reconcile --upstream origin/main --dry-run --json
```

The plan classifies upstream-only external commits, recorded promotions, exact
and patch-equivalent landings, unrecorded integration commits, pending queue
entries, and ambiguous equivalence. Full SHAs in JSON let an operator trace
each decision. Cold evidence never becomes a guess: ambiguous matches and
replay conflicts return a successful read-only report marked unsafe.

Every unrecorded integration commit must be reviewed individually in a schema-2
resolution file:

- `preserve_and_replay` keeps the commit's delta and can still report a real
  conflict against upstream;
- `replaced_by_exact_upstream_sha` names the full reachable upstream commit
  that replaces it;
- `drop_because_content_empty` is accepted only for a commit with no tree
  change.

There is no “discard unknown work” disposition. The file also binds the exact
upstream and integration tips, so it becomes stale when either moves. See the
[CLI reference](../reference/cli.md) for the complete schema and queue-entry
attestation fields.

When the dry-run is safe, review the complete report and copy its 64-character
`plan_digest` into the apply command:

```bash
aethyme broker integration reconcile \
  --upstream origin/main \
  --resolution-file reconciliation.json \
  --apply \
  --confirm <reviewed-plan-digest>
```

Apply recomputes the plan from the current refs before comparing the digest.
It rebuilds integration from the current upstream tip, replays preserved work
in original first-parent order, updates queue records, and journals the digest
through a crash-recoverable two-phase transaction. A missing or mismatched
confirmation changes nothing. If a crash leaves an intent behind, the next
broker open completes it only when the ref reached the planned new tip, aborts
it only when the old tip remains, and otherwise requests explicit recovery.

## Reuse A Worktree Safely

Use reuse when a dedicated broker worktree should continue with a follow-up
task. Start by checking whether integration advanced while the session was
working:

```bash
aethyme broker integration status
cd /path/to/the/session-worktree
aethyme broker adopt --reuse --task "Address review feedback" --json
```

The adoption report says what actually happened with `outcome`: `created`
means a closed session produced a new session ID on the existing worktree,
`reused` means the active session identity stayed the same, and `replaced`
means a stale active registration was replaced. With `--reuse`, JSON also
includes `integration_drift`: the full session and integration HEADs, their
`current`, `behind`, `ahead`, or `diverged` relation, ahead/behind counts,
overlapping changed paths when available, a warning, and a safe next action.

Plain reuse reports drift but does not silently move the checkout. To begin the
follow-up at the current integration tip, request guarded synchronization:

```bash
aethyme broker adopt --reuse --sync-integration \
  --task "Address review feedback"
```

`--sync-integration` requires `--reuse`. It checks that the session worktree is
clean, permits only a fast-forward, and performs that fast-forward before
choosing the new diff baseline. It refuses dirty or diverged worktrees and
leaves them unchanged. If the report says the session is ahead or diverged,
follow its `safe_next_action`; do not force the worktree onto integration.

## Choose Gate Cache Policy Deliberately

Gate results prove an exact Git tree. Normal gate runs use the cache when the
same gate has already passed or failed for that tree:

```bash
aethyme broker gates run --session 111
aethyme broker gates run --session 111 --json
```

Text output shows an abbreviated 12-character tree hash. JSON returns the full
hash in `tree_hash` and identifies whether the result was executed or cached.
Before acting on any result, compare that tree with the tree you intend to
submit.

Use a cache bypass when fresh execution itself is required—for example, after
repairing an external dependency or validating a flaky-environment
hypothesis:

```bash
aethyme broker gates run --session 111 --no-cache
aethyme broker gates run --all --no-cache --json
aethyme broker submit --session 111 --no-cache
```

`--no-cache` bypasses lookup for that run; it does not disable storage or erase
older evidence. The newly executed result is stored normally and can satisfy a
later default-policy run for the same tree. Submit threads the same policy into
its merged-tree gates, so use the flag there when the landing decision requires
fresh evidence.

Managed pre-commit hooks follow the same diagnostic principle. Successful
cheap gates stay quiet. A failure replays complete standard output and error,
prints the broker diagnosis, and preserves the failing exit code.

## Understand Normalized Submission Planning

Submit plans commit provenance before it simulates a merge. This matters when
main and integration contain patch-equivalent commits under different SHAs,
as can happen after a rebase or cherry-pick. The broker does not merge the
session HEAD as one undifferentiated history. It classifies the commits, then
replays only pending session-owned single-parent patches onto the exact current
integration tip, in their original order.

The JSON result from `aethyme broker submit --session 111 --json` includes a
`submission_plan` with full SHAs. Ownership and integration state are separate:

| Dimension | Values | Meaning |
| --- | --- | --- |
| `ownership` | `session_owned`, `inherited_from_recorded_baseline`, `ambiguous` | whether the recorded task boundary proves the session owns the commit |
| `integration_state` | `pending`, `already_integrated_by_ancestry`, `already_integrated_by_stable_patch_identity`, `ambiguous` | whether integration still needs that patch |

An inherited commit that already exists under a stable patch identity is
reported but not replayed. A pending owned commit is replayed using its parent
tree as the three-way base. Promotion records a normalized integration commit;
the original session SHA does not need to become an ancestor of integration.
Finish, cleanup, and integration status use the verified queue record to track
that delivered content safely.

The broker refuses instead of guessing when the recorded baseline is missing,
ownership or stable patch identity is ambiguous, or a pending owned commit is
a merge commit. A session rebased directly onto the current integration tip is
accepted only when that range is unambiguous.

On a real conflict, `conflict_details` binds each surviving path to its full
originating session commit, ownership, known integration-side commits, and
ordered remediation commands. The legacy `conflicts` path list remains for
compatibility. A live session is named in `blocking_sessions` only when its
active lease overlaps one of those surviving paths; leases on duplicate-patch
noise do not become blockers. The same evidence and recovery sequence are
written to `.aethyme/broker-action-required.md`.

## Inspect Semantic Gate Suggestions

Use the semantic report when you want to see which additional gate surfaces
callers of changed code might exercise:

```bash
aethyme broker gates affected --session 111 --why
aethyme broker gates semantic --session 111
aethyme broker gates semantic --session 111 --json
```

`gates affected` is the enforced answer. `gates semantic` repeats that
path-selected set and adds a separate suggestion list. A warm graph can explain
each suggestion as changed file → caller file → gate, for example
`src/core.rs -> src/service.rs -> service-integration`. Suggestions already
selected by changed paths are omitted.

The lookup is deterministic and bounded to two incoming call edges, 128
callable nodes, and 64 caller paths. A truncated report is still useful as a
bounded hint, but it is not a completeness claim. An empty warm result means
the graph is usable but contains no relevant callable/caller path for the
change.

Cold, stale, and corrupted graphs do not block work. The command returns a
successful report with `graph_missing`, `graph_stale`, or `provider_error` and
an explanation, while the ordinary path-selected gates remain runnable. In
all states, only path triggers from `.aethyme/gates.toml` reach `gates run` or
submit-time merged-tree verification. See the [CLI reference](../reference/cli.md)
for the complete status table and JSON fields.

## Plan Leases, Then Claim

Lease planning answers “would these claims conflict right now?” without
reserving or refreshing anything:

```bash
aethyme broker leases plan src/broker.rs packages/aethyme/docs/ \
  --session 111
aethyme broker leases plan src/broker.rs packages/aethyme/docs/ \
  --session 111 --json
```

Use repository-relative file paths for exact claims and a trailing slash for a
directory claim. Paths containing `.` or `..` components are rejected as
ambiguous. The plan reports exact and directory overlaps, the owning session,
implicit or explicit lease kind, expiry, and whether a claim would currently
conflict. With `--session`, leases already owned by that session are separated
from foreign blockers; without it, every overlap is a potential conflict.

A plan is a point-in-time read. Another session can claim a path after the plan
returns, so the claim remains authoritative:

```bash
aethyme broker leases claim src/broker.rs --session 111
aethyme broker leases claim packages/aethyme/docs/ --session 111
aethyme broker exec --session 111 -- cargo fmt --all
# edit, verify, and commit
aethyme broker leases release src/broker.rs --session 111
```

Planning does not append broker events or command telemetry. Claiming and
releasing do mutate broker state and are recorded normally.

## Finish With A Durable Handoff

Use `finish`, rather than the lower-level `close`, for the normal end of a
session:

```bash
aethyme broker finish --session 111
aethyme broker finish --session 111 --json
```

The report snapshots:

- submitted, promoted, and published delivery state;
- dirty paths and unsubmitted commits;
- active, released, and expired leases;
- the latest gate, full tree hash, event time, and executed/cache-hit source;
- cleanup safety and one recommended next action.

If dirty or unsubmitted work makes closure unsafe, `finish` refuses and does
not create a misleading completion record. A successful finish atomically
closes the session and writes a redacted `session.finished` event, so the
handoff survives lease cleanup and worktree removal.

Retrieve the latest completed handoff later with exactly one selector:

```bash
aethyme broker handoff --session 111
aethyme broker handoff --worktree /path/to/the/former-worktree --json
```

Session lookup returns that session's newest completed handoff. Worktree lookup
returns the newest completed session registered to the exact path, including a
former absolute path after the worktree has been removed. JSON includes stable
`event_id` and `recorded_at` provenance. Retrieval is read-only: it does not
refresh sessions or leases, rerun gates, or append command telemetry.

The persisted record is deliberately operational and redacted. It excludes the
absolute worktree path, task text, command output, logs, warnings, diffs, and
file content. Treat it as a durable answer to “what landed, what remains, and
what is safe next,” not as an audit replay of the work itself.

## A Complete Follow-Up

One safe follow-up sequence is:

```bash
aethyme broker submit --session 110
aethyme broker finish --session 110
aethyme broker handoff --session 110 --json

cd /path/to/the/session-worktree
aethyme broker adopt --reuse --sync-integration --task "Follow-up work"
aethyme broker leases plan src/broker.rs --session 111
aethyme broker leases claim src/broker.rs --session 111
# edit and commit
aethyme broker gates run --session 111 --no-cache
aethyme broker submit --session 111
aethyme broker finish --session 111
```

The new session ID may differ from the old one. Always use the ID printed by
`adopt`; “existing worktree” does not imply “same session identity.”
