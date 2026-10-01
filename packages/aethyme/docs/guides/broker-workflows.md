# Broker Follow-Up Workflows

Last Updated: 2026-09-27

This guide covers broker workflows that matter after the basic
start-edit-submit loop: preparing declared worktree dependencies, reusing a session worktree, choosing fresh or cached
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
| Prepare worktree dependencies | `prepare status --session <id>` | `prepare --session <id>`, optionally `--offline` | no implicit execution; symlink outputs, undeclared offline commands, lease loss, and writes outside session leases fail closed |
| Continue in an existing worktree | `broker advanced integration status` | `start --reuse`, optionally with `--sync-integration` | synchronization requires a clean, fast-forwardable worktree |
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
`broker advanced ship plan`/`broker advanced ship execute` lane when publication is authorized.

## Inspect a Running Coordinated Operation

Every provider command that has acquired the repository write lane records a
small liveness payload in its operation journal. The payload contains the
current phase, the last progress message, the last heartbeat, and bytes read
from provider stdout/stderr. `broker status` exposes the derived state beside
each unresolved operation; `operations show <id>` and `operations list` expose
the same signal for an operator investigating a hold.

The states answer different questions:

- `active`: the broker process is heartbeating and meaningful progress was
  observed within the stall window;
- `progress_stale`: the broker is alive, but neither provider output nor an
  explicit progress event has arrived for the stall window;
- `heartbeat_stale`: the broker itself stopped renewing the operation, so the
  holder may have died and should be inspected before retrying;
- `unknown`: an older running row predates the liveness payload and needs
  manual inspection.

Wrapped commands can report a phase or message without opening the broker
database. If `AETHYME_BROKER_PROGRESS_FILE` is set, append one complete
newline-delimited JSON object at a time:

```json
{"schema_version":1,"phase":"quality","message":"gate 3 of 7 complete","ts":0}
```

Plain text lines are accepted too. The broker reads the file while the
operation runs, and the file is temporary and removed when the command ends.
Stale-progress status is advisory evidence, not an automatic kill or retry:
inspect the operation and its provider before reconciling or re-running it.

## Prepare Each Worktree Explicitly

If a repository commits `.aethyme/prepare.toml`, every new or adopted session
reports whether its local dependencies are current and prints the exact next
action. Creation remains cheap and deterministic: it never installs packages
or runs a version command automatically.

```bash
aethyme broker submit prepare status --session 111 --json
aethyme broker submit prepare --session 111 --wait 10m
# only when every step declares an offline alternative:
aethyme broker submit prepare --session 111 --offline
```

The repository defines argv commands, lockfile/config inputs, local output
paths, cache scope, and whether hooks need the result. The broker provides the
session boundary, path audit, content-addressed state, runtime fingerprint,
and host-wide serialization for shared caches. It never creates cross-worktree
dependency symlinks and never substitutes ecosystem heuristics. This same
contract supports npm, pnpm, Yarn, Cargo, Python, or mixed repositories.

Use `worktree_local` for disposable outputs wholly owned by one checkout. Use
`repository_shared` only when the tool can safely consume the exported
`AETHYME_PREPARE_CACHE_DIR`; the broker heartbeats that lease and records an
interrupted state if completion was not proven. A failed or stale preparation
does not delete existing dependencies. Fix the declared command or input and
run the printed retry command. Hooks that require preparation refuse before
gates run and leave staged changes untouched. See the
[CLI reference](../reference/cli.md) for the schema and state model.

## Recover External Main Movement

Status warns as soon as the configured upstream contains the first commit that
broker-managed integration does not contain. This commonly happens when a
release or deploy workflow writes changelogs directly to main. Do not merge or
reset refs by hand: update the remote-tracking ref through the coordinated Git
lane, then inspect a reconciliation plan:

When the movement follows a successful
`aethyme broker advanced gh ... -- pr merge ...`, the broker performs that target refresh
itself. If every promotion in the complete integration layer is conclusively
landed, it cleans the stale layer and resolves its publication exposures
transactionally. This includes one upstream squash representing several
contiguous promotions. Mixed or uncertain layers are never rewritten
automatically; the merge succeeds, cleanup is reported as deferred, and the
commands below remain the recovery path.

```bash
aethyme broker advanced git --session 111 --reason "inspect upstream for recovery" -- \
  fetch origin main
aethyme broker advanced integration reconcile --upstream origin/main --dry-run --json
```

