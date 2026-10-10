# Local v3 — L4 candidate boundary (#663)

Last Updated: 2026-10-10

This record covers the first slice of L4: separating the **construction** of a candidate
from verifying, gating and accepting it, and wrapping the existing Git replay as the
compatibility producer (plan §6.1, §6.6, §7.3 step 9). It is provisional: E1 (#650) has not
run, so D04–D06, D12 and D18 are still open and nothing here selects a composition
algorithm, grammar or base rule.

Code: `aethyme-broker::composition`, plus:

- `collaboration_archive::snapshot_of_commit`;
- `gate_trust::assess_trusted`;
- `Broker::audit_submit_ownership_read_only`, with `leases_as_refreshed` and
  `classify_overlaps_now`.

There is no schema, queue, gate or submit change.

## Decided here

### One outcome for every producer

The legacy replay and the composer (#664) both return a `CompositionOutcome`:

| Outcome | `code()` | Carries source? |
|---|---|---|
| `Candidate` | `candidate` | Yes: subject, tree, commit |
| `NoChange` | `no_change` | No (baseline only) |
| `Conflict` | `conflict` | No (baseline, then path and input per conflict) |
| `Unsupported` | the reason's code | No |
| `Refused` | the reason's code | No |

Only a `Candidate` names a snapshot, a tree or a commit. A conflict never exposes the
conflict-marker tree that `git merge-tree` writes, so no caller can mistake one for
something it could gate. This is the "no success-shaped output" rule of §7.2.

A `Candidate` holds:

- `subject`: the #652 `SourceSnapshotId` of its committed bytes, the immutable identity
  later steps use. `snapshot_of_commit` computes it without retaining anything, using the
  archive's own refusals: gitlinks, checkout-transforming attributes, and paths #652
  rejects. So a subject is named exactly when `retain_snapshot_with` would retain the same
  commit, under the same ID. That holds unless the source disappears in between.
  - Tested against the independent oracle.
  - Tested for refusal-code parity: four transforming attributes and an NTFS `.git` alias.
  - Cost: it reads and hashes every blob of the tree, O(tree bytes) per call, with no cache.
    That is acceptable for one candidate per submit. A caller that builds many candidates
    should retain the snapshot and reuse the ID.
- `tree`, and `commit`: an unreferenced commit of `tree` whose only parent is `baseline`.
  Building a candidate accepts nothing. Acceptance stays a separate step, and the composer
  has no canonical write capability (§7.3 step 9).
- `baseline` and `baseline_source` (`integration` or `upstream`, as in a submit outcome's
  `verified_against.source`).
- `inputs`, in application order: each contribution's `base` and `result` commits, plus
  their snapshot IDs when the producer read them from the archive (the legacy replay
  knows only commits).
- `producer` (`legacy-git-replay/v0`, or a composer and its profile) and the **observed**
  `mode`: `text`, `structural` or `hybrid`. The legacy replay is `text`. A producer that
  cannot observe whether a structural engine fell back to text must say `hybrid` (§7.2).

Failing to read the source for any other reason (a Git failure, I/O) is an `Err`: the typed
`BrokerOpError::CandidateSource(ArchiveError)`, keeping the archive's code.

### Reason codes

| Kind | Code | Used by |
|---|---|---|
| Refused | `checkout_drift` | Legacy: the checkout left its recorded branch. Submit refuses with `SessionCheckoutDrift` |
| Refused | `ownership_violation` | Legacy: unleased paths, another active session's conflicting explicit lease, or adoption-time foreign files. Submit refuses with `OwnershipViolation` |
| Refused | `untrusted_policy` | Legacy: the gate policy at the baseline is untrusted, and submit refuses too |
| Refused | `unsafe_plan` | Legacy: commit provenance is ambiguous, and submit refuses the same plan |
| Refused | `unknown_base`, `dependency_cycle`, `missing_input`, `competing_revisions`, `budget_exhausted`, `inseparable_selection` | Reserved for the composer (§7.2, §7.3) |
| Unsupported | `merge_commit` | Legacy: a pending commit with two or more parents. Submit refuses with `UnsupportedSubmissionCommit` |
| Unsupported | `commit_shape` | Legacy: a pending root commit (no parent). Submit refuses with `UnsupportedSubmissionCommit` |
| Unsupported | `snapshot_entry` | The candidate holds something a #652 snapshot cannot name: a gitlink, or a path #652 rejects |
| Unsupported | `transforming_attribute` | A candidate path has `filter`, `working-tree-encoding` or `ident`, which the archive refuses to retain |
| Unsupported | `partial_clone` | The repository is a partial clone, which the archive reads no source from |

The codes are stable strings, tested. Adding one is fine. Renaming one is a contract change
once a consumer reads them.

### The legacy adapter

`Broker::legacy_candidate(session)` builds the candidate `submit` would gate on now. It runs
submit's preflight in submit's order:

1. **Checkout identity**: the same `require_session_checkout_identity` submit runs. A
   checkout on another branch is `checkout_drift`, not a candidate built from the wrong
   branch.
2. **Lease ownership**: `audit_submit_ownership_read_only` makes the same judgement as
   `audit_submit_ownership`, without its writes.
   - Submit refreshes leases first, writing implicit leases, the overlap cache, announcements
     and `leases.refreshed_at_ms`. The read-only audit computes the implicit leases that
     refresh would set and holds them in memory.
   - It classifies this session's overlapping pairs from scratch, without the shared cache.
   - It reads session liveness from a snapshot.
   - The path checks and both verdicts (`judge_by_policy` for guarded exec,
     `judge_on_conflicts` for submit) are one shared code path, so the two audits cannot
     drift apart.
3. **Baseline**, chosen as submit chooses it: the fetched default branch under
   `verify-only`, otherwise the integration branch. A `verify-only` repository with no
   fetched default branch falls back to integration, as submit does. Submit creates or
   fast-forwards the integration branch before simulating (the follows-main refresh). The
   adapter **computes** that result and does not perform it: a missing or behind
   integration branch means the main checkout's HEAD.
4. **Policy**: the gate policy committed at the baseline must be trusted.
   `assess_trusted` reaches the same decision as `require_trusted` but records nothing. A
   repository that submit would grandfather counts as trusted without being grandfathered.
5. **Plan and replay**: the same `build_submission_plan` and `replay_submission_plan` that
   submit runs, unchanged.
6. **Classify**: replay conflicts become `Conflict`. A tree equal to the baseline's becomes
   `NoChange`, except for a main-checkout session, which submit also gates. Anything else
   becomes `Candidate`, or `Unsupported` if the subject cannot be named.

It writes no queue row, gate result, slot, lease, overlap verdict, ref, reflog, Git
configuration, trust record, event or action file. Like `submission_plan`, it can leave
unreachable Git objects, which `git gc` removes.

### Parity is tested against submit itself

`tests/candidate_boundary.rs` builds the candidate and proves the build changed nothing. It
then runs the real `submit` on the same state:

| Case | Adapter | Submit |
|---|---|---|
| Clean merge after another promotion | `candidate`: same tree, baseline and source; subject equals the promoted commit's snapshot | promoted |
| Same line edited twice | `conflict` naming `shared.txt` and the session commit | `Conflict`, same path and commit |
| Empty commit | `no_change` | `no_changes`, not promoted |
| Merge commit in the session | `merge_commit` | `UnsupportedSubmissionCommit` |
| Root commit merged in from unrelated history | `commit_shape` | `UnsupportedSubmissionCommit`, 0 parents |
| History rewritten off the recorded baseline | `unsafe_plan` | `UnsafeSubmissionPlan` |
| Checkout switched to another branch | `checkout_drift` | `SessionCheckoutDrift` |
| Path claimed by another active session that edits it incompatibly | `ownership_violation` | `OwnershipViolation` |
| Path claimed by another session that merges cleanly | `candidate`, same tree | promoted |
| `verify-only`, provider moved the default branch | `candidate` on `origin/main`, source `upstream` | verified against the same commit and tree, not promoted |
| `verify-only`, no fetched default branch | `candidate` on integration | verified against integration with a fallback reason, same tree |
| Main checkout moved past integration | baseline is main HEAD; integration not moved | verified against main HEAD, same tree |
| Gate policy added on the baseline, untrusted | `untrusted_policy`, no trust record or event written | `GatePolicyUntrusted` |
| Gate history but no trust record (grandfathering) | `candidate`, nothing recorded | grandfathers, then promotes the same tree |
| Gitlink in the session (claimed) | `snapshot_entry` | promoted (see the divergences) |
| `filter` attribute in the session | `transforming_attribute` | (not compared; see the divergences) |
| Partial clone | `partial_clone` | promoted |

"Changed nothing" is checked across all of the following:

- every ref, and the main checkout's HEAD;
- the worktree list;
- the porcelain status of the main checkout and each session, ignored files included;
- the bytes of every file under `.git`, except `objects` and the index, which `git status`
  rewrites. That covers reflogs, configuration and worktree records;
- the bytes of every file under each `.aethyme` tree, so action files are included;
- every row of every table of `broker.db`, read through a read-only connection.

A neuter that writes `.aethyme/broker-action-required.md` from the adapter fails these
checks.

## Known divergences from submit

- **Subjects.** A gitlink, a path #652 rejects, a transforming attribute or a partial clone
  gives `Unsupported` where submit would gate the merged tree. The boundary needs a subject
  the archive could retain. The legacy path keeps working because submit does not route
  through the boundary.
- **The #135 recovery.** A session's earlier promotion can lose its response, leaving the
  integration tip as that session's unclaimed promotion. Submit then claims it and reports
  `promoted` with `no_changes`. The adapter only reports `no_change`: the claim is a write,
  and there is no new candidate to build.
- **Liveness and abandonment.** Submit's refusal policy uses `agents`, which can persist an
  exited or abandoned session's transition before judging. The adapter uses
  `agents_snapshot`, so a holder that submit would first close still counts as live.
  Only an active holder can refuse, and abandonment needs inactivity, so the two should
  agree in practice. Where they differ, the adapter is the stricter of the two.
- **Verify-only integration refresh.** Before auditing, submit advances a verify-only
  repository's disposable integration branch onto the fetched default branch (#352). That
  moves the base the session's changes are measured from. The adapter measures from the
  integration branch as it stands. It is exact whenever integration is current.
- **Overlap classification budget.** Both classify within `CLASSIFY_BUDGET`. Submit starts
  from the cached verdicts, the adapter from none. A pass cut short by the budget treats an
  unclassified pair as low severity in both.
- **Metadata of the candidate commit.** It has the same tree and parent as submit's
  verification commit, but a different message (`chore(broker): candidate for session N`)
  and broker-only attribution. Submit's verification commit carries the promotion subject,
  the agent's `Co-Authored-By` trailer, and the contract decision and justification copied
  from the pending commits. The contract gate reads that commit, so when #692 routes submit
  through this boundary, the message must come from submit's rules, not from this adapter.
  Until then, the subject (the snapshot ID) is the identity to compare, not the commit ID.

## Not decided here (provisional pending E1 #650)

- **D04–D06, D12:** the composer's algorithm, grammar, mode observation, base
  normalization, ordering, atomic groups and exclusion. #664 consumes E1's profile. The
  reserved refusal codes are the plan's vocabulary, not a chosen behavior.
- **D18:** what stays compatible when optional capture fails. The legacy path is unchanged
  here, so this slice adds no new failure mode.
- **Wiring into the gate boundary.** Submit still mints its verification commit inside
  `simulate_and_gate_against`. PR #725 (issue #692) gives the verification candidate a typed
  record and splits minting out of gating, and also bumps the broker schema. Routing submit,
  and later the composer's candidate, through `CompositionOutcome` waits for #725 to land so
  the two changes do not fight over `merge.rs`. This slice changes `merge.rs` only by
  exposing `replay_submission_plan`, `SubmissionReplay` and
  `require_session_checkout_identity` to the crate.
- Retaining a candidate's subject in the archive, and candidate manifests and resolution
  requests (#665).

## Tests

- `cargo test -p aethyme-broker --test candidate_boundary`: every case above except the
  two trust cases, plus the stability of every reason code.
- `cargo test -p aethyme-broker --test candidate_boundary_trust`: the untrusted policy.
- `cargo test -p aethyme-broker --test candidate_boundary_grandfather`: grandfathering.
  This and the untrusted-policy test are binaries of their own because each changes the
  process environment (the suite's trust escape and the host state directory).
- `cargo test -p aethyme-broker --lib collaboration_archive::tests::a_commit_snapshot`:
  `snapshot_of_commit` agrees with the oracle and with `retain_snapshot`, and makes the same
  refusals with the same codes.
