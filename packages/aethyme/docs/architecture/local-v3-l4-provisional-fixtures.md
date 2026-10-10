# Local v3 — L4 provisional composition fixtures (#664)

Last Updated: 2026-10-10

L4 (#663–#665) depends on E1 (#650), which selects the composition profile and freezes
the FX01–FX07 requirements and their behavior oracle. E1 has not run yet. To start L4
anyway, this record adds **provisional** fixtures and an oracle that stand in for E1's.
They do not close D04–D06 or D12. They are not Q1 evidence, and they do not claim any
of T16–T23.

Code and data:

- `aethyme_testkit::composition_fixtures`: the loader, `materialize`, and the oracle
  `judge`;
- `crates/aethyme-testkit/fixtures/composition-provisional/`: the cases, held-out
  variants and oracle self-tests;
- `crates/aethyme-testkit/tests/composition_fixtures.rs`: the tests.

## Discipline (D03)

- **Frozen before any composer.** The fixtures and oracle land in their own commit,
  before any composer code exists. A later change to a requirement needs its own
  reviewed commit, never one made alongside composer tuning.
- **Independent oracle.** The oracle is in `aethyme-testkit`, which links no product
  crate. It checks behaviors stated in `requirements.json`:
  - element text and attributes;
  - which element sits inside which ancestor;
  - counts;
  - call arity between a producer and its consumers;
  - that every looked-up element id exists.

  Every candidate must also parse, with unique ids and no conflict markers. A
  conflict-free merge is never itself evidence of success.
- **Blind composers.** A composer test gets only the materialized repository and a
  scenario's `ScenarioInput`. It never sees `required_outcomes`, `provisional_text`,
  `requirements.json`, `oracle-selftest/`, or (while being tuned) `held-out/`.
- **Self-tested oracle.** Each positive scenario has a correct composition the oracle
  must accept. Each case has wrong compositions it must reject, such as a last-writer
  pick, an edit on the similar or stayed-behind subtree, an inherited change applied
  twice, a relabelled synthesis, or a candidate where only a refusal is allowed. Each
  kind of check was disabled once, and that made a self-test fail.

## Cases

The outcome vocabulary is:

- `candidate`;
- `conflict`;
- `unsupported`;
- `resolution_required`;
- the refusals `unknown_base`, `dependency_cycle`, `missing_input`,
  `competing_revisions`, `budget_exhausted` and `inseparable_selection`.

"Text" means plain sequential three-way `git merge-file`, as **measured** by
`the_provisional_text_column_is_measured`.

| Case / scenario | Plan requires | Text measured |
|---|---|---|
| FX01 compose-all (6 orders) | candidate | candidate, passes |
| FX02 move-and-edit (re-indented move) | candidate | conflict |
| FX02 identical-twins | candidate (edit follows the move) or conflict | **candidate, fails**: a silent wrong merge |
| FX03 inherited (c2 based on c1's result) | candidate | candidate, passes |
| FX03 duplicate-delivery | candidate | candidate, passes |
| FX03 unrecorded-base | unknown_base | not measured (planning refusal) |
| FX04 interaction (rename plus handler using the old id) | candidate after synthesis, or resolution_required | candidate, fails |
| FX05 same-property | conflict | conflict |
| FX06 atomic-pair | candidate | candidate, passes |
| FX06 consumer or producer alone | missing_input | not measured |
| FX06 cycle | dependency_cycle | not measured |
| FX06 competing or incompatible prerequisite revisions | competing_revisions | not measured |
| FX07 A without B, originals retained | candidate (fresh composition), inseparable_selection or unsupported | candidate, passes (measured as A alone on S0) |
| FX07 A without B, A not retained | inseparable_selection or unsupported | not measured |
| FX07 A without B, keeping a consumer of X | inseparable_selection or missing_input | not measured |
| HX01, HX02, HX05 (held-out) | candidate, candidate, conflict | conflict, conflict, conflict |

What the measurement shows for L4:

1. **Text does not meet the mandatory move-plus-edit positive (FX02).** A real profile
   from E1 is still required.
2. **Text silently mis-merges identical subtrees** (FX02 identical-twins). The line diff
   deletes the second twin, so the edit lands on the card that stayed. A composer on the
   text profile must not accept a candidate on merge cleanliness. It needs the
   complete-candidate check (§7.3 step 8, T20), or it must refuse ambiguous
   correspondence.
3. **Text conflicts on adjacent lines.** The held-out FX01 variant (HX01) conflicts where
   FX01's spacing let it through, so FX01 passing does not generalize.
4. **Lineage matters, the engine does not.** Merging c2 against its exact base (c1's
   result) applies c1 once. Repeated deliveries are no-ops.

## Replacement

When E1 lands, its fixtures, requirements and evidence replace this directory. The
loader and `judge` API can stay if E1's oracle fits behind them. Otherwise composer
tests switch to E1's oracle, and this provisional set is deleted rather than kept beside
it.