The plan classifies upstream-only external commits, recorded promotions, exact
and patch-equivalent landings, unrecorded integration commits, pending queue
entries, and ambiguous equivalence. Full SHAs in JSON let an operator trace
each decision. Cold evidence never becomes a guess: ambiguous matches and
replay conflicts return a successful read-only report marked unsafe.

The ordinary status surfaces use the same automatic evidence without replaying
or mutating anything. `reconciliation_ready` means all recorded promotions are
already landed and only stale broker history remains. `blocked` means at least
one promotion is unresolved or ambiguous, the layer is incomplete, or an
unrecorded commit needs an explicit disposition. Inspect the structured JSON
before acting; the states are intentionally not interchangeable.

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
aethyme broker advanced integration reconcile \
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
aethyme broker advanced integration reconcile \
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

Before starting work, `aethyme broker advanced worktree-root --json` shows the exact
clone-specific root without changing state. A normal `broker start` creates a
sibling beneath that external root even when invoked from an existing broker
worktree; it never nests the new checkout below the invoking worktree. Start
output records the selected root and reports any fallback reason.

To keep worktrees on an external drive, set `[worktrees] root` in
`.aethyme/config.toml`. The per-user default stays in use whenever that path is
missing, unplugged, inside the repository or below its free-space floor, and
`start` says why, so machines without the drive keep working. Worktrees on the
drive are locked, so nothing prunes or cleans them up while it is away. See the
CLI reference for the rules.

Use reuse when a dedicated broker worktree should continue with a follow-up
task. Start by checking whether integration advanced while the session was
working:

```bash
aethyme broker advanced integration status
cd /path/to/the/session-worktree
aethyme broker start --reuse --task "Address review feedback" --short-name "Review fixes" --json
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
aethyme broker start --reuse --sync-integration \
  --task "Address review feedback" --short-name "Review fixes"
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
aethyme broker advanced external-events ingest verified-event.json --json
aethyme broker advanced external-events list --json
```

There is no Aethyme listener, polling loop, or payload archive. Ingestion is
idempotent by provider and event ID, and the strict envelope excludes review
bodies, comments, diffs, credentials, and arbitrary metadata. A supported
event becomes an ordinary non-blocking advisory only when the canonical
repository, watched pull request, and exact commit identify one durable owner.

Uncertain events remain visible without being assigned. Reconcile only after
checking the provider and broker provenance:

```bash
aethyme broker advanced external-events show 17 --json
aethyme broker advanced external-events reconcile 17 --outcome assign \
  --session 111 --reason "verified exact commit ownership"
# Or retain the audit fact without an advisory:
aethyme broker advanced external-events reconcile 17 --outcome ignore \
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
aethyme broker advanced review register --session 111 \
  --repo owner/name --pr 42
aethyme broker submit --session 111
aethyme broker advanced review request --session 111
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
aethyme broker advanced review show --session 111 --json
aethyme broker advanced review unlock --session 111
```

`review unlock` polls live evidence. A successful trusted check can advance
directly from `review_requested`, which covers reviewers whose all-clear signal
has no webhook. Wrong-actor, stale-head, unsuccessful, missing, truncated, and
unavailable evidence fails closed without running the unlock mutation.

Every GitHub write uses the coordinated operation journal. A failed or
crash-ambiguous transition leaves lifecycle state unchanged and blocks blind
retry until `broker advanced operations reconcile` resolves the external outcome.
Cloud Build remains an external manual-trigger adapter boundary; the core
state machine stores no GCP credential and performs no background polling.

### Recover review ownership after a session closes

Closing a session does not erase its review evidence. `review show` remains
available for diagnosis, while `review request`, `review unlock`, and new
leases refuse before provider or database mutation. Choose one explicit
recovery:

