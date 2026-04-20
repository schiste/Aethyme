---
name: aethyme
description: Use Aethyme's current repository analyzers and code graph for
  navigation, caller tracing, derived facts, dead-code analysis, and task
  context.
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

Use the analyzer first. `eval-json` is the lowest-friction path: it returns the
task-ready answer list plus observability when requested.

```bash
(cd "$AETHYME_ROOT" && "$AETHYME_PY" -m src.cli analyze dead-code --repo "$REPO" --scope "<directory>" --boundary outside-directory --format eval-json --show-observability)
```

Read `unused_functions` first. Each item includes `function_name`, `defined_in`,
`status`, `external_callers`, `internal_callers`, `evidence`, `confidence`, and
`reason`. Read `observability` to verify the command, repository path, index
freshness, graph/fact counts, confidence summary, output size, and degraded
reasons.

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

- **Starting a task:** run `graph overview`, then `task scope` if the prompt has a concrete task.
- **Finding impact:** run `graph callers` or `graph parents` before broad text search.
- **Dead code / API surface:** run `analyze dead-code`; use `facts function-usage` to verify ambiguous candidates.
- **Need a compact task pack:** run `task context` or `task pack` before reading many files.

## When NOT to Use

- Don't use Aethyme when a simple `grep` or `find` suffices
- Don't call multiple commands when one answers your question
- If a graph/facts command returns decisive evidence, don't repeat the same search manually
- Don't use eval baselines, prior eval reports, or generated reference artifacts as evidence for benchmark answers
