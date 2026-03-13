---
name: aethyme
description: Use Aethyme's code graph for any repository task — understanding
  code, finding files, tracing dependencies, or preparing changes. Returns
  graph-informed file selection and actual source code in a single call.
---

# Aethyme Navigation

## Setup

```
AETHYME_ROOT="{{AETHYME_ROOT}}"
AETHYME_PYTHON="$AETHYME_ROOT/.venv/bin/python"
```

Every command: `cd "$AETHYME_ROOT" && $AETHYME_PYTHON -m src.cli <subcommand> ...`

## Start Here — Always

For any task involving this repository, begin with ONE call:

```bash
cd "$AETHYME_ROOT" && $AETHYME_PYTHON -m src.cli task context \
  --repo "<repo-path>" --task "<your task description>" --json-output
```

This returns in a single response:
- **file_contents** — actual source code of graph-selected key files
- **anchors** — the 3-5 most relevant entry points for your task
- **in_scope / out_of_scope** — which files and areas matter
- **navigation_order** — suggested exploration sequence
- **dependencies / impact** — structural relationships
- **risk_flags** — areas requiring careful attention

## After the Context Call

- If the file contents answer your question, **answer directly**.
  Do not make additional Aethyme calls just because they exist.
- If you need a file not included, read it with normal shell tools.
- If you need to trace a specific call chain:
  `graph callers <repo> <target> --json-output`
  `graph callees <repo> <target> --json-output`
- If you need to expand a node's neighborhood:
  `graph expand <repo> <node-id> --json-output`
- If you need dependency or impact frontier:
  `query deps <repo> <target> --json-output`
  `query impact <repo> <target> --json-output`

## What NOT to Do

- Do not call `task anchors`, `task scope`, `task pack` separately —
  `task context` includes all of them plus file content.
- Do not call `repo inspect` — context already includes overview data.
- Do not make multiple Aethyme calls when one `task context` suffices.
- Do not prefer graph metadata over reading the actual file contents
  returned in the context pack.