```bash
# Continue the same exact-head review from a live replacement session.
aethyme broker advanced review reassign --session <closed-id> \
  --to-session <live-id> --reason "reviewed ownership transfer"

# Retire the lifecycle while retaining its audit and generation history.
aethyme broker advanced review abandon --session <closed-id> \
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

`broker advanced ship plan --entry <id> --json` then explains coverage for every queue
entry included in the selected prefix. Coverage binds the registered canonical
repository, target branch, session commit, promoted queue entry, and review
lifecycle. `broker advanced ship execute` revalidates the provider evidence immediately
before its normal fetch, fast-forward, exact-push, and verify sequence. Missing,
stale, mismatched, ambiguous, or unavailable evidence refuses publication; it
never silently falls back to direct publication.

For repositories that explicitly permit an emergency lane, set
`allow_break_glass = true`, review the normal ship plan, and execute with both
`--break-glass` and `--reason`. This is a separately authorized action. The
broker persists only the reason digest and still enforces the full confirmed
SHA, remote freshness, non-force push, and unknown-outcome barriers.

### When a target path moved under a session

A session branched before another session renamed the files it edits will fail
to replay with `CONFLICT (modify/delete): ... deleted in HEAD`, which reads
exactly like the file having been deleted. `broker start --adopt` (and `--reuse`) now reports
the rename instead:

```
Renamed target: plugins/old/tool.py is now plugins/new/tool.py (queue entry 118, session 131)
  port this session's changes onto the new path before submitting
```

Detection is bounded to the paths the session's own commits touch, and reports
only paths that are absent from the integration tip *and* have a rename to
follow. A path that was genuinely deleted stays a deletion: that is a real
conflict to resolve, and pointing at a path that does not exist would be worse
than saying nothing.

### Running push hooks outside the coordination lock

The repository lock orders remote mutations, but `git push` runs `pre-push`
inside its own process, so the lock is held across that hook too. Where the hook
is a legitimately long full suite, every other session's coordinated operation
on that repository waits for it, and the fleet serialises on whoever is pushing
the largest change.

A repository can opt out of that coupling:

```toml
# .aethyme/config.toml
[coordination]
hooks_outside_lock = true
```

With it enabled, a coordinated push first runs `git push --dry-run`, which
executes `pre-push` against exactly the commits the real push will send, before
queueing for the lock. A hook that refuses stops the operation there, without
ever taking the lock. The broker then acquires the lock, re-plans the push, and
refuses if any destination or proposed commit changed while it waited -- the
hook's verification would no longer describe what is being sent. Only then does
it push, with `--no-verify`, because re-running the hook would double the cost
the setting exists to avoid.

This is off by default and is a deliberate trade. `--no-verify` skips every
`pre-push` protection, not only the slow gate, so a repository using that hook
for secret scanning or signing checks is choosing to run those in the dry run
alone. Enable it when the hook's cost is the constraint and its checks are
deterministic over the same commits.

### Measuring whether repository locking is still the bottleneck

Before designing a finer ref lock, inspect the bounded timing window:

```bash
aethyme broker advanced operations stats --repo github.com/org/repo --json
```

The report includes p50 and p99 lock-hold and queue-wait durations by
operation kind, queue depth, the subset that ran with `hooks_outside_lock`,
and waits where both sides named disjoint known scopes. Rows written before
timing instrumentation are counted as unmeasured rather than reconstructed
from wall-clock timestamps. Repository-wide or provider-unknown targets stay
out of the unrelated-wait count because the broker cannot safely infer that
they do not overlap.

To measure the provider round-trip a future `gh pr merge` ref-scoping design
would add, opt in explicitly:

```toml
[coordination]
measure_pr_merge_ref = true
```

This runs a read-only `gh pr view <selector> --json baseRefName` probe, records
its duration, and leaves the current repository-wide lock policy unchanged.

### Reconciling a local default branch

`ship` publishes an exact promoted prefix and refuses to discard local work the
prefix omits. When the local default branch carries commits integration does
not, `broker advanced main reconcile` decides whether moving it is safe:

```bash
aethyme broker advanced main reconcile plan
aethyme broker advanced main reconcile apply --session <id> --confirm <plan-sha256>
```

Representation is decided by content, not ancestry. The broker promotes by
replaying into a squashed commit, so a commit whose work already landed is
usually not an ancestor of anything on integration, and `git cherry` misses the
same cases because patch ids do not survive squashing. A commit counts as
already represented when integration holds its content for every path the
commit touched; a deletion counts when the path is absent there.

The apply refuses unless every local-only commit is represented and no tracked
path is dirty. Untracked files are ignored: they survive the move, the same way
`ship` preserves them. Before moving anything it creates
`aethyme/preserve/<branch>-<sha>` at the pre-move tip, so the commits left
behind remain recoverable even though their content is already on integration.

Work that is genuinely unrepresented is not moved over by default. Replay it
through a broker session and submit it, then reconcile.

Where that is not the right answer -- work superseded elsewhere, or an
experiment that should simply leave the branch -- record a reviewed decision
instead:

```bash
aethyme broker advanced main reconcile plan --write-resolution-template resolutions.json
# edit each entry's resolution and reason
aethyme broker advanced main reconcile plan --resolution-file resolutions.json
aethyme broker advanced main reconcile apply --session <id> --confirm <digest> \
    --resolution-file resolutions.json
