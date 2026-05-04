---
name: aethyme
description: Use Aethyme's high-level Explore intents, current repository
  analyzers, and code graph for navigation, caller tracing, derived facts,
  dead-code analysis, and task context.
---

# Aethyme Navigation

## Setup

Run Aethyme from the tool package root, but keep the target repository as
`$REPO`. The deployed skill replaces `{{AETHYME_ROOT}}` with the local Aethyme
package path.

```
AETHYME_ROOT="{{AETHYME_ROOT}}"
AETHYME_PY="$AETHYME_ROOT/.venv/bin/python"
REPO="$PWD"
```

Use this command shape:

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli <command> ...)
```

## Fast Workflows

### Start Here: Explore

Prefer the high-level Explore surface before low-level graph navigation.
Without an explicit intent, Aethyme runs the general-purpose
`task_localization_query` intent and returns ranked candidate files, symbols,
areas, compact evidence, confidence, verification steps, trust policy, and
observability for the user request. This default path uses one bounded
`task-localize` graph call,
bounded symbol search, source-text evidence, source call-site expansion,
filename fallback, and compact expansions. Filename-only fallback is never
authoritative: it appears in `navigation_hints[]`, not `answer[]`, and is
marked `navigation_only` with low confidence. The default output detail is
`compact`; use `--detail standard` or `--detail full` only when additional
evidence/debug payload is worth the extra tokens. If graph localization exceeds
the responsiveness budget on a large repo, Aethyme can still return degraded
source-backed `answer[]` candidates when local text/call-site evidence is
strong enough, but the trust policy becomes `needs_verification` and
`safe_to_use_as_answer=false`.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli explore --repo "$REPO" --request "<user request>" --format answer-json --show-observability)
```

Use a specialized intent only when the request clearly matches it. For
behavior localization, debugging, or "which files implement this workflow"
questions, use `behavior_localization_query`; it spends more budget on
source-text ranking and call-site expansion.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli explore --repo "$REPO" --intent behavior_localization_query --request "<user request>" --format answer-json --show-observability)
```

For dead-code, boundary usage, or public API caller audits, call
`usage_boundary_query` directly with structured params.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli explore --repo "$REPO" --intent usage_boundary_query --request "<user request>" --params '{"scope":"<directory>","symbol_kind":"public_top_level_function","boundary":{"type":"outside_directory","path":"<directory>"},"search_roots":[],"budget_ms":10000,"max_evidence_per_symbol":5}' --format answer-json --show-observability)
```

You can still list the intent catalog directly:

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli intents --request "<user request>" --format compact-json)
```

Read `trust_policy` and `safe_to_use_as_answer` first. Use `answer[]` as the
primary result only when `safe_to_use_as_answer` is true. If false, treat
`answer[]` and `navigation_hints[]` as a ranked investigation plan, not a final
answer; follow `verification_steps[]` before concluding. Read `excluded[]` to
understand why candidates or areas were rejected. Read `ambiguous[]` before
trusting low-confidence candidates. Read
`output_adapters.dead_code_eval_json` only when the task specifically asks for
the dead-code evaluation schema.

Always inspect `observability` before trusting the result: command, repo path,
index freshness, graph/fact counts, output size, confidence summaries, and
degraded reasons.

If `degraded_reasons` says `task-localize` timed out, read
`observability.degradation_guidance` before retrying. If
`degradation_guidance.status` is `recovered`, inspect source-backed
`answer[].evidence.line_refs` and `verification_steps[]` before broad manual
search. If `safe_to_use_as_answer` is false, follow `verification_steps[]`,
`navigation_hints[]`, and `next_actions` as an investigation plan and verify
with normal repo search before concluding. If a graph/symbol/source-backed
`answer[]` is present, inspect candidates in order, then verify manually before
finalizing.

For very large repositories where first-response speed is more important than
graph coverage, pass `--params '{"graph_query_timeout_ms":500}'`.

### Repository Orientation
```bash
# Repo-level graph overview
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph overview "$REPO" --json-output)

# Task-focused scope and starting anchors
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli task scope --repo "$REPO" --task "<task>" --json-output)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli task anchors --repo "$REPO" --task "<task>" --json-output)
```

### Graph Navigation
```bash
# Inspect a node and expand nearby graph context
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph node "$REPO" "<file-or-symbol>" --json-output)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph expand "$REPO" "<file-or-symbol>" --json-output)

# Caller/callee evidence for a function or method
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph callers "$REPO" "<function-or-method>" --json-output)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli graph callees "$REPO" "<function-or-method>" --json-output)
```

### Dead Code and API Surface

Use `explore --intent usage_boundary_query` first for boundary usage/dead-code
questions. It is the lowest-friction path because it returns the final answer,
excluded candidates, confidence, and observability in one JSON object.

For PHP scopes, this Explore intent uses the scope-first `analyze-usage-boundary`
engine path and avoids building the full repository graph. If the result has
`degraded_reasons` for language support or no public symbols in a non-PHP scope,
fall back to the graph-backed analyzer or facts commands below.

Use the direct analyzer only as a fallback when a task explicitly asks for the
legacy dead-code shape.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli analyze dead-code --repo "$REPO" --scope "<directory>" --boundary outside-directory --format eval-json --show-observability)
```

Read `unused_functions` first. Each item includes `function_name`, `defined_in`,
`status`, `external_callers`, `internal_callers`, `evidence`, `confidence`, and
`reason`. `excluded_functions` explains why candidates were rejected. Read
`observability` to verify the command, repository path, index freshness,
graph/fact counts, confidence summary, output size, and degraded reasons.

Interpret status precisely:
- `Unused`: no internal or external code callers found.
- `Ambiguous`: no external code callers found, but internal callers or
  docs/config-only references exist. It matches prompts asking for “no callers
  outside this directory” but may not be safe to remove.
- `Used`: at least one caller exists outside the boundary, so exclude it from external-boundary dead-code answers.

For harder cases, derive the public surface and inspect one function's usage
relative to the boundary.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli facts public-functions --repo "$REPO" --scope "<directory>" --json-output)
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli facts function-usage --repo "$REPO" --target "<function>" --boundary "<directory>" --json-output)
```

Use `--roots "<dir1>,<dir2>"` on `analyze dead-code` or `facts function-usage`
when the repository is large and the task gives likely search roots.

## When to Use

- **Starting a task:** run `explore --request` first; it composes anchors, scope, compact evidence, and verification steps.
- **Need a task-ready answer:** run `explore --request`; choose a specialized intent only when the request clearly matches one.
- **Finding impact:** run `graph callers` or `graph parents` before broad text search.
- **Dead code / API surface:** run `explore --intent usage_boundary_query`; use `facts function-usage` to verify ambiguous candidates.
- **Need a compact task pack:** run `task context` or `task pack` before reading many files.

## When NOT to Use

- Don't use Aethyme when a simple `grep` or `find` suffices
- Don't call multiple commands when one answers your question
- If a graph/facts command returns decisive evidence, don't repeat the same search manually
- Don't use eval baselines, prior eval reports, or generated reference artifacts as evidence for benchmark answers
