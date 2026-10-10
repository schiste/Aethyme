# Local v3 — L4 provisional composer (#664)

Last Updated: 2026-10-10

This record covers the first composer: plan §7.3 steps 1–5 over contributions the archive
retained, emitting through the #663 candidate boundary. It is **provisional**. E1 (#650)
has not run, so D04 (engine, grammar, mode), D05 (base normalization), D06 (exclusion from
synthesized results) and D12 (prerequisites, atomic groups, cycles) are not closed. The
only profile is a line merge, chosen because it is the baseline E1 compares against, not
because it is supported. It is measured against the provisional fixtures of #733, which
stand in for E1's.

Code: `aethyme-broker::composer`, plus `collaboration_archive::retained_contribution` and
a `reason` on `composition::CompositionConflict`. No schema, queue, gate or submit change;
nothing calls the composer outside tests yet.

## Decided here

### Inputs come from the archive, never a worktree

`compose(store, repo, request)` takes:

- the baseline **commit**, which the composer retains first, reading and verifying it from
  Git as capture does (a baseline nobody based a contribution on is otherwise absent);
- the accepted history the baseline descends from, as commits;
- a catalog of `ContributionSpec`s: each a lineage record ID, or none when the caller knows
  the contribution only by name, plus the declared `requires`, `atomic_group`,
  `revision_of` and `derived_from`;
- the deliveries in policy order, and a `CompositionBudget`.

