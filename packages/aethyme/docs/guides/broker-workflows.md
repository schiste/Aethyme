# Broker Follow-Up Workflows

Last Updated: 2026-09-01

This guide covers broker workflows that matter after the basic
start-edit-submit loop: reusing a session worktree, choosing fresh or cached
gate evidence, understanding normalized submission planning, inspecting
advisory semantic gate suggestions, planning leases before claiming them, and
exporting redacted routing metadata, and leaving or retrieving a durable finish handoff. For the complete flag
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
| Share gate scope with CI | `gates manifest` and `gates scope` | none | unsupported schema, digest drift, missing refs, or invalid committed policy fail closed |
| Inspect graph-derived gate hints | `gates semantic` | none; suggestions remain advisory | only changed-path triggers reach `gates run` and `submit` |
| Recover external main movement | `integration reconcile --dry-run` | `--apply --confirm <plan-digest>` | every unrecorded SHA needs a reviewed disposition; drift invalidates confirmation |
| Reserve paths | `leases plan` | `leases claim` | the claim, not the plan, decides whether a conflict exists now |
| Route external path queues | `leases export --session/--entry` | none | committed routing config and bounded redacted output are authoritative |
| Recover a closed review owner | `review show` | `review reassign` or `review abandon` with a reason | only an exact-head live session can inherit; abandonment is explicit and audited |
| Recover a rewritten checkpoint | `checkpoint plan --json` | `checkpoint apply --confirm <digest>` when the plan is safe | unsafe plans preserve the exact tip before directing a clean replay; they never recommend a blanket integration rebase |
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

Do not transcribe that schema from diagnostics. Ask the first dry-run to write
the complete, no-clobber review document:

```bash
aethyme broker integration reconcile \
  --upstream origin/main \
  --write-resolution-template reconciliation.json \
  --dry-run
```

The generated `null` operator decisions and reasons cannot validate or apply.
Fill them after reviewing the adjacent structured evidence, then pass the same
file with `--resolution-file`. If a later dry-run discovers another blocked
entry, its replacement template preserves already valid reviewed entries and
adds the missing placeholders.

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

Before starting work, `aethyme broker worktree-root --json` shows the exact
clone-specific root without changing state. A normal `broker start` creates a
sibling beneath that external root even when invoked from an existing broker
worktree; it never nests the new checkout below the invoking worktree. Start
output records the selected root and reports any legacy fallback reason.

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

## Deliver Authenticated Provider Events

Keep provider authentication outside the broker. After a webhook receiver or
explicit authenticated poll verifies one GitHub fact, normalize the allowlisted
provenance fields, compute the envelope digest, and deliver the local file:

```bash
aethyme broker external-events ingest verified-event.json --json
aethyme broker external-events list --json
```

There is no Aethyme listener, polling loop, or payload archive. Ingestion is
idempotent by provider and event ID, and the strict envelope excludes review
bodies, comments, diffs, credentials, and arbitrary metadata. A supported
event becomes an ordinary non-blocking advisory only when the canonical
repository, watched pull request, and exact commit identify one durable owner.

Uncertain events remain visible without being assigned. Reconcile only after
checking the provider and broker provenance:

```bash
aethyme broker external-events show 17 --json
aethyme broker external-events reconcile 17 --outcome assign \
  --session 111 --reason "verified exact commit ownership"
# Or retain the audit fact without an advisory:
aethyme broker external-events reconcile 17 --outcome ignore \
  --reason "provider event does not apply to this repository"
```

The broker stores a digest, not the reconciliation reason. Reconciliation
never grants publication authority and advisories never expand gate selection
or block submit.

## Coordinate Review Before Expensive Validation

Review coordination is opt-in through `[review]` in `.aethyme/config.toml`.
With no section, submission and gate behavior is unchanged. In an opted-in
repository, create the draft PR through the repository's normal authenticated
workflow, then bind it to the live session:

```bash
aethyme broker review register --session 111 \
  --repo owner/name --pr 42
aethyme broker submit --session 111
aethyme broker review request --session 111
```

