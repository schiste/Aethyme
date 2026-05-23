# Parity-test contract: byte-identical, not just functional

This note documents the methodological rationale for the three parity
tests in `tests/local/`:

- `test_eval_navigation_context_adapter_parity.py` (bug-fix eval)
- `test_eval_explain_repo_adapter_parity.py` (explain-repo eval)
- `test_eval_navigation_ctf_adapter_parity.py` (navigation-ctf eval)

The contract these tests enforce is **byte-identical**, not "functional"
or "semantically equivalent." This choice is load-bearing for the
AethymeBench extraction work (see `extraction-plan.md` Stage A.1.10 and
the Stage B validation gate). If a future contributor weakens it to
"functional," the chain of reasoning that lets us call the extracted
framework reproducible breaks silently. Hence this doc.

---

## What the parity tests assert

Each test runs the same eval-flow helper twice against the same target
repo + task:

1. **Legacy path** — direct Python call into `build_task_pack` /
   `build_task_context` (the historical, Aethyme-coupled transport).
2. **Adapter path** — same logical operation routed through
   `get_adapter("aethyme")` → CLI subprocess → JSON parse.

Both paths must produce navigation-context dicts that are equal when
serialized with `json.dumps(..., sort_keys=True)`. A single byte of
divergence in any value at any nesting depth fails the test.

A complementary assertion is that `get_adapter("graphify")` (any
non-Aethyme tool) returns `None` from these helpers — the explicit
opt-out signal that triggers the tool-context-file flow instead of an
Aethyme-shaped payload.

## What "byte-identical" precisely means here

It does *not* mean the raw bytes of the two intermediate Python objects
match (that would depend on dict iteration order, float repr, and other
non-load-bearing artifacts). It means **the canonical-JSON serialization
of the two outputs is identical**, byte-for-byte, where canonical-JSON
is defined as `json.dumps(value, sort_keys=True)`.

This is the contract that matters because the eval's prepare flow
ultimately writes `json.dumps(nav_context, indent=2)` to a file that the
agent reads. JSON-level equality is the actual agent-facing surface; raw
Python identity is not.

Calling the contract "byte-identical" in conversation and commit
messages is a deliberate shorthand. The literal expansion is
"canonical-JSON-identical." If you ever tighten the test to assert raw
file-byte equality on the prepared `navigation_context.json`, the
contract becomes *stronger*, not weaker, and this doc is still correct.

## Why byte-identical and not just functional

A functional contract — "both paths produce an agent that fixes the
bug at the same rate" — is what most refactor parity tests assert.
For the AethymeBench framework, that's not enough. Three reasons:

1. **Eval outputs are the artifact.** The whole point of the
   framework is to produce reproducible, comparable, audited numbers.
   If the transport change (direct Python → CLI subprocess) silently
   shifts a single anchor, file order, or score by 0.3 points, the
   published comparison between "Aethyme on commit X" and "Aethyme on
   commit Y" is no longer attributable to Aethyme — it's confounded
   by transport drift. Cardinal rule #2 ("never modify the framework
   to improve eval scores") presupposes that the framework's outputs
   are *stable across non-semantic changes*. Byte-identical is how we
   actually enforce that.

2. **Functional parity is unfalsifiable at this scale.** "The agent
   fixes the bug" depends on the LLM. Two runs of the same agent
   against the same prompt produce different token counts and
   sometimes different fixes. A functional test for transport parity
   would have to compare distributions across many runs — expensive,
   noisy, and slow to fail. A byte-identical test fails in 200 ms on
   the first divergent byte, against the input the agent actually
   sees. The cost of catching transport drift drops by ~3 orders of
   magnitude.

3. **The extraction is the use case.** AethymeBench's Stage 2.5
   (monorepo soak) and Stage 3.9 (bidirectional verification) both
   re-run historical evals through the moved code and require
   byte-identical output. The validation gate is "the framework can
   run with `tool=aethyme` via subprocess-only AND produce
   byte-identical results to the legacy path." Without these parity
   tests already enforcing that on every commit, Stage 2.5 would
   discover divergence weeks late, after it's been baked into the
   moved code. The tests make the gate *check itself* on every CI
   run.

## When divergence is acceptable

**Never, while the legacy path exists.** A failure of any of these
three tests blocks merge.

The contract becomes vacuous (but not violated) only when the legacy
path is deleted entirely — that is, when `tool=None` no longer routes
through direct Python and instead routes through the Aethyme adapter
by default. **That decision landed 2026-05-19 as Stage B item 2.3
(Option A — full extraction cleanness).** Until 2.3 *executes* (after
2.16 captures golden snapshots), removing or weakening these tests is
a Cardinal-rule-#2 violation: it allows the framework to drift in ways
that change eval outputs without detection.

### Migration plan when 2.3 executes

Once 2.16 has written golden snapshots and 2.3 deletes the legacy
direct-Python branch, the tests should **not** be deleted — they
should be repurposed to assert that `tool=get_adapter("aethyme")`
produces output matching a checked-in golden JSON file (the same
byte-identical contract, just against a stable reference instead of a
live alternative implementation). That keeps the canary alive across
the transport change.

Concrete steps for the 2.3 migration commit:

1. The two test functions in
   `tests/local/test_eval_navigation_context_adapter_parity.py`
   (and the `explain_repo` / `navigation_ctf` parallels) split roles:
   - **First test** (currently `test_legacy_and_adapter_paths_produce_identical_context`)
     gets renamed to `test_adapter_path_matches_golden_snapshot`. It
     loads `tests/local/fixtures/<eval-type>-nav-context.golden.json`
     and compares the adapter output via `json.dumps(..., sort_keys=True)`
     against that file's contents. Same canonical-JSON equality, frozen
     reference instead of live one.
   - **Second test** (`test_non_aethyme_tool_returns_none_on_*`) stays
     unchanged. The opt-out contract is independent of how the Aethyme
     path is implemented.
2. The golden JSON files come from 2.16's snapshot run — they are the
   output the framework produced on the last commit before 2.3 deleted
   the legacy path. The audit trail is the SHA of that commit, recorded
   in the golden file's frontmatter or sibling `.meta.json`.
3. Updating a golden file thereafter is a deliberate methodology-drift
   event and follows the CalVer-tier rules in `extraction-plan.md`'s
   methodology-drift section. A golden-file change is **not** equivalent
   to a refactor — it is a reproducibility breaking change and must be
   reviewed accordingly.

## Why this isn't documented inline in the tests

The test docstrings explain *what* they assert. The reasoning above —
why byte-identical instead of functional, why this contract is the
extraction's validation gate, when it's allowed to relax — is
extraction-strategy context, not test context. Putting it in
docstrings would either:

- Bloat each test file with ~80 lines of unrelated rationale, or
- Get out of sync as the extraction plan evolves (the docstrings have
  no `last-updated-when` signal; this doc is referenced from
  `extraction-plan.md` and updates land together).

This doc is the single source of truth for the contract; the tests
link to it implicitly via shared vocabulary ("byte-identical",
"Cardinal rule #2") and via `extraction-plan.md` Stage A.1.10.

## Cross-references

- `extraction-plan.md` row 1.10 (this doc) and the Stage B validation
  gate description.
- `extraction-plan.md` row 2.3 — when the legacy path is removed,
  this doc's "when divergence is acceptable" section is the migration
  guide.
- `extraction-plan.md` row 3.9 — bidirectional verification across
  the publication boundary uses the same contract, scaled up.
- `CLAUDE.md` Cardinal Rule #2 — the meta-rule this contract
  operationalizes.