```

Three dispositions are available, and only one unblocks the move:

- `replay_through_broker` -- the default; keeps refusing, because the work
  belongs in integration;
- `archive_local` -- accept that it leaves the default branch, remaining
  reachable from the preservation ref;
- `keep_local_and_block_publication` -- keep it and refuse to move at all.

A commit with no entry stays undecided and keeps refusing: the file records a
decision rather than waiving the check. `already_represented` cannot be chosen,
because asserting it would defeat the content check that makes the move safe.
Each entry requires a reason, and the decisions are bound into the plan digest,
so an edited file no longer matches a reviewed plan.

### Synchronizing local main from another worktree

`ship execute --sync-main` is safe to run from any session worktree, which is
the normal case: the primary checkout almost always has the default branch
checked out, and sessions run elsewhere. Synchronization runs
`git merge --ff-only` **in the primary checkout**, so that checkout's index and
working tree advance with the ref. Before doing so it refuses if the primary
checkout is on another branch, has tracked changes a fast-forward would
overwrite, has untracked paths that would collide, or has diverged from the
confirmed publication.

Do not substitute `git update-ref refs/heads/<default>` for this. It moves the
ref without touching the primary checkout's index or working tree, which leaves
every file changed by the published commits looking locally modified when it is
only stale. `git fetch . origin/<default>:<default>` refuses this case on its
own; `update-ref` does not.

## Choose Gate Cache Policy Deliberately

Gate results prove an exact Git tree. Normal gate runs use the cache when the
same gate has already passed or failed for that tree:

```bash
aethyme broker advanced gates run --session 111
aethyme broker advanced gates run --session 111 --json
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
output content in telemetry. They also record the machine they ran on:
`load_avg_1m_start`, `load_avg_1m_end`, `cpu_count`, and
`free_disk_bytes_start`, so a slow run can be told apart from a loaded
machine. Cached results preserve the original execution's
startup/output measurements, report zero new wait, and report `null`
machine-environment fields. Use these fields to tell
resource contention from slow startup and slow test execution. Before acting
on any result, compare its tree with the tree you intend to submit.

Use a cache bypass when fresh execution itself is required—for example, after
repairing an external dependency or validating a flaky-environment
hypothesis:

```bash
aethyme broker advanced gates run --session 111 --no-cache
aethyme broker advanced gates run --all --no-cache --json
aethyme broker submit --session 111 --no-cache
```

`--no-cache` bypasses lookup for that run; it does not disable storage or erase
older evidence. The newly executed result is stored normally and can satisfy a
later default-policy run for the same tree. Submit threads the same policy into
its merged-tree gates, so use the flag there when the landing decision requires
fresh evidence.

### Gate database isolation

Gate children receive a disposable broker database bound to the canonical
primary repository under test. Commands from its linked worktrees use the same
temporary database, so a binary with unreviewed migrations cannot accidentally
upgrade the operator's live database. Independent fixture repositories retain
their own databases instead of sharing sessions, leases, and counters.

Nested gates retain the ancestor repository bindings. An explicit
`AETHYME_BROKER_DB` pointing to a different file still takes precedence. The
runner also exports internal scope metadata alongside the legacy override;
older binaries and invalid or unresolvable scope metadata retain the temporary
override rather than falling back to live state. Invalid inherited metadata
refuses a nested gate launch.

This prevents accidental database discovery for repositories under validation;
it is not a filesystem sandbox. Commands deliberately targeting other real
repositories or overriding the environment still require their usual authority.

## Review Gate Quality Separately From Readiness

Syntax validity is necessary but does not prove that a gate is safe or useful
for parallel agents. Inspect the committed policy without executing it:

```bash
aethyme broker advanced gates doctor
aethyme broker advanced gates doctor --json
```

