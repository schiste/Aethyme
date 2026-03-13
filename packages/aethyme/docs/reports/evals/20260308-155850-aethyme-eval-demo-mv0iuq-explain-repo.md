# Eval Report: Explain this repo

Last Updated: 2026-03-08

- Repository: `/tmp/aethyme-eval-demo-Mv0IUq`
- Generated: `2026-03-08T15:58:50.735842+00:00`

## Summary

- Baseline prompt chars: `140`
- Aethyme prompt chars: `111`
- Navigation items: `3`
- Risk items: `0`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 75,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 0/1",
      "source files with area assignment: 1/1",
      "generic source file names: 0"
    ]
  },
  "entrypoint_clarity": {
    "score": 30,
    "level": "weak",
    "evidence": [
      "direct code entrypoint edges: 0",
      "configs with entrypoints: 0",
      "areas with ambiguous entrypoints: 0"
    ]
  },
  "config_hygiene": {
    "score": 50,
    "level": "weak",
    "evidence": [
      "operational configs: 0",
      "linked configs: 0/0",
      "duplicate config families: 0"
    ]
  },
  "hidden_coupling": {
    "score": 65,
    "level": "mixed",
    "evidence": [
      "low-confidence semantic edges: 0/0",
      "high-confidence semantic edges: 0/0",
      "cross-area semantic edges: 0/0"
    ]
  },
  "parser_visibility": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "supported source files: 1/1",
      "source files with semantic extraction: 1/1",
      "total extracted functions/classes: 1"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: /tmp/aethyme-eval-demo-Mv0IUq
Explore the repository directly and produce a structured explanation.
```

### Run Metrics

- command: `/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py`
- exit code: `1`
- input tokens: `None`
- output tokens: `None`
- retries: `1`
- review burden: `None`
- wall time: `1.199s`

### Final Output Message

```text
{"input_tokens": null, "output_tokens": null, "retries": 1, "review_burden": null, "final_output_message": null, "structured_output": null}
```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "label": "baseline",
  "command": "/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py",
  "exit_code": 1,
  "duration_seconds": 1.1987637920537964,
  "stdout": "{\"input_tokens\": null, \"output_tokens\": null, \"retries\": 1, \"review_burden\": null, \"final_output_message\": null, \"structured_output\": null}",
  "stderr": "2026-03-08T15:58:48.442064Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/database (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442084Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ci-deploy (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442094Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/auth (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442103Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ai-agents (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442105Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/_meta (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442113Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/observability (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442116Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/integrations (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442119Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/testing (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442121Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/architecture (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442124Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-quality (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442126Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/agent-workflow (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442128Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/docs-tooling (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442140Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/api (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442147Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ops (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442155Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/performance (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:48.442158Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-core (symlink): No such file or directory (os error 2)\nWarning: no last agent message; wrote empty content to /var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/aethyme-codex-eval-192sd83r/last-message.json",
  "input_tokens": null,
  "output_tokens": null,
  "retries": 1,
  "review_burden": null,
  "final_output_message": "{\"input_tokens\": null, \"output_tokens\": null, \"retries\": 1, \"review_burden\": null, \"final_output_message\": null, \"structured_output\": null}",
  "structured_output": null
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 1.0,
    "entrypoints": 1.0,
    "important_docs": 0.0,
    "key_configs": 1.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 45.0,
  "max_score": 100
}
```

## Aethyme

### Prompt