The first command proves that the open draft's full head SHA equals the session
HEAD. Submission supplies the queue provenance. The ready-for-review write is
therefore unavailable until local submission has passed. Provider review
events arrive through `external-events ingest`; matching changes requested
becomes a typed advisory and matching approval satisfies review. An older-commit
approval cannot advance a replacement generation.

Formal GitHub approval is the default evidence adapter. For a reviewer that
comments but never submits `APPROVED`, configure `github_check_run` instead:

```toml
[review]
enabled = true
evidence_adapter = "github_check_run"
required_approvals = 0
evidence_check_name = "review-gate/codex"
evidence_app_slug = "github-actions"
unlock_adapter = "github_label"
unlock_label = "aethyme-validation-ready"
```

The named check must be a trusted repository control that translates the
reviewer's native evidence. A robust adapter verifies the reviewer identity,
binds its structured status to the full current head SHA, and refuses while
that reviewer owns any unresolved thread on the head. Aethyme then verifies
the resulting check's exact name, app, head, status, and conclusion. It never
parses review comment bodies or treats a reaction as authorization. Prefer a
dedicated GitHub App; when using `github-actions`, ensure pull requests cannot
replace or spoof the workflow that creates the check.

Inspect state locally and unlock only after review evidence is current:

```bash
aethyme broker review show --session 111 --json
aethyme broker review unlock --session 111
```

`review unlock` polls live evidence. A successful trusted check can advance
directly from `review_requested`, which covers reviewers whose all-clear signal
has no webhook. Wrong-actor, stale-head, unsuccessful, missing, truncated, and
unavailable evidence fails closed without running the unlock mutation.

Every GitHub write uses the coordinated operation journal. A failed or
crash-ambiguous transition leaves lifecycle state unchanged and blocks blind
retry until `broker operations reconcile` resolves the external outcome.
Cloud Build remains an external manual-trigger adapter boundary; the core
state machine stores no GCP credential and performs no background polling.

### Recover review ownership after a session closes

Closing a session does not erase its review evidence. `review show` remains
available for diagnosis, while `review request`, `review unlock`, and new
leases refuse before provider or database mutation. Choose one explicit
recovery:

```bash
# Continue the same exact-head review from a live replacement session.
aethyme broker review reassign --session <closed-id> \
  --to-session <live-id> --reason "reviewed ownership transfer"

# Retire the lifecycle while retaining its audit and generation history.
aethyme broker review abandon --session <closed-id> \
  --reason "pull request superseded"
```

Reassignment requires the destination session to be live and at the exact
commit already bound to the lifecycle. It retains state, evidence, and
generation history. Abandonment makes the lifecycle inactive so a later
session can register a fresh lifecycle for the same pull request. Reason text
is not persisted; the broker records only its digest in a redacted event.

## Require Review Evidence at Publication

Review coordination and publication authorization are separate controls. The
default ship lane remains direct and requires the exact planned SHA. To make
unlocked review evidence mandatory, commit the policy in the promoted series:

```toml
[publication]
schema_version = 1
mode = "review_gated"
allow_break_glass = false
```

`broker ship plan --entry <id> --json` then explains coverage for every queue
entry included in the selected prefix. Coverage binds the registered canonical
repository, target branch, session commit, promoted queue entry, and review
lifecycle. `broker ship execute` revalidates the provider evidence immediately
before its normal fetch, fast-forward, exact-push, and verify sequence. Missing,
stale, mismatched, ambiguous, or unavailable evidence refuses publication; it
never silently falls back to direct publication.

For repositories that explicitly permit an emergency lane, set
`allow_break_glass = true`, review the normal ship plan, and execute with both
`--break-glass` and `--reason`. This is a separately authorized action. The
broker persists only the reason digest and still enforces the full confirmed
SHA, remote freshness, non-force push, and unknown-outcome barriers.

## Choose Gate Cache Policy Deliberately

Gate results prove an exact Git tree. Normal gate runs use the cache when the
same gate has already passed or failed for that tree:

```bash
aethyme broker gates run --session 111
aethyme broker gates run --session 111 --json
```

Run this session-scoped command after the final commit instead of invoking the
same test suite directly. If integration does not move and normalized
submission produces the identical tree, `broker submit` reuses that proof and
does not execute the expensive gate a second time. If either tree or the full
gate definition differs, submit runs it normally; cache reuse never weakens the
landing check.

