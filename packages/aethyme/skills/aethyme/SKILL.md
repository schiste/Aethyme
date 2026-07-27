---
name: aethyme
description: Use Aethyme's high-level Explore intents, current repository
  analyzers, and code graph for navigation, caller tracing, derived facts,
  dead-code analysis, and task context.
---

# Aethyme Navigation

Use this skill when the task needs repository navigation, task localization,
caller/callee tracing, API/dead-code analysis, graph context, or a compact task
pack. Do not load every reference by default; start with the short contract
below and load only the relevant reference if the first result is insufficient.

## Setup

```bash
AETHYME_ROOT="{{AETHYME_ROOT}}"
AETHYME_BIN="$AETHYME_ROOT/rust/target/release/aethyme"
AETHYME_PY="$AETHYME_ROOT/.venv/bin/python"
REPO="$PWD"
```

Important: `python -m src.cli explore` is not a valid command. `explore` runs
only through the native binary.

## Default Contract

1. Make one bounded Explore call before broad manual search:

   ```bash
   "$AETHYME_BIN" explore --repo "$REPO" --request "<user request>" --format answer-json --show-observability --depth 0
   ```

2. Inspect these fields before trusting the result:
   `safe_to_use_as_answer`, `trust_policy`, `observability`,
   `degraded_reasons`, `answer[]`, `navigation_hints[]`, and
   `verification_steps[]`.

3. If `safe_to_use_as_answer=true` and the evidence names concrete files or
   symbols, verify narrowly by reading those files or grepping those symbols.
   Do not repeat a broad `rg --files` or repository-wide grep just because the
   tool returned something.

4. If `safe_to_use_as_answer=false`, treat `answer[]` and
   `navigation_hints[]` as an investigation plan. Follow
   `verification_steps[]`; widen search only after the hints fail.

5. Escalate deliberately. Prefer one deeper Explore call over several unrelated
   commands. Use `--depth 1/2/3` only when the previous result did not provide
   enough evidence to act.

## Load References Only When Needed

- `references/explore.md`: depth ladder, intent choice, trust/observability,
  and bounded retry rules.
- `references/graph-task.md`: graph views, callers/callees, task scope,
  context-pack, and prompt-pack commands.
- `references/dead-code.md`: usage-boundary, dead-code, public API, facts, and
  ambiguity handling.

## Non-Explore Commands

Use the Python CLI for non-Explore surfaces such as `graph`, `task`, `intents`,
`facts`, and `analyze`. The exact commands live in the references above.

## When Not To Use Aethyme

- A simple file read, exact path lookup, or tiny grep already answers the task.
- You already have one decisive Aethyme result and only need narrow source
  verification.
- The task asks for eval baselines, prior reports, or generated reference
  artifacts as evidence; those must not be used for benchmark answers.
