# Provisional composition fixtures (FX01–FX07)

**Provisional.** These stand in for the E1 (#650) fixtures so L4 (#663–#665) can be
built against concrete cases. E1's frozen requirements, oracle and evidence
(`evidence/q1-composition-profile.json`) replace them once they land. Nothing here is
E1 evidence or a Q1 claim. Decision record:
`packages/aethyme/docs/architecture/local-v3-l4-provisional-fixtures.md`.

The source is synthetic and public: tiny HTML pages and a small TypeScript surface.
These are unit fixtures, materialized into temporary Git repositories by
`aethyme_testkit::composition_fixtures::materialize`. They are not an evaluation.
Evaluations run only against Playground repositories.

## Layout

- `cases/<FXnn-name>/`: one directory per plan case.
  - `trees/<name>/`: whole source trees, such as the baseline `S0` or an unrecorded
    intermediate integration.
  - `contributions/<id>/`: each contribution's whole result tree.
  - `case.json`: the inputs, plus two expectation columns.
    - `required_outcomes`: the outcomes plan v3 accepts.
    - `provisional_text`: what plain three-way text merge (`git merge-file`, applied
      sequentially per plan §7.3 step 5) actually produced. A test measures this column,
      so it is never a guess.
  - `requirements.json`: independently specified behaviors. Only the oracle reads it.
- `held-out/`: variants with renamed elements and reordered siblings. **Never use them
  to tune a composer.** Run them only to report how a frozen composer generalizes.
- `oracle-selftest/<case>/<scenario>/<variant>/`: hand-written compositions that test
  the oracle itself. `correct*` must be accepted and `wrong-*` rejected. **These are
  oracle tests only; never serve them to a composer.**

## Rules for composer tests

A composer gets the materialized repository and a scenario's `ScenarioInput`: the
baseline, the request, the orders and any unretained contributions. It does not read
any of these:

- `required_outcomes`;
- `provisional_text`;
- `requirements.json`;
- `oracle-selftest/`;
- `held-out/`, while being tuned.

A clean text merge is never success. Only `judge` passing on the complete candidate is.