Every finding is advisory and includes confidence, bounded evidence, and a
remediation. The doctor checks explicit positive timeouts, cheap and full
lanes, trigger coverage over exact tracked paths, broad expensive gates,
uncovered source areas, service isolation, fixed shared identifiers, writable
caches, main-checkout coupling, failure-status preservation, and equivalent
definitions. It does not expand path-selected gates, change submit behavior,
or run as part of `broker status readiness`.

Add a reviewed native deadline to each gate:

```toml
[[gate]]
name = "integration"
command = "./scripts/integration-test"
cost = 3
timeout_seconds = 1800
triggers = ["src/**", "tests/**"]
```

Omitting `timeout_seconds` keeps the legacy unbounded behavior and produces a
doctor warning. Zero, negative, or non-integer values are invalid. Changing a
deadline changes both the execution-definition hash and the portable scope
manifest, so cached evidence from the old policy cannot authorize the new one.
On expiry, the broker terminates the complete process group and records a typed
timeout rather than a test failure.

A deadline sized for an idle machine is too short on a host whose CPUs are
already saturated by other repositories' builds. Expensive gates are therefore
admitted by host load before they start: while the one-minute load average
divided by the logical CPU count is above the gate's threshold, the broker
waits, reporting `gate <name> waiting for host load: ...` about every 30
seconds. The threshold is `max_load_per_cpu` when the gate sets it, otherwise
`3.0` for any gate with `cost >= 3`; cheaper gates without the key are never
delayed. The wait is bounded by the gate's `resource_wait_seconds` (so a gate
with the default `0` is never delayed), and when the bound is reached, or the
load cannot be read, the gate runs anyway: load never refuses a gate. The wait
happens before owner locks and host resource leases are taken and before the
`timeout_seconds` clock starts, and it is counted in the result's
`wait_duration_ms`.

```toml
[[gate]]
name = "cross-process-contract"
command = "./scripts/contract-check"
cost = 1
timeout_seconds = 300
resource_wait_seconds = 900
max_load_per_cpu = 2.5   # opt a cheap-by-cost gate in; any positive number
```

Setting the key changes the gate's execution-definition hash; leaving it unset
keeps the hash a gate had before the key existed.

When static evidence is insufficient, opt into a disposable probe:

```bash
aethyme broker advanced gates doctor --probe
aethyme broker advanced gates doctor --probe --only integration --json
```

The probe checks out exact committed HEAD in a broker-owned detached worktree,
uses declared host-resource leases, and keeps results and logs in an ephemeral
store that cannot populate the normal gate cache. It snapshots Git state before
and after execution and distinguishes tracked, untracked, and ignored writes.
It does not run repository preparation automatically; that assumption is
explicit in JSON. Cleanup targets only the registered probe worktree,
temporary evidence, and ownership-token-protected resource leases—never broad
Docker names or the invoking checkout. A failing or mutating probe still
returns diagnostic evidence for review; it does not rewrite the policy or
change enforced gates.

## Turn Broker History Into Maintainer Recommendations

Readiness and gate doctor also inspect a bounded window of typed broker
history. They report a maintainer recommendation only after enough consistent
evidence exists for one of these patterns: repeated conflicts on one path,
session-tree passes followed by merged-tree failures, infrastructure failures,
slow gates with sufficient duration samples, repeated cache bypasses,
out-of-lease writes, untracked runtime artifacts, resource contention, or
lease expiration. These recommendations are advisory: they never select a
gate, alter cache policy, block submission, or rewrite repository policy.

Use the explicit advisory inventory to persist the current recommendation
snapshot and manage its lifecycle:

```bash
aethyme broker advanced advisories list --json
aethyme broker advanced advisories show <id> --json
aethyme broker advanced advisories ack <id>
aethyme broker advanced advisories suppress <id>
```

Acknowledgement hides the current evidence but allows the same deterministic
identity to reopen when its bounded evidence changes. Suppression is a
deliberate maintainer choice and remains suppressed across later samples.
When a producer's complete bounded window contains sufficient newer clean
evidence and the pattern is absent, the broker resolves the stored
recommendation automatically. Use `--all` to audit acknowledged, suppressed,
and resolved history.

Recommendation producers consume allowlisted structured fields only. Their
records contain repository-relative paths, typed counts, gate labels, and
durations. They exclude task text, command output, diffs, file contents,
absolute paths, environment values, and secrets. Unsafe path or gate labels
are dropped or redacted before identity generation. Maintainer recommendations
appear only in readiness, gate doctor, and explicit advisory commands; they
are never delivered after commit or before gate execution.

