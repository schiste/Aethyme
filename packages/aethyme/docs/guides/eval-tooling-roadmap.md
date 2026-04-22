# Eval Tooling Roadmap

Last Updated: 2026-04-22

This roadmap defines the next repository-agnostic improvements for Aethyme's evaluation tooling.

The goal is not to optimize `task-conditioned` prompts first. The goal is to make the generic product stronger so that:

1. `explore` beats control more consistently
2. `leverage` beats `explore` with minimal extra cost
3. `task-conditioned` can later build on a strong generic base instead of hiding weak generic tooling

## Product Goal

The product should help an agent do three generic things better on any repository:

1. build the right candidate set
2. verify candidates precisely
3. deliver a valid final answer with less cost than unaided exploration

The product should not depend on benchmark-shaped prompt engineering.

## Design Principles

### 1. Optimize the generic layer first

Prioritize improvements that benefit:

- debugging
- dead-code analysis
- impact analysis
- feature localization
- architecture explanation

Do not start by tuning `task-conditioned` prompt packs.

### 2. Favor smaller, sharper guidance

Large context injections often increase cost and can anchor the agent too early.

Prefer:

- ranked hints
- bounded candidate lists
- targeted verification tools

Over:

- long prompt rewrites
- broad task-specific context dumps

### 3. Separate enumeration from verification

The strongest runs consistently follow this shape:

1. enumerate the search space
2. resolve ambiguities
3. synthesize the final answer

The tooling should make those phases explicit.

### 4. Treat output discipline as product behavior

A technically correct investigation that fails the output contract is still a failed run.

Schema validation, output shaping, and final contract checks are part of the product surface.

## Priority Roadmap

### P1. Candidate Enumeration Layer

Build a generic capability that helps an agent construct a bounded, relevant search space before deep analysis.

Current implementation status:

- `explore --request ...` now defaults to the general-purpose
  `task_localization_query` intent.
- `task_localization_query` composes one bounded `task-localize` graph call,
  bounded symbol search, source-text ranking, source call-site expansion,
  filename fallback, and compact `task-expand` output into
  graph/symbol/source-backed `answer[]`, low-confidence `navigation_hints[]`,
  next actions, confidence, trust policy, and observability.
- On large repos, `task_localization_query` returns degraded partial output
  instead of blocking. If `task-localize` times out, the default path still
  attempts bounded source-backed recovery unless explicitly disabled.
- Source-backed recovery ranks localized symbol clusters before whole files,
  prefers production call sites over tests, and performs a bounded second-hop
  expansion so workflow bridges can be surfaced without an LLM inferring every
  step manually.
- `behavior_localization_query` is available for debugging and feature/workflow
  localization. It uses the same answer schema with a larger source-text and
  call-site budget.
- Filename-only fallback is navigation-only. It must not appear as an
  authoritative answer, and `safe_to_use_as_answer` must stay false when no
  graph/symbol-backed candidates are available.
- `explore --intent usage_boundary_query` remains the first specialized
  high-level Explore intent.
- For PHP scopes, it uses a scope-first `analyze-usage-boundary` engine path that enumerates scoped public functions/methods without building the full repository graph.
- The output is already answer-shaped when trusted: `answer[]`, `excluded[]`,
  `ambiguous[]`, confidence summaries, trust policy, and observability. When
  degraded, `navigation_hints[]` should preserve normal agent exploration
  rather than biasing the final answer.
- Non-PHP usage-boundary scopes still need graph-backed fallback or future language-specific scope-first scanners.

Expected capabilities:

- enumerate public/exported symbols in a scope
- enumerate likely fix-surface files for a behavior
- enumerate external callers/callees for a symbol
- enumerate files participating in a workflow or dependency path

Expected output shape:

- candidate id
- candidate kind
- evidence summary
- confidence

Why this is first:

- recent winning runs succeeded because they enumerated the full candidate set before narrowing
- recent losing runs narrowed too early

Validation target:

- `explore` should improve first
- false negatives should drop before token usage rises

### P2. Verification Primitives

Once candidates exist, the agent needs generic ways to prove or disprove them.

