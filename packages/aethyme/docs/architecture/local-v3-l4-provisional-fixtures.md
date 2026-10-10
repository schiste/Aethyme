# Local v3 — L4 provisional composition fixtures (#664)

Last Updated: 2026-10-10

L4 (#663–#665) depends on E1 (#650), which selects the composition profile and freezes
the FX01–FX07 requirements and their behavior oracle. E1 has not run yet. To start L4
anyway, this record adds **provisional** fixtures and an oracle that stand in for E1's.
They do not close D04–D06 or D12, are not Q1 evidence, and claim none of T16–T23.

Code and data:

- `aethyme_testkit::composition_fixtures`: the composer-facing API, plus a private
  `oracle`;
- `crates/aethyme-testkit/fixtures/composition-provisional/`: inputs, the answer key and
  oracle self-tests;
- the oracle's own tests, which are unit tests inside the module because they read the
  answer key;
- `tests/composition_fixtures.rs`: public-API tests.

## Discipline (D03)

- **Frozen before any composer result.** The fixtures and oracle landed before any
  composer code existed. The revision below came from an independent review, made
  before any composer result existed, while #664's composer was being written in
  parallel and had not been tuned on anything. A later change to a requirement needs
  its own reviewed commit, never one made alongside composer tuning.
- **Blind composers.** The public API is the composer's whole view:
  - `CaseInput { case, contributions, scenarios }`;
  - `CaseInput::materialize`;
  - `judge` and `judge_scenario`.

  Nothing public exposes a required outcome, a behavior, the measured column or a
  fixture path. Inputs and answers live in separate directories. Tree, contribution,
  scenario and group ids are neutral (`t0`, `c1`, `s1`, `g1`), and the materialized
  repository's refs and commit messages are those ids. The case id (`FX02`) stays
  visible, so composer code is reviewed for case-specific branching.
- **Independent oracle.** It is in `aethyme-testkit`, which links no product crate, and
  it never treats a conflict-free merge as success.
- **Self-tested oracle.** Every case, held-out ones included, has wrong compositions
  the oracle must reject. Every candidate-accepting scenario has a correct one. Every
  kind of check was disabled once, and that made a test fail.

## What the oracle checks

**Per run (`judge`).**

1. The outcome must be one the plan accepts. For a candidate, the checks below also
   apply.
2. **No conflict markers**, indented or not, and every HTML file parses with unique
   ids.
3. **Preservation.** Let the *allowed* contributions be the composed ones (for a
   subtraction, the ones the expectations name). Then:
   - the candidate's file set is the baseline's, plus files the allowed contributions
     add, minus files they delete;
   - a file no allowed contribution changes is byte-identical to the baseline's;
   - a baseline element with an id that no allowed contribution changes keeps its tag,
     attributes and descendant text.
4. **Behaviors** from the answer key:
   - text and attributes;
   - containment;
   - counts;
   - producer/consumer call arity;
   - every `getElementById`/`querySelector("#…")` lookup resolves;
   - an event listener looks up a given id, directly or through a `const` bound to it.

   Script checks run on comment-stripped source.

**Per scenario (`judge_scenario(case, scenario, &[(order, observed)])`).**

1. Every declared order runs at least twice (a subtraction, twice with an empty
   order), and no undeclared order runs.
2. Every run passes `judge`.
3. All runs have one outcome.
4. Candidates are byte-identical:
   - across all orders when the scenario is commutative;
   - otherwise, across repeats of an order.

## Input semantics

- **Accepted history and unknown bases.** `Materialized.accepted` is the scenario's
  baseline commit and its accepted ancestors. A contribution's base is *known* if it is
  one of those, or the result commit of a retained contribution. Any other base is
  `unknown_base`: for example, FX03 `c3`, based on an integration nothing records.
- **Retention.** `materialize` leaves a scenario's `unretained` contributions out
  entirely. A composer cannot read their source.
- **`requires`** pins an exact contribution revision.
- **`revision_of`** names an alternative revision of the same change. Selecting two
  revisions is `competing_revisions`.
- **`atomic_group`.** A selection containing any member must contain every member. A
  `revision_of` member fills its original's place, so each change in the group appears
  in exactly one revision.
  - Selecting part of a group is `missing_input` or `inseparable_selection`. Both name
    the split group, so either is accepted.
  - A consumer pinned to revision 1 with only revision 2 selected (FX06 s6) is
    `competing_revisions` or `missing_input`. Both name the pinned prerequisite.

## Cases

The outcome vocabulary is:

- `candidate`;
- `conflict`;
- `unsupported`;
- `resolution_required`;
- the refusals `unknown_base`, `dependency_cycle`, `missing_input`,
  `competing_revisions`, `budget_exhausted` and `inseparable_selection`.

"Text" means plain sequential three-way `git merge-file` against each contribution's
exact base. The unit test `the_provisional_text_column_is_measured` measures it.

| Case / scenario | Plan requires | Text measured |
|---|---|---|
| FX01 s1: label, aria, child (6 orders) | candidate | candidate, passes |
| FX02 s1: re-indented move plus edit | candidate | conflict |
| FX02 s2: identical twins | conflict, resolution_required or unsupported | candidate, **rejected**: the edit lands on the twin that stayed |
| FX03 s1: c2 (on c1) edits c1's item; baseline t1 has an accepted footer change | candidate | candidate, passes |
| FX03 s2: c1 redelivered after c2 | candidate | **conflict**: idempotence must come from lineage |
| FX03 s3: unrecorded base | unknown_base | not measured (planning) |
| FX04 s1: rename plus handler using the old id | candidate after synthesis, or resolution_required | candidate, fails |
| FX05 s1: same property | conflict or resolution_required | conflict |
| FX06 s1: atomic pair | candidate | candidate, passes |
| FX06 s2: consumer alone | missing_input | not measured |
| FX06 s3: producer alone | missing_input or inseparable_selection | not measured |
| FX06 s4: cycle | dependency_cycle | not measured |
| FX06 s5: two revisions selected | competing_revisions | not measured |
| FX06 s6: pin on the unselected revision | competing_revisions or missing_input | not measured |
| FX07 s1: X minus B, originals retained | candidate (fresh composition), inseparable_selection or unsupported | candidate, passes (A alone on t0) |
| FX07 s2: A not retained | inseparable_selection or unsupported | not measured |
| FX07 s3: keep a consumer of X | inseparable_selection or missing_input | not measured |
| HX01 (FX01 variant) | candidate | conflict (adjacent lines) |
| HX02 (FX02 variant) | candidate | conflict |
| HX03 s1 (FX03 variant) | candidate | conflict (edit next to the accepted change) |
| HX04 (FX04 variant) | candidate or resolution_required | candidate, fails |
| HX05 (FX05 variant) | conflict or resolution_required | conflict |
| HX06 s1 (FX06 variant) | candidate | candidate, passes |
| HX07 s1 (FX07 variant) | candidate, inseparable_selection or unsupported | candidate, passes |

What the measurement shows for L4:

1. **Text misses the mandatory move-plus-edit positive** (FX02 s1, HX02). A real
   profile from E1 is still needed.
2. **Text silently mis-merges identical subtrees** (FX02 s2). A composer must refuse
   ambiguous correspondence. It cannot rely on merge cleanliness, and the oracle no
   longer accepts any candidate there.
3. **Duplicate delivery is a lineage question.** A redelivered contribution whose change
   a later contribution already edited conflicts under a line merge (FX03 s2). The
   composer must skip it by identity.
4. **Text conflicts on adjacent lines** (HX01, HX03), so FX01 and FX03 passing does not
   generalize.

## Independent review (2026-10-10)

The review was made before any composer result existed. Against the first version,
`judge` accepted 13 of 13 hand-built wrong candidates. A composer reading the public
`Case` (required outcomes, the self-test trees) passed all 20 scenarios. This revision
fixes each finding, and the attacks are ported as must-reject unit tests:

- the answer-key leak (H1);
- untouched content going unchecked (H2);
- FX03 not testing lineage (H3);
- no order or determinism check (H4);
- FX04's weak handler checks (M1);
- guessable twins (M2);
- marker and leftover-file gaps (M3);
- held-out coverage (M4);
- atomic-group semantics (M5);
- unrecorded bases, unretained sources, FX05's outcome set and `=>` arity (L1–L5).

## Replacement

When E1 lands, its fixtures, requirements and evidence replace this directory. The
public API can stay if E1's oracle fits behind it. Otherwise composer tests switch to
E1's oracle, and this provisional set is deleted rather than kept beside it.