Dynamic recommendation state remains in `.aethyme/broker.db`. It is not added
to tracked onboarding or generated agent-context artifacts, so regenerating
repository guidance remains byte-deterministic and does not expose local
broker history.

### Authoritative committed graph fragments

`[graph].authority = "committed_fragments"` makes graph freshness a repository
deployment-integrity requirement rather than a semantic suggestion. The
repository must also declare its stable graph namespace in `[graph].repository`.
The broker checks the exact candidate tree before configured gates across
session runs, full-tree CI, pre-push, and submission. It reports full tree and
policy hashes in structured outcomes and refuses before promotion when
fragments are stale, missing, corrupted, or pinned to another Aethyme version.

Verification never repairs the patch. It regenerates only inside a disposable
checkout and leaves staged, unstaged, and untracked work recoverable. Review
the version-safe `aethyme graph refresh plan --repo .`, then execute its
digest-confirmed refresh workflow; do not copy fragments from another checkout
or rebuild with a different binary version. A missing local
`.aethyme/graph_store.redb` does not make committed fragments stale.

Managed pre-commit hooks follow the same diagnostic principle. Successful
cheap gates stay quiet. A failure replays complete standard output and error,
prints the broker diagnosis, and preserves the failing exit code.

Submit reports gate evidence separately from queue eligibility. Its JSON
`gate_verification.status` is one of `no_configuration`,
`no_gates_triggered`, `passed`, `failed`, or `deferred` (`not_run` is reserved
for a conflict or content-empty submission). `deferred` means the host, not the
change, stopped every gate that did not pass: host resources were refused, the
command never started (for example the disk-headroom refusal), the broker hit a
host resource error, the command ran out of disk, or the broker's deadline
killed it for the first time on that tree. Each such gate is marked
`"host_fault": true`. Text in a gate's own log never defers a submission -- a
failing test that prints "timed out" is still a rejection -- and a second
timeout on the same tree is a verdict, so a change that hangs is rejected. A
deferred entry stays `submitted`, is never promoted, exits 6 (environment),
and shows as `session.latest-submit-deferred` in `broker status`; free the
resource and resubmit without changing code. The accompanying counts distinguish
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
aethyme broker advanced gates manifest --head <head-sha> --json
aethyme broker advanced gates scope --base <base-sha> --head <head-sha> --json
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
aethyme broker advanced checkpoint plan --session <id> --json
aethyme broker advanced checkpoint apply --session <id> --confirm <plan-sha256>
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
boundary. `broker advanced repair` is narrower still—it repairs a recorded submit or
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
aethyme broker advanced gates affected --session 111 --why
aethyme broker advanced gates semantic --session 111
aethyme broker advanced gates semantic --session 111 --json
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
aethyme broker advanced leases plan src/broker.rs packages/aethyme/docs/ \
  --session 111
aethyme broker advanced leases plan src/broker.rs packages/aethyme/docs/ \
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
aethyme broker advanced leases claim src/broker.rs --session 111
aethyme broker advanced leases claim packages/aethyme/docs/ --session 111
aethyme broker advanced exec --session 111 -- cargo fmt --all
# edit, verify, and commit
aethyme broker advanced leases release src/broker.rs --session 111
```

Planning does not append broker events or command telemetry. Claiming and
releasing do mutate broker state and are recorded normally.

## Export Lease Routing Without Sharing Authority

External queues can ask which category owns a selected session's current and
historical lease rows without learning task text or machine layout:

```bash
aethyme broker advanced leases export --session 111 --json
aethyme broker advanced leases export --entry 320 --limit 100 --json
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

## Push Session Branches As You Go

Work that exists only in a session worktree is lost with the worktree, and
nobody can review a branch that was never pushed. On one machine, a survey of
broker worktrees found 94 holding work that existed nowhere else, and one
repository's local integration branch held 74 commits that had not been
published in five weeks. A repository that delivers through pull requests
should make the session branch the unit of delivery and push it continuously:

```toml
# .aethyme/config.toml
[promote]
mode = "verify-only"   # verify on submit, keep nothing unpublished locally

[delivery]
default = "pull_request"
push_session_branches = true
```

With `push_session_branches = true`, the generated agent instructions tell
agents to commit early and small, and after each commit:

```bash
aethyme broker push --session 111          # push the session's own agent/<slug> branch
aethyme broker push --session 111 --pr     # once there is a first meaningful commit: open a draft PR
```