Text output shows an abbreviated 12-character tree hash. JSON returns the full
hash in `tree_hash` and identifies whether the result was executed or cached.
Executed results also separate `wait_duration_ms` from command `duration_ms`,
record `first_output_ms`, and count combined `output_bytes` without storing
output content in telemetry. Cached results preserve the original execution's
startup/output measurements and report zero new wait. Use these fields to tell
resource contention from slow startup and slow test execution. Before acting
on any result, compare its tree with the tree you intend to submit.

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

Submit reports gate evidence separately from queue eligibility. Its JSON
`gate_verification.status` is one of `no_configuration`,
`no_gates_triggered`, `passed`, or `failed` (`not_run` is reserved for a
conflict or content-empty submission). The accompanying counts distinguish
configured, selected, freshly executed, and cached gates. In text mode a
manual-mode entry with no gate proof is called `conflict-checked`, never simply
`verified`.

`gates validate` intentionally reads the checkout where it is invoked; submit
loads `.aethyme/gates.toml` from the simulated submitted tree. Therefore a
local untracked gates file cannot protect a spawned worktree or submission.
`aethyme certify` warns about that state: review and commit the gate definition
before relying on it.

## Share Exact Gate Scope With External Validators

CI and path-scoped merge queues should consume the broker contract instead of
reimplementing glob parsing or diff classification:

```bash
aethyme broker gates manifest --head <head-sha> --json
aethyme broker gates scope --base <base-sha> --head <head-sha> --json
```

The manifest is normalized and versioned. Its digest changes when execution
policy changes, but its JSON omits the executable command and all runtime
values. The scope report binds full resolved SHAs, the manifest digest, the
complete sorted changed-path surface, and deterministic path-selection reasons.
Both commands are read-only and load policy from the exact committed head, so
untracked or dirty local configuration cannot alter their answer.

Use the scope result for enforced validation routing. Semantic graph advice is
not part of this exact evaluator and is explicitly marked unenforced; inspect
it separately with `gates semantic`. This preserves the invariant that a warm,
cold, stale, corrupted, or truncated graph never silently changes what local
submit or external CI must run.

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

For an owned merge commit, the refusal is also the recovery plan. It names the
accepted checkpoint, the pending merge commit, and the current session HEAD.
Run its commands in order: first preserve that HEAD on the uniquely named
`aethyme/recovery/...` branch, then use `git reset --soft <accepted-checkpoint>`
to stage the reviewed net tree change, commit it as a linear patch, and submit
again. Never reset or rewrite the session before creating the preservation
branch; the preserved ref is the rollback path if flattening included work you
did not intend to own.

If a clean session was deliberately rewritten onto the promoted integration
history, its accepted session SHA can cease to be an ancestor even though the
accepted content remains proven by the recorded integration commit. Do not
replace that ownership boundary by hand. Review a typed recovery instead:

```bash
aethyme broker checkpoint plan --session <id> --json
aethyme broker checkpoint apply --session <id> --confirm <plan-sha256>
```

The plan is read-only and binds the old checkpoint, proposed integration
checkpoint, current session HEAD, relation, pending commits, normalized
`SubmissionPlan`, safety conditions, and preservation ref. Apply rebuilds the
plan, requires the exact digest, refuses dirty or divergent work, creates the
recovery ref first, and then atomically journals the checkpoint update. It
never rebases or resets the worktree. After a successful apply, submit normally;
only commits after the reviewed integration checkpoint are session-owned.

JSON includes stable `refusal_codes` and ordered `next_actions`. When recovery
is unsafe, the first action preserves the full session tip on the named
recovery branch, followed by graph inspection and a clean replay-session
workflow. Do not blanket-rebase a session onto `aethyme/integration`: that ref
can contain unrelated promoted work and is a replay target, not an ownership
boundary. `broker repair` is narrower still—it repairs a recorded submit or
promoted-path conflict. With no such conflict it refuses immediately and
points to `checkpoint plan` instead of repeating a non-progressing repair.

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

## Export Lease Routing Without Sharing Authority

External queues can ask which category owns a selected session's current and
historical lease rows without learning task text or machine layout:

