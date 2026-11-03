# E1 original composition fixtures (FX01–FX07)

**Adopted as E1's original frozen inputs.** These fixtures were first added as a
provisional stand-in so that L4 (#663–#665) could be built before E1 (#650) ran.
E1 then froze them unchanged as its original FX01–FX07 inputs, before any
structural trial. The inventory is
[`../composition-e1/frozen-input-inventory.json`](../composition-e1/frozen-input-inventory.json):
55 paths with SHA-256 digests, checked by
`original_e1_inputs_match_the_pretrial_inventory`. The "provisional" label is
superseded. The directory name and the `provisional-v0` schema string stay, because
renaming either would change frozen paths or digests.

The Q1 evidence record is
[`evidence/q1-composition-profile.json`](../../../../../../../evidence/q1-composition-profile.json).
History of the provisional profile:
`packages/aethyme/docs/architecture/local-v3-l4-provisional-fixtures.md`.

**Do not edit any input file.** A changed input breaks the freeze. Any new fixture
belongs in its own reviewed, separately frozen set.

The source is synthetic and public: tiny HTML pages and a small TypeScript surface.
These are unit fixtures, materialized into temporary Git repositories. They are not an
evaluation; evaluations run only against Playground repositories.

## Layout

- `inputs/{cases,held-out}/<case>/`: what a composer may know.
  - `trees/t<n>/`: whole source trees.
  - `contributions/c<n>/`: each contribution's whole result tree.
  - `inputs.json`: tree ancestry (whether each tree is accepted history),
    contribution metadata (base, `requires`, `atomic_group`, `revision_of`,
    `synthesized`, `derived_from`), and scenarios (`s<n>`: baseline, request, orders,
    `commutative`, `unretained`). Ids are neutral on purpose.
- `expectations/{cases,held-out}/<case>.json`: **the answer key**. It holds the
  required outcomes, the behaviors, the allowed changes, and the measured
  provisional-text column. Only the oracle reads it.
- `oracle-selftest/<case>/<scenario>/<variant>/`: compositions that test the oracle.
  `correct*` must be accepted and `wrong-*` rejected. **Oracle tests only; never serve
  them to a composer.**
- `held-out` cases: **never use them to tune a composer.** Run them only to report how
  a frozen composer generalizes.

## Rules for composer tests

1. Use only the public API of `aethyme_testkit::composition_fixtures`:
   - `cases()` and `held_out_cases()`;
   - `CaseInput::materialize(scenario, root)`;
   - the `ScenarioInput` and `ContributionInput` metadata;
   - `judge_scenario` with every declared order run at least twice.
2. Never read `expectations/` or `oracle-selftest/`, and never branch on a case or
   scenario id.

A clean text merge is never success. Only `judge_scenario` passing is.
