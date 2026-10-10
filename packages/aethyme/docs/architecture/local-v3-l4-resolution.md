# Local v3 — L4 resolution requests and candidate manifests (#665)

Last Updated: 2026-10-10

This record covers plan §7.3 steps 6–9, §7.4 and §7.5 on top of the provisional composer
(#664, `local-v3-l4-composer.md`). It is **provisional**. E1 (#650) has not run, so D06
(exclusion from synthesized results), D08 and D12 stay open. No resolver ships: the
resolver is a narrow interface, exercised by test doubles. D15 sets real token, time and
byte caps before any paid dispatch.

Code: `aethyme-broker::resolution`, plus crate-private helpers in `composer`
(`snapshot_entries`, `blob`, `materialize_candidate`, `retain_commit`) and a
`Synthesized` value on `composition::CompositionMode`. No gate, queue, submit or broker.db
change. Nothing calls this outside tests yet.

## Decided here

### Three records, all in the archive

All three are #653 canonical records, written as archive objects and read back with
`read_record`, which refuses bytes that do not decode to the record's ID.

| Record | Schema | Written by |
|---|---|---|
| Candidate manifest | `aethyme.candidate-manifest/experimental-v0` | `write_manifest` |
| Resolution request | `aethyme.resolution-request/experimental-v0` | `request_resolution` |
| Synthesized group | `aethyme.synthesized-group/experimental-v0` | `resolve` |

**Candidate manifest.** One per composition, whatever its outcome. It records:

- the outcome code;
- the profile;
- the baseline commit, plus its snapshot when Git can supply it;
- every delivered input exactly as retained (lineage record, base and result snapshot), or
  its name alone when nothing of it is retained;
- the order and the recipe;
- the unresolved decisions: each conflict, and each resolution request raised for it.

Only a candidate's manifest has `subject`, `candidate_commit` and `mode`, and only it has
`requires_verification: true`. An attempt that produced nothing is recorded, but is never
success-shaped.

**Resolution request.** It is raised from exactly two things.

- **A conflict.** The group is the step that conflicted, plus every earlier step whose own
  change touches a conflicting path. The accumulator is the composition of the steps
  before it, rebuilt and retained: it is an input, never offered for acceptance, so
  atomic grouping is lifted to rebuild it.
- **A candidate whose independent checks failed** (§7.3 step 6). The group is every
  contribution in the candidate, because nothing narrows an interaction without a
  qualified analysis (AQ0). The accumulator is the candidate itself.

Anything else, including a candidate with no failed check, is `invalid_request`: there is
nothing to resolve.

The request carries:

- every member's lineage record and base and result snapshots, read from the archive;
- the baseline and the accumulator, as retained snapshots;
- the triggering conflicts or checks;
- references to the independent requirements;
- the **scope**: exactly the paths the group's own changes touch;
- the **protected** paths: `.aethyme/`, `.github/`, `.gitattributes`, `.gitmodules`, plus
  every requirement that names a harness path;
- the authorized preference, if any;
- the allowance at the time it was raised.

**Synthesized group.** It records:

- the request;
- the group key;
- the constituents, and `derived_from` naming them;
- the resolver's identity;
- the brief, when there is one;
- the attempt number;
- the baseline, the accumulator and the new candidate;
- `requires_verification: true`.

### What `resolve` enforces, in order

1. **The allowance.** There are at most `MAX_SYNTHESIS_ATTEMPTS` (2) per group, and each
   is charged in its own transaction **before** dispatch, so a crash never refunds an
   attempt.
   - The group key is a digest of the members' base and result snapshots. None of these
     resets it: a new candidate, a different order, a new contribution name, a new commit
     carrying the same change, a new capture operation, a rewritten brief, or another
     resolver.
   - An infrastructure failure still consumes its attempt. Refunding it would make cost
     unbounded.
   - A spent allowance returns `budget_exhausted` without calling the resolver.
2. **Inputs.** The resolver reads through `ResolutionInputs`, and can reach only the
   request's own snapshots at its scoped paths. Any other read is `out_of_scope_read`.
3. **Scope (T23).** Each of these makes the whole proposal a `scope_violation`:
   - a write to a protected path, even one inside the scope;
   - a write outside the scope;
   - a write that makes a file executable or a symlink where it was not one before.
4. **No last writer (§7.5, FX05).** This applies to a conflict with no authorized
   preference. Leaving a conflicting path exactly as one side had it drops the other
   side's change, so the attempt is `unresolved`. That covers both writing the
   contributor's version and writing nothing for the path. With a preference, the
   preferred side may win.
5. **Materialize.** An accepted proposal becomes a **new** candidate:
   - It is built through the composer's own materialization path: Git objects only, the
     baseline commit as parent, the commit's snapshot checked against the entries, then
     retained.
   - Its producer is `aethyme.resolve.bounded/provisional-v0` and its mode is
     `synthesized`.
   - It is marked `requires_verification`, and nothing from its constituents' analyses
     carries over.
   - A proposal that changes nothing is `inconclusive`.

The failure states are exactly `unresolved`, `inconclusive`, `scope_violation`,
`infrastructure_deferred` and `budget_exhausted`. None of them carries a candidate.

### Alternate views (§7.4)

A synthesized X records its constituents. "X without B" uses the composer's
`recompose_without`: it composes the remaining **originals** into a new candidate, whose
subject is never X's. Without the constituents' retained records, the request is refused
with `inseparable_selection`. Both cases are tested on a resolver-synthesized X.

### The allowance ledger is created on first use

The ledger is the `resolution_attempts` table in `state.db`. It is created with
`CREATE TABLE IF NOT EXISTS` when first charged, not as a numbered migration.

The reason is that the L3 stack (#718, #730) already claims schema 5 and 6 for its own
tables. A number taken here would make a store migrated by one stack skip the other
stack's tables. No other reader consults this table, and an older binary ignores it.
When the stacks merge, it becomes the next numbered migration.

## Measured (provisional fixtures, blind API)

The pipeline runs each scenario as: compose; write the manifest; for a candidate,
reconstruct it from the archive and run the independent check; raise a request when the
check fails. Every declared order runs twice. Scenarios are judged with `judge_scenario`,
and the test never names a case.

| | Composer alone (#664) | With verification and requests |
|---|---|---|
| Provisional scenarios accepted | 15/17 | **16/17** |
| Held-out scenarios accepted | 8/12 | **9/12** |

- **FX04 and its held-out twin HX04:** the clean but wrong text candidate now fails its
  check and becomes `resolution_required`. The fixture accepts that outcome. The request
  scopes both members and both files.
- **FX02 s1 (move plus edit) is still a gap,** together with HX01, HX02 and HX03 s1. They
  conflict, and a structural engine (E1, D04) is required.

A test-double resolver handles the interaction case: it propagates identifier renames,
reads only the request's inputs, and is located by behavior, not by name.
- On FX04, its proposal becomes a new candidate that passes the independent check.
- On HX04, run once after the fact and never tuned on, it also passes.

**FX05 without a preference:**
- a resolver that declines gives `unresolved`;
- last writer and first writer are both refused as `unresolved`;
- an averaging proposal becomes a candidate that requires verification, and its check then
  fails.

## Open

- **Rooting:** manifests, requests and synthesized candidates have no retention root yet,
  so reclamation may remove them. Keeping them is the acceptance step's job (L5), as for
  composer candidates.
- **Group narrowing:** for failed checks the group is the whole candidate. A qualified
  analysis could narrow it, but none is consumed yet.
- **Real checks:** the independent check in tests is the fixtures' oracle. In production it
  is the gate run on the complete candidate. That wiring waits for #725.
- **Objectives:** there is no objective or issuer record yet, so the allowance is per
  group, not per objective. A scope expansion or new objective needs explicit issuer
  authorization (§7.5), and nothing grants that here.