The broker reads the policy from `.aethyme/config.toml` as committed on the
default branch, so an agent cannot grant itself the authority by editing its
own worktree. `push` publishes only the session's own `agent/*` branch, to the
default branch's remote: a fast-forward is pushed plainly, and a rewritten
branch only under a lease on the commit the broker last pushed for that
session. Uncommitted files are reported, not pushed.

The policy is the authorization for exactly those two actions. It never
authorizes merging, marking a PR ready, or pushing any other ref; those remain
explicit, separately authorized operations through `broker advanced git` and
`broker advanced gh`. `broker status` and `broker doctor` report sessions with
unpushed commits, and `finish` refuses to close one while its commits are
unpushed unless the agent records why the work should not be kept with
`--abandon --reason "<why>"`.

Without the policy, the generated instructions keep the conservative default:
submitting never authorizes publishing, and delivery goes through the
reviewed `broker advanced ship` workflow.

### Verify-only repositories do not use integration

Under `mode = "verify-only"` each session branch and its pull request are the
delivery path, and the integration branch plays no part:

- **`broker submit` is a pre-flight against the current default branch.** It
  simulates the merge onto the fetched default branch and gates that tree,
  never the integration branch, which nothing advances in this mode once a
  pull request merges on the provider. `submit --json` reports the base as
  `verified_against` (`source`: `upstream`, `reference`, `commit`). Without a
  fetched default branch it falls back to integration and says why in
  `verified_against.fallback_reason`.
- **`broker push` reports when the default branch moved.** Each push fetches
  exactly the default branch, then reports `default_branch` (`behind`,
  `ahead`, `would_conflict`, `conflicting_paths`, `suggested_command`) from a
  `git merge-tree` simulation that touches no worktree. A branch already on
  the remote is caught up by merging (`git fetch origin && git merge
  origin/main`), because rebasing a published branch rewrites what its pull
  request shows; an unpublished one by rebasing. A failed fetch never fails
  the push: the last fetched copy is compared and `default_branch_note` says
  so. This applies in every promote mode.
- **`broker status` shows `session.behind-main`** per live session: `info`
  while the branch still merges cleanly, `warning` when it would conflict. It
  uses only fetched refs and the verdict cached by the last push or status,
  re-measuring at most three sessions per call.
- **Integration rows go quiet.** `integration.behind-upstream` is not shown.
  `integration.unpublished-work` is still shown when integration holds
  commits from before the switch, because that is work at risk, but it no
  longer recommends the mode already in force.

Open pull requests are compared separately (`pr_overlaps`); `default_branch`
covers work that has already merged.

In a verify-only repository a conflicting lease **informs** rather than
blocks. `broker submit` reports another active session's conflicting explicit
lease in `lease_warnings`, with the `broker advanced note send` command to
coordinate, then runs the gates as usual. Submit only verifies against the
current default branch and promotes nothing; each session delivers through
its own pull request, so the conflict is resolved when one of them merges.
Blocking would only stall an agent. `broker advanced exec`'s guard is
unchanged: it stops an agent from editing paths outside its own leases inside
its own worktree, which is a different concern.

When several sessions work on the same pull request, `broker start`, `push`
and `status` say so (`duplicate_work`, `session.duplicate-work`). Decide which
one continues and finish the others; two agents repairing one PR keep
conflicting with each other however leases are configured.

Files that every session regenerates, such as generated manifests, conflict in
nearly every pair of sessions. List them in `[leases] ignore` (exact paths,
directory prefixes ending in `/`, or bare file names), or stop committing
them, and resolve the merge-time conflict by regenerating.

## Where A New Session Starts

Without `--base`, `broker start` cuts the session branch from the first of:

1. The integration branch, when `[promote]` promotes (`auto` or `manual`) and
   integration contains the fetched default branch's tip.