Current implementation status:

- `usage_boundary_query` performs a first-pass outside-boundary caller check, internal-caller check, docs/config-only reference check, and name-collision ambiguity check.
- The analyzer accepts `budget_ms` and `max_evidence_per_symbol` so evaluation runs can trade completeness for speed explicitly.
- Follow-up verification still relies on graph-backed `facts function-usage` for ambiguous/non-PHP cases.

Expected capabilities:

- external-usage checks
- importer checks
- dependency path checks
- state write/read path checks
- export-but-unused checks

Why this is second:

- the best runs resolved ambiguous helpers, wrappers, and service exports explicitly
- weaker runs relied on quick grep evidence and stopped too early

Validation target:

- better precision on ambiguous candidates
- fewer ad hoc grep loops

### P3. Ambiguity Surfacing

The tooling should mark uncertain cases instead of forcing the agent to infer every ambiguity manually.

Important ambiguity classes:

- public but internal helpers
- interface or contract methods
- compatibility shims
- deprecated but retained APIs
- wrapper functions that only delegate

Expected output shape:

- ambiguity type
- why it is ambiguous
- suggested next verification step

Why this matters:

- many benchmark errors are not from missing symbols
- they are from misclassifying ambiguous symbols

Validation target:

- improved precision without requiring larger prompts

### P4. Output Discipline and Contract Support

Add generic help for valid final output generation.

Expected capabilities:

- output schema preview
- final schema validation
- repair or retry loop for invalid output
- explicit degradation when the contract is not met

Why this matters:

- invalid JSON should never erase an otherwise good investigation
- output correctness is part of evaluation reliability

Validation target:

- contract failures approach zero across structured evals

### P5. Compact Leverage Guidance

Only after P1-P4 are stronger, refine `leverage`.

`leverage` should remain generic. It should not become task-conditioned.

Preferred shape:

- a short Aethyme usage card
- the generic `explore --request ... --format answer-json --show-observability` command as the first call
- instructions to choose a specialized intent only when the user request clearly matches one
- instructions to read `answer[]`, `excluded[]`, `ambiguous[]`, and `observability`

Avoid:

- long engine-generated prompt packs
- benchmark-shaped hints
- prefilled candidate names from the reference answer

Validation target:

- `leverage` should outperform `explore` with only a small cost increase
- reports should show whether `src.cli explore` / `src.cli intents` were actually called

Current implementation status:

- The leverage card can point to `.codex/skills/aethyme/aethyme-explore`, a
  narrow wrapper installed in the Aethyme playground. This keeps prompts
  generic and avoids disclosing the Aethyme source root.
- Aethyme usage extraction recognizes the wrapper as an Explore call.
- Single-run and batch eval paths share the same cold-probe leakage stamping
  helper, so leakage fields are available for later analysis when the probe is
  enabled.

## What Not To Optimize First

Do not prioritize these before the generic layer is stronger:

- task-specific prompt packs
- eval-specific heuristics
- scenario-specific engine behavior
- longer prompt context dumps

Those changes make the eval harder to trust and reduce the product's generality.

## Success Criteria

The roadmap is working when repeated runs show:

1. `explore` more consistently beats `control-cto-off` on quality
2. `leverage` more consistently beats `explore` on the recalculated eval score
3. quality gains come from better search behavior, not larger prompt size
4. token and time costs grow more slowly than quality
5. contract failures become rare

## Measurement Plan

Track these on every run:

- `quality_score`
- `recalculated_eval_score`
- `tool_call_count`
- `top_tools`
- `total_tokens`
- `duration_seconds`
- `score_per_1k_tokens`
- `score_per_minute`

Interpretation:

- `quality_score` answers task success
- `recalculated_eval_score` answers whether a condition justified itself against `control-cto-off`
- `score_per_1k_tokens` and `score_per_minute` answer whether quality gains are economically useful

## Recommended Next Implementation Order

1. add a generic candidate-enumeration layer
2. add generic verification primitives
3. add ambiguity markers and next-step suggestions
4. add output-contract helpers
5. only then refine `leverage`
6. only after that revisit `task-conditioned`