Each lineage record is re-read and must hash to its row and decode to its ID; every
manifest and blob is checked against its digest as it is read. The archive is held shared
for the whole composition, so reclamation (#659) cannot remove an input mid-read. A
contribution that is not retained (or known by name only), or a baseline Git cannot
supply, is `missing_input`; a baseline holding an entry a snapshot cannot name is
`snapshot_entry`.

### Planning refusals (§7.2, §7.3 steps 2–4)

| Situation | Outcome |
|---|---|
| A delivery the catalog does not know | `missing_input` |
| Two revisions of one contribution (`revision_of` chain) | `competing_revisions` |
| A requirement satisfied only by another revision of it | `competing_revisions` |
| A synthesized result selected beside one of its constituents | `competing_revisions` |
| A requirement, or a member of a touched atomic group, not selected | `missing_input` |
| Requirements that form a cycle | `dependency_cycle` |
| A base that is the result of a known but unselected contribution | `missing_input` |
| A base that is neither accepted history nor a selected result | `unknown_base` |
| Any budget limit exceeded | `budget_exhausted` |

Lineage is read from the archive, not trusted from the caller. A base in accepted history
(the baseline or an accepted ancestor) applies three-way onto the baseline. A contribution
whose retained base equals another selected contribution's retained result goes after it
and applies only its own base-to-result change. A contribution delivered twice, by name or
by lineage, applies once: re-merging it as text would conflict with the work built on it
(FX03). Order is topological over requirements and inherited bases, then the order
of first delivery. Nothing else is sorted, so no order independence is implied: the
fixtures' commutativity claims are tested by permutation.

### Applying a contribution (§7.3 step 5)

Each contribution is applied to the accumulator, path by path, as the three-way
`(its base, accumulator, its result)`, only on paths its own change touches. It applies
atomically: one conflicting path and none of its paths apply, and the outcome is a
`Conflict` naming every conflicting path of that contribution with a reason:

| Reason | When |
|---|---|
| `content` | `git merge-file` reports overlapping changes |
| `delete_modify` | One side deleted the path, the other changed it |
| `add_add` | Both added the path with different content |
| `mode` | Both changed the entry kind differently |
| `binary` | Both changed a binary file (Git's NUL test) or a symlink: exact replacement only |
| `moved_block` | Either side moved a block within the file (below) |

`git merge-file` runs with no system or global configuration, so a configured conflict
style or driver cannot change the profile. The engine version is recorded.

### The profile's own limit: moved blocks

A line merge has no notion of identity. When one side moves a block and the other edits
inside it, the edit either conflicts with the deletion or, when the block has an identical
twin, lands cleanly on the copy that did not move (FX02 `identical-twins`, measured by
#733 as a silent wrong merge under plain `git merge-file`). Only the complete-candidate
behavior check sees that, so the profile refuses to guess: if either side's line diff
deletes three consecutive non-blank lines (compared without surrounding whitespace) and
inserts the same three elsewhere, a concurrent change to that file is a `moved_block`
conflict. The rule is about the merge engine, not any fixture: it also makes a
re-indented block conflict with a concurrent edit, which is the right answer for a line
merge. A structural engine selected by E1 replaces it.

### The candidate

The result is written into `repo` as Git objects only: blobs, trees, and one commit whose
only parent is the baseline's commit, with a fixed identity and date so the same
composition of the same inputs is the same commit. No ref, index or worktree changes
(tested). Its subject is checked twice, once from the composed entries and once by
`snapshot_of_commit`, and must agree. The candidate snapshot is then retained in the
archive, and a recipe record (`aethyme.composition-recipe/experimental-v0`) is stored
beside it: profile, engine and version, baseline, candidate, the order with every input's
lineage, base and result IDs, the number of deliveries, budget limits and usage, and for a
recomposition what it was recomposed from.

A composer candidate is **unverified**. A clean text merge is not behavior (FX04: the
merge is clean, the handler keeps an id the other contribution renamed). Verification,
resolution requests (#665) and acceptance are separate steps; the composer has no
canonical write capability.

### Recomposition, never relabelling (§7.4)

`recompose_without(store, repo, request, Subtraction { from, remove, keep })` composes
`from`'s recorded constituents minus `remove`, plus `keep`, on the request's baseline, as
a new candidate with its own subject. It refuses with `inseparable_selection` when `from`
records no constituents, a removed contribution is not one of them, a remaining
constituent is no longer retained, or a kept contribution requires `from` or was built on
its result. It never returns `from`'s result under another name.

## Measured on the provisional fixtures

The fixture test uses only the fixtures' composer-facing API (scenario inputs,
contribution metadata, `materialize`), never their answer key, and never branches on a case
or scenario id. Each scenario is materialized, its retained contributions captured through
#658, composed from the archive alone, rebuilt with `reconstruct`, and judged by
`judge_scenario` over every declared order run twice (a subtraction twice with no order),
which also checks that outcomes and candidates are identical across runs.

**15 of 17** scenarios are accepted. The two that are not:

| Scenario | Outcome | Why |
|---|---|---|
| FX02 s1 (move plus edit) | conflict `moved_block` | The mandatory structural positive needs E1's engine; a line merge cannot carry the edit along the move. |
| FX04 s1 (semantic interaction) | candidate, behavior fails | The text merge is clean but the handler keeps an id the other contribution renamed. Only the complete-candidate check sees it; resolution is #665's. |

FX02 s2 (identical twins), where plain `git merge-file` silently puts the edit on the wrong
card, is a `moved_block` conflict and accepted. The test asserts a floor of 15 accepted
scenarios, so a regression fails without the test naming any scenario.

**Held-out**, run once after the profile was fixed, never used for tuning: **8 of 12**
accepted. HX01, HX02 and HX03 s1 are conflicts where a candidate is required (the same
class as FX02 s1: the text profile is conservative), and HX04 is a clean candidate whose
behavior fails (the same class as FX04). None is a planning error.

Rules the fixtures do not isolate have their own small repositories in the same test:
each conflict reason, mode-plus-content composition, revert to `no_change`, the twins
layout, inherited order, accepted-history bases, repeats by name and by lineage, every
planning refusal, recomposition, each budget limit, and no ref movement.

## Not decided here

- **The engine.** D04 is E1's. A structural engine becomes a new profile ID; this one is
  not extended.
- **Base normalization beyond exact lineage.** D05: a base explained by an accepted
  baseline other than the request's, or by a reproducible subset, is `unknown_base` here.
- **Candidate retention.** A retained candidate has no retention root, so reclamation may
  remove it. Keeping one is the acceptance step's job (L5).
- **Wiring.** Nothing produces a composition request yet (L5 controller), and the gate
  boundary waits for #725.
