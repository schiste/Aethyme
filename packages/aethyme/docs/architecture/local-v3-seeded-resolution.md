# Local conflict-prefix continuation

Last Updated: 2026-10-20

Status: integration contract only. The composer helper, controller transition,
retained readers and native gate join need combined source qualification before
this path is enabled or described as complete. No provider dispatch or paid
model call is authorized by this document.

## Inputs and authority

The controller names the current target owner, exact blocked wave and target
version, exact archived resolution request and settled attempt token. The wave
must still link the original immutable conflict source and full-order plan.
The settled group is unverified source. It does not establish gate success,
acceptance, budget settlement or permission to retry.

Ordinary direct attachment continues to require full selected coverage. A
middle-conflict result covers only the applied prefix and its conflicting step;
it cannot pass that boundary. A final-step conflict uses direct attachment.

## Composition and commit

1. Load the exact current blocked-wave request/source/plan binding.
2. Validate the settled attempt against that request, including its input
   manifest, candidate subject/tree/commit and retained source closure.
3. Compose the remaining frozen suffix in its original order. Do not reselect,
   regenerate the plan, treat the seed as accepted history or split an atomic
   group across the prefix boundary. Refusal, unsupported inputs and a new
   conflict remain explicit outcomes.
4. Retain the seed, full candidate, recipe, original source/plan/request/result,
   manifest, source snapshots and manifest blobs before dropping their guards.
5. Under archive, response and current-owner guards, recheck cancellation,
   exact wave/version/revision/selection, latest sequence, unreleased revision,
   current request/source and absence of an attached result.
6. Commit the exact result link, complete candidate and final recipe/source in
   one SQLite transaction with a target-version and wave-state comparison.
   A stale comparison leaves the wave unchanged. Private retained artifacts
   confer no current authority.

The final wave keeps the settled seed group as `resolution_result`, the final
recipe as `recipe_object`, and a normal full `CompositionSource` record as
`composition_source`. The final source records the original frozen request,
complete composition and requirements. The recipe retains the original conflict
source and plan through versioned `seeded_conflict` metadata.

## Reading and fresh verification

Readers independently validate the complete candidate and normal source/recipe,
then validate the original request/result and prefix proof. They compare all
versioned seed metadata, full selected order, exact suffix, decision members,
profile pins and original record/object references. A seed candidate and final
candidate remain distinct in provenance.

A fresh native gate job binds the complete candidate, retained recipe/source,
seed request/result/manifest and full selected lineage. Existing job evidence
cannot be relabeled for the new candidate. This transition enables verification
only; private completion still requires the existing independent verifier and
source-provider acceptance path.

Historical completion reuse must prove this complete producer chain from its
retained roots after the old revision is released. It must not consult a current
owner epoch to recreate historical authority, nor regenerate missing evidence.

## Required checks

- A real A/B conflict plus nonempty C suffix reaches the complete candidate and
  fresh native gate preparation through the public controller operation.
- A final-step conflict works through direct attachment; the continuation
  helper refuses its empty suffix explicitly.
- Stale version/wave, changed request/source/result/selection, cancellation,
  released revision and missing roots leave the wave unchanged.
- Tampered seed or final candidate, recipe, plan, manifest, prefix/suffix or
  producer pins fail retained reading and fresh gate preparation.
- Old native observations and acceptance remain unchanged; this path neither
  charges nor settles an attempt.

Local compilation, tests and mutation checks remain pending for this integration
contract. The external and internal drives currently lack the required native
build headroom. Existing bounded composer checks are separate evidence.
