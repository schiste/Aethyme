# Local v3 — L4 candidate boundary (#663)

Last Updated: 2026-10-10

This record covers the first slice of L4: separating the **construction** of a candidate
from verifying, gating and accepting it, and wrapping the existing Git replay as the
compatibility producer (plan §6.1, §6.6, §7.3 step 9). It is provisional: E1 (#650) has not
run, so D04–D06, D12 and D18 are still open and nothing here selects a composition
algorithm, grammar or base rule.

Code: `aethyme-broker::composition`, plus `collaboration_archive::snapshot_of_commit` and
`gate_trust::assess_trusted`. No schema, queue, gate or submit change.

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
  later steps use. `snapshot_of_commit` computes it from the repository without retaining
  anything. It equals the ID `retain_snapshot` would give the same commit (tested against
  the archive and the independent oracle).
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

### Reason codes

| Kind | Code | Used by |
|---|---|---|
| Refused | `unsafe_plan` | Legacy: commit provenance is ambiguous, and submit refuses the same plan |
| Refused | `untrusted_policy` | Legacy: the gate policy at the baseline is untrusted, and submit refuses too |
| Refused | `unknown_base`, `dependency_cycle`, `missing_input`, `competing_revisions`, `budget_exhausted`, `inseparable_selection` | Reserved for the composer (§7.2, §7.3) |
| Unsupported | `merge_commit` | Legacy: a pending commit with other than one parent. Submit refuses with `unsupported_commit_shape` |
| Unsupported | `snapshot_entry` | The candidate holds something a #652 snapshot cannot name, e.g. a gitlink |

The codes are stable strings, tested. Adding one is fine. Renaming one is a contract change
once a consumer reads them.

### The legacy adapter

`Broker::legacy_candidate(session)` builds the candidate `submit` would gate on now:

1. **Baseline**, chosen as submit chooses it: the fetched default branch under
   `verify-only`, otherwise the integration branch. Submit creates or fast-forwards the
   integration branch before simulating (the follows-main refresh). The adapter **computes**
   that result and does not perform it: a missing or behind integration branch means the
   main checkout's HEAD.
2. **Policy**: the gate policy committed at the baseline must be trusted.
   `assess_trusted` reaches the same decision as `require_trusted` but records nothing. A
   repository that submit would grandfather counts as trusted without being grandfathered.
3. **Plan and replay**: the same `build_submission_plan` and `replay_submission_plan` that
   submit runs, unchanged.
4. **Classify**: replay conflicts become `Conflict`. A tree equal to the baseline's becomes
   `NoChange`, except for a main-checkout session, which submit also gates. Anything else
   becomes `Candidate`.

It writes no queue row, gate result, slot, ref, trust record, event or action file. Like
`submission_plan`, it can leave unreachable Git objects, which `git gc` removes.

### Parity is tested against submit itself

`tests/candidate_boundary.rs` builds the candidate, proves the build changed nothing, then
runs the real `submit` on the same state:

| Case | Adapter | Submit |
|---|---|---|
| Clean merge after another promotion | `candidate`: same tree, baseline and source; subject equals the promoted commit's snapshot | promoted |
| Same line edited twice | `conflict` naming `shared.txt` and the session commit | `Conflict`, same path and commit |
| Empty commit | `no_change` | `no_changes`, not promoted |
| Merge commit in the session | `merge_commit` | `UnsupportedSubmissionCommit` |
| `verify-only`, provider moved the default branch | `candidate` on `origin/main`, source `upstream` | verified against the same commit and tree, not promoted |
| Main checkout moved past integration | baseline is main HEAD; integration not moved | verified against main HEAD, same tree |
| Gate policy added on the baseline, untrusted | `untrusted_policy`, no trust record or event written | `GatePolicyUntrusted` |
| Gitlink in the session | `snapshot_entry` | (not compared; Git merges it) |

"Changed nothing" means: every ref, the main checkout's HEAD, the worktree list, the
porcelain status (ignored files included) of the main checkout and each session, the merge
queue and the event count are identical before and after.

## Known divergence

- A gitlink, or a path #652 refuses, makes the adapter return `snapshot_entry` where submit
  would gate the merged tree. The boundary needs a subject. The legacy path keeps working
  because submit does not route through the boundary.

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
  exposing `replay_submission_plan` and `SubmissionReplay` to the crate.
- Retaining a candidate's subject in the archive, and candidate manifests and resolution
  requests (#665).

## Tests

- `cargo test -p aethyme-broker --test candidate_boundary`: every case above except the
  policy case, plus the stability of the reason codes.
- `cargo test -p aethyme-broker --test candidate_boundary_trust`: the untrusted-policy case.
  It is a binary of its own because it removes the suite's trust escape from the process
  environment.
- `cargo test -p aethyme-broker --lib collaboration_archive::tests::a_commit_snapshot`:
  `snapshot_of_commit` agrees with the oracle and with `retain_snapshot`, and refuses a
  gitlink.