2. The fetched default branch (`origin/HEAD`'s target, else the main
   checkout's upstream). This is always the base under
   `mode = "verify-only"`, and the base whenever integration has fallen
   behind upstream.
3. The local default branch, then `main` or `master`, when nothing has been
   fetched.

A session never starts from an integration branch that is behind upstream:
one repository's integration stopped moving while upstream merged 2,101
commits, and every session cut from it started three days stale. `start`
names what it skipped in `start_base.bypassed_integration` (`reason`,
`behind_default_commits`, `ahead_default_commits`, and a `recovery_command`
when integration is behind), prints a warning, and `broker status` keeps an
`integration.behind-upstream` row until integration is reconciled with
`aethyme broker advanced integration reconcile --upstream <ref>`. `start` never
fetches, so "upstream" is as fresh as the last fetch.

`[promote]` is read with the same trust rule as the push policy: the
`.aethyme/config.toml` committed on the fetched default branch wins, and the
main checkout's working-tree file is used only when the default branch commits
none. A merged `mode = "verify-only"` therefore takes effect without anyone
pulling it into the main checkout.

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
aethyme broker finish cleanup 111
```

For periodic reclamation, first inspect the dry-run plan, then apply it:

```bash
aethyme broker finish cleanup --all-cleaned
aethyme broker finish cleanup --all-cleaned --apply
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
aethyme broker advanced queue history --limit 50 --json
aethyme broker advanced queue history --limit 50 --before <next-before-id> --json
```

Status also grades retained cleanup cost using worktree and branch counts,
estimated bytes, and oldest closed-session age against the declared retention
policy. A warning means at least one threshold has been crossed; review the
dry-run cleanup or GC plan before authorizing reclamation.

To assess whether agent-facing advisory delivery is effective without
retaining repository content, inspect the bounded allowlisted metrics:

```bash
aethyme broker advanced advisories metrics
aethyme broker advanced advisories metrics --json
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
deletions. Check `aethyme broker status doctor` or `aethyme certify` for pending
recovery and retention health. Keep the shipped defaults unless repository
history or storage constraints justify an explicit `.aethyme/broker.toml`.

### Build Caches In Session Worktrees

Build caches are the largest thing a broker worktree holds and the only thing
in it that no one contributed, so they are reclaimed on their own terms:
`[retention].artifact_sweep_budget_ms` lets a broker open reclaim them without
confirmation, since rebuilding recovers everything it removes.

A `target/` of any real size takes minutes to unlink, which no per-open budget
can hold. Removal therefore stops at the deadline mid-directory and leaves the
tree still recognisable as the cache it is — the file that classifies it is
always the last one taken. An unfinished pass withholds the sweep's cadence
stamp, so the next broker command resumes instead of waiting out
`artifact_sweep_interval_hours`, and a backlog too large for one budget still
drains.

The sweep also reaches sessions that are still open once their agent has been
quiet for `[retention].idle_session_artifact_hours` (default 24). The session
stays open with its checkout and branch; only its witnessed build caches go,
and a cache written to within the window is left for whatever is still
writing it.

`gc apply` re-proves each build cache before removing it, and a cache that no
longer qualifies — a session live again, a directory no longer carrying its
witness — is reported as `retained:` while the run continues. Candidates here
share no fate, and a run that stopped at the first of them left a journal that
pinned every later `gc plan` with no command to release it.

The broker also writes cargo build defaults once, in a `.cargo/config.toml`
beside the worktrees rather than inside one:

```toml
[build]
incremental = false

[profile.dev]
debug = "line-tables-only"
```

A session worktree is built a handful of times and then reclaimed, so it pays
for neither rebuild state nothing will rebuild from nor debug info nothing
reads back; backtraces keep their file and line numbers. Gates already decline
both through `gates.toml`. Because the file sits above the worktree, it is
never an untracked file in `git status`, a repository shipping its own
`.cargo/config.toml` outranks it, and `CARGO_PROFILE_*` in a gate command still
wins over both. Delete or empty it to build with cargo's defaults; the broker
writes it only when it is absent.

Retrieve the latest completed handoff later with exactly one selector:

```bash
aethyme broker advanced handoff --session 111
aethyme broker advanced handoff --worktree /path/to/the/former-worktree --json
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
aethyme broker advanced handoff --session 110 --json

cd /path/to/the/session-worktree
aethyme broker start --reuse --sync-integration --task "Follow-up work" --short-name "Follow-up"
aethyme broker advanced leases plan src/broker.rs --session 111
aethyme broker advanced leases claim src/broker.rs --session 111
# edit and commit
aethyme broker advanced gates run --session 111 --no-cache
aethyme broker submit --session 111
aethyme broker finish --session 111
aethyme broker finish cleanup 111
```

The new session ID may differ from the old one. Always use the ID printed by
`start --reuse`; “existing worktree” does not imply “same session identity.”