```bash
aethyme broker leases export --session 111 --json
aethyme broker leases export --entry 320 --limit 100 --json
```

Define categories explicitly in committed repository policy:

```toml
[leases.routing]
broker = ["packages/aethyme/rust/crates/aethyme-broker/"]
docs = ["packages/aethyme/docs/"]
```

The export is a point-in-time, read-only projection. It identifies the
canonical remote without exposing its URL, distinguishes lease lifetime and
overlap states, reports truncation, and ignores dirty configuration. Repeating
it cannot refresh an expiry or acknowledge ownership. Keep provider-specific
labels and queue payloads in adapters; use the exported schema as their
idempotent input rather than extending the broker's lease storage.

## Finish With A Durable Handoff

Use `finish`, rather than the lower-level `close`, for the normal end of a
session:

```bash
aethyme broker finish --session 111
aethyme broker finish --session 111 --json
aethyme broker finish --session 111 --keep-worktree
```

The report snapshots:

- submitted, promoted, and published delivery state;
- dirty paths and unsubmitted commits;
- active, released, and expired leases;
- the latest gate, full tree hash, event time, and executed/cache-hit source;
- cleanup safety, exact physical cleanup outcome, and one recommended next
  action.

If dirty or unsubmitted work makes closure unsafe, `finish` refuses and does
not create a misleading completion record. Otherwise it closes broker state,
writes a redacted handoff, and by default reclaims a represented broker-owned
spawned worktree plus its exact checked branch. The command can safely remove
the checkout it was invoked from after startup. Use `--keep-worktree` when the
checkout must remain available for review or reuse; use `close` when only the
broker state should close.

An interrupted or refused physical cleanup leaves the session closed and
prints its recovery command. Resume it with:

```bash
aethyme broker cleanup 111
```

For periodic reclamation, first inspect the dry-run plan, then apply it:

```bash
aethyme broker cleanup --all-cleaned
aethyme broker cleanup --all-cleaned --apply
```

The plan lists each retained spawned worktree, its eligibility, and estimated
bytes. Apply revalidates every candidate and never force-removes adopted
worktrees, dirty paths, symlinked paths, or commits not represented by main,
integration, or the configured upstream. `broker status` warns when eligible
cleaned worktrees remain. Use `--json` for the stable plan or sweep report.

Treat `broker status` as the bounded present-state dashboard, not as an audit
log. Resolve its warnings, inspect outstanding advisories and exposures with
the exact printed commands, then consider live sessions and current queue
entries. Fetch terminal queue history separately and page it when needed:

```bash
aethyme broker queue history --limit 50 --json
aethyme broker queue history --limit 50 --before <next-before-id> --json
```

Status also grades retained cleanup cost using worktree and branch counts,
estimated bytes, and oldest closed-session age against the declared retention
policy. A warning means at least one threshold has been crossed; review the
dry-run cleanup or GC plan before authorizing reclamation.

To assess whether agent-facing advisory delivery is effective without
retaining repository content, inspect the bounded allowlisted metrics:

```bash
aethyme broker advisories metrics
aethyme broker advisories metrics --json
```

These rows correlate display surfaces with acknowledgement or verified
publication resolution. They deliberately exclude task text, arguments,
repository paths, evidence, diffs, and secrets.

The cleanup sweep above is limited to represented session worktrees. For the
complete bounded retention lifecycle—including terminal database history,
gate logs, command metrics, and the same represented worktrees—review and
confirm a unified GC plan:

```bash
aethyme broker gc plan --json
aethyme broker gc apply --confirm <sha256>
```

The digest binds the exact rows, files, refs, hashes, byte estimates, policy,
and blockers. Apply is crash-resumable and refuses changed artifacts. A later
broker command may advance only an already-confirmed recovery journal for at
most `[retention].startup_budget_ms`; no startup path silently approves new
deletions. Check `aethyme broker doctor` or `aethyme certify` for pending
recovery and retention health. Keep the shipped defaults unless repository
history or storage constraints justify an explicit `.aethyme/broker.toml`.

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
aethyme broker cleanup 111
```

The new session ID may differ from the old one. Always use the ID printed by
`adopt`; “existing worktree” does not imply “same session identity.”