```text
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

### Run Metrics

- command: `/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py`
- exit code: `1`
- input tokens: `None`
- output tokens: `None`
- retries: `1`
- review burden: `None`
- wall time: `1.185s`

### Final Output Message

```text
{"input_tokens": null, "output_tokens": null, "retries": 1, "review_burden": null, "final_output_message": null, "structured_output": null}
```

### Structured Output

```json
null
```

### Raw Run Record

```json
{
  "label": "aethyme",
  "command": "/opt/homebrew/Cellar/python@3.14/3.14.3_1/Frameworks/Python.framework/Versions/3.14/bin/python3.14 /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/scripts/eval/run_codex_eval.py",
  "exit_code": 1,
  "duration_seconds": 1.1852993329521269,
  "stdout": "{\"input_tokens\": null, \"output_tokens\": null, \"retries\": 1, \"review_burden\": null, \"final_output_message\": null, \"structured_output\": null}",
  "stderr": "2026-03-08T15:58:49.627054Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/database (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627070Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ci-deploy (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627080Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/auth (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627090Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ai-agents (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627092Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/_meta (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627101Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/observability (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627104Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/integrations (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627107Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/testing (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627109Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/architecture (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627112Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-quality (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627114Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/agent-workflow (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627117Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/docs-tooling (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627129Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/api (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627136Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/ops (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627153Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/performance (symlink): No such file or directory (os error 2)\n2026-03-08T15:58:49.627155Z ERROR codex_core::skills::loader: failed to stat skills entry /Users/christophehenner/.codex/skills/frontend-core (symlink): No such file or directory (os error 2)\nWarning: no last agent message; wrote empty content to /var/folders/f2/krjzd4c15nn491pm37zrkp1h0000gn/T/aethyme-codex-eval-6edsk3af/last-message.json",
  "input_tokens": null,
  "output_tokens": null,
  "retries": 1,
  "review_burden": null,
  "final_output_message": "{\"input_tokens\": null, \"output_tokens\": null, \"retries\": 1, \"review_burden\": null, \"final_output_message\": null, \"structured_output\": null}",
  "structured_output": null
}
```

### Assessment

```json
{
  "scores": {
    "code_areas": 0.0,
    "reference_areas": 1.0,
    "entrypoints": 1.0,
    "important_docs": 0.0,
    "key_configs": 1.0,
    "key_languages": 0.0,
    "high_risk_areas": 1.0,
    "navigation_order": 0.0,
    "representative_code_files": 0.0,
    "representative_docs": 0.0
  },
  "weighted_score": 45.0,
  "max_score": 100
}
```

## Comparison

- Prompt chars delta: `-29`
- Navigation items surfaced: `3`
- Risk items surfaced: `0`

## Reference

### Output Schema

```json
{
  "type": "object",
  "required": [
    "repo_summary",
    "code_areas",
    "reference_areas",
    "entrypoints",
    "important_docs",
    "key_configs",
    "key_languages",
    "high_risk_areas",
    "navigation_order",
    "representative_code_files",
    "representative_docs"
  ],
  "properties": {
    "repo_summary": {
      "type": "string"
    },
    "code_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "reference_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "entrypoints": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "important_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_configs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "key_languages": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "high_risk_areas": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "navigation_order": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_code_files": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "representative_docs": {
      "type": "array",
      "items": {
        "type": "string"
      }
    },
    "evidence": {
      "type": "array",
      "items": {
        "type": "string"
      }
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "code_areas": 20,
    "reference_areas": 10,
    "entrypoints": 20,
    "important_docs": 15,
    "key_configs": 10,
    "key_languages": 10,
    "high_risk_areas": 5,
    "navigation_order": 5,
    "representative_code_files": 3,
    "representative_docs": 2
  },
  "notes": [
    "Prefer exact path and area matches.",
    "Navigation order is partial-credit and ordered.",
    "Repo summary is informative but not currently machine-scored."
  ]
}
```

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "src"
  ],
  "reference_areas": [],
  "entrypoints": [],
  "important_docs": [
    "README.md"
  ],
  "key_configs": [],
  "key_languages": [
    "python"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "README.md",
    "src"
  ],
  "representative_code_files": [
    "src/main.py"
  ],
  "representative_docs": [
    "README.md"
  ],
  "evidence": [
    "src/main.py",
    "README.md"
  ]
}
```

## Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/tmp/aethyme-eval-demo-Mv0IUq",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "README.md",
        "file": "README.md",
        "reason": "repository readme"
      },
      {
        "kind": "folder",
        "id": "src",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "src/main.py",
        "file": "src/main.py",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "README.md",
      "src",
      "src/main.py"
    ],
    "in_scope_files": [
      "src/main.py"
    ],
    "in_scope_symbols": [
      "src/main.py::main"
    ],
    "in_scope_areas": [
      "src"
    ],
    "out_of_scope": [],
    "risks": []
  },
  "commands": [
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect /tmp/aethyme-eval-demo-Mv0IUq --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview /tmp/aethyme-eval-demo-Mv0IUq --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand /tmp/aethyme-eval-demo-Mv0IUq <anchor-id> --json-output"
  ]
}
```

## Aethyme Pack

```json
{
  "task": {
    "raw": "Explain this repo",
    "normalized": "explain this repo",
    "kind": "explain_repo"
  },
  "overview": {
    "overview_docs": [
      "README.md"
    ],
    "code_areas": [
      "src"
    ],
    "reference_areas": [],
    "subareas": [],
    "entrypoints": [],
    "key_configs": [],
    "representative_code_files": [
      "src/main.py"
    ],
    "representative_docs": [
      "README.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "README.md",
      "file": "README.md",
      "reason": "repository readme"
    },
    {
      "kind": "folder",
      "id": "src",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "src/main.py",
      "file": "src/main.py",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "src/main.py",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [
      {
        "value": "src/main.py::main",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      }
    ],
    "areas": [
      {
        "value": "src",
        "kind": "area",
        "reason": "primary top-level area"
      }
    ]
  },
  "out_of_scope": {
    "files": [],
    "symbols": [],
    "areas": []
  },
  "dependencies": [
    {
      "from": "dir:aethyme-eval-demo-Mv0IUq:src",
      "to": "src/main.py",
      "kind": "contains"
    },
    {
      "from": "src",
      "to": "dir:aethyme-eval-demo-Mv0IUq:src",
      "kind": "contains"
    },
    {
      "from": "src/main.py",
      "to": "src/main.py::main",
      "kind": "defines"
    }
  ],
  "impact": [],
  "snippets": [
    {
      "file": "README.md",
      "start_line": 1,
      "end_line": 1,
      "kind": "overview"
    },
    {
      "file": "src/main.py",
      "start_line": 1,
      "end_line": 2,
      "kind": "overview"
    }
  ],
  "risk_flags": [],
  "navigation_order": [
    "README.md",
    "src",
    "src/main.py"
  ],
  "budget": {
    "max_anchors": 5,
    "max_files": 8,
    "max_snippets": 8,
    "dependency_depth": 1,
    "impact_depth": 1
  },
  "confidence": {
    "anchor_confidence": 0.85,
    "scope_confidence": 0.8
  }
}
```

## Explanation

```text
Task: Explain this repo
Languages: python
Top-level directories: src
Files indexed: 2
Functions indexed: 1
Classes indexed: 0
Docs indexed: 1
Configs indexed: 0
README: README.md

Code areas:
- src

Representative code:
- src/main.py

Representative docs:
- README.md

Navigation order:
- README.md
- src
- src/main.py
```
