# Eval Report: Explain this repo

Last Updated: 2026-03-09

- Repository: `.`
- Generated: `2026-03-09T07:35:10.846328+00:00`

## Summary

- Control prompt chars: `112`
- Explore prompt chars: `668`
- Leverage prompt chars: `111`
- Navigation items: `5`
- Risk items: `22`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 67,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 895/5267",
      "source files with area assignment: 150/151",
      "generic source file names: 0"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 3",
      "configs with entrypoints: 3",
      "areas with ambiguous entrypoints: 1"
    ]
  },
  "config_hygiene": {
    "score": 27,
    "level": "weak",
    "evidence": [
      "operational configs: 30",
      "linked configs: 30/30",
      "duplicate config families: 23"
    ]
  },
  "hidden_coupling": {
    "score": 41,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 2458/4058",
      "high-confidence semantic edges: 1237/4058",
      "cross-area semantic edges: 441/4058"
    ]
  },
  "parser_visibility": {
    "score": 91,
    "level": "strong",
    "evidence": [
      "supported source files: 150/151",
      "source files with semantic extraction: 112/151",
      "total extracted functions/classes: 1012"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: .
Explore the repository directly and produce a structured explanation.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Control runner not executed.
```

### Structured Output

```json
null
```

## Explore

### Prompt

```text
Task: Explain this repo
Repository path: .
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect . --json-output
  /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview . --json-output
  /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand . <anchor-id> --json-output

Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Explore runner not executed.
```

### Structured Output

```json
null
```

## Leverage

### Prompt

```text
Task: Explain this repo
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

### Run Metrics

- input tokens: `null`
- output tokens: `null`
- retries: `null`
- wall time: `null`

### Final Output Message

```text
Leverage runner not executed.
```

### Structured Output

```json
null
```

## Comparison

| Metric | Control | Explore | Leverage |
| --- | --- | --- | --- |
| Prompt chars | `112` | `668` | `111` |

- Navigation items surfaced: `5`
- Risk items surfaced: `22`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
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
    "representative_docs",
    "evidence"
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
    "rust",
    "sdk",
    "src"
  ],
  "reference_areas": [
    "docs",
    "k8s"
  ],
  "entrypoints": [
    ".github/actions/aethyme-scorecard/index.js",
    "rust/crates/aethyme-engine/src/lib.rs"
  ],
  "important_docs": [
    "README.md",
    "docs/README.md",
    "docs/architecture/auth-boundary.md"
  ],
  "key_configs": [
    "rust/crates/aethyme-engine/Cargo.toml"
  ],
  "key_languages": [
    "javascript",
    "python",
    "rust"
  ],
  "high_risk_areas": [
    "docs/architecture/auth-boundary.md",
    "docs/auth-setup.md",
    "k8s/helm/aethyme/templates/cache/redis-deployment.yaml"
  ],
  "navigation_order": [
    "README.md",
    "docs/README.md",
    "rust",
    "sdk",
    "docs"
  ],
  "representative_code_files": [
    "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
    "scripts/eval/run_claude_eval.py",
    "scripts/eval/run_codex_eval.py"
  ],
  "representative_docs": [
    "README.md",
    "docs/README.md",
    "docs/architecture/auth-boundary.md"
  ],
  "evidence": [
    "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
    "scripts/eval/run_claude_eval.py",
    "README.md"
  ]
}
```

## Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": ".",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "rust/README.md",
        "file": "rust/README.md",
        "reason": "repository readme"
      },
      {
        "kind": "file",
        "id": "docs/architecture/auth-boundary.md",
        "file": "docs/architecture/auth-boundary.md",
        "reason": "architecture document"
      },
      {
        "kind": "folder",
        "id": "src",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "folder",
        "id": "rust",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
        "file": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "rust/README.md",
      "docs/architecture/auth-boundary.md",
      "src",
      "rust",
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs"
    ],
    "in_scope_files": [
      "rust/README.md",
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs"
    ],
    "in_scope_symbols": [
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::main",
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::print_explanation",
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::read_option",
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::run"
    ],
    "in_scope_areas": [
      "rust",
      "src"
    ],
    "out_of_scope": [],
    "risks": [
      "docs/architecture/auth-boundary.md",
      "docs/auth-setup.md",
      "k8s/helm/aethyme/templates/cache/redis-deployment.yaml",
      "k8s/helm/aethyme/templates/deployment.yaml",
      "k8s/helm/aethyme/templates/secret.yaml",
      "k8s/helm/aethyme/templates/workers/indexer-deployment.yaml",
      "migrations/001_initial_schema.sql",
      "migrations/002_add_rls_hardening.sql",
      "migrations/002_query_optimization_indexes.sql",
      "migrations/003_scorecard_tables.sql",
      "migrations/004_add_display_id.sql",
      "scripts/deploy/blue_green_deploy.sh",
      "scripts/deploy/canary_deploy.sh",
      "sdk/python/aethyme_sdk/auth.py",
      "src/auth/__init__.py",
      "src/auth/api_keys.py",
      "src/auth/middleware.py",
      "src/auth/oidc.py",
      "tests/auth/__init__.py",
      "tests/auth/test_isolation.py",
      "tests/auth/test_rls.py",
      "tests/support/auth_db.py"
    ]
  },
  "commands": [
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect . --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview . --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand . <anchor-id> --json-output"
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
  "summary": {
    "snapshot": {
      "languages": [
        "javascript",
        "python",
        "rust"
      ],
      "top_level_dirs": [
        ".github",
        "docs",
        "k8s",
        "migrations",
        "monitoring",
        "ops",
        "rust",
        "scripts",
        "sdk",
        "src",
        "tests"
      ],
      "readme_path": "rust/README.md"
    },
    "files_count": 326,
    "functions_count": 818,
    "classes_count": 194,
    "docs_count": 97,
    "configs_count": 36
  },
  "signals": {
    "boundary_clarity": {
      "score": 67,
      "level": "mixed",
      "evidence": [
        "cross-area semantic edges: 895/5267",
        "source files with area assignment: 150/151",
        "generic source file names: 0"
      ]
    },
    "entrypoint_clarity": {
      "score": 100,
      "level": "strong",
      "evidence": [
        "direct code entrypoint edges: 3",
        "configs with entrypoints: 3",
        "areas with ambiguous entrypoints: 1"
      ]
    },
    "config_hygiene": {
      "score": 27,
      "level": "weak",
      "evidence": [
        "operational configs: 30",
        "linked configs: 30/30",
        "duplicate config families: 23"
      ]
    },
    "hidden_coupling": {
      "score": 41,
      "level": "weak",
      "evidence": [
        "low-confidence semantic edges: 2458/4058",
        "high-confidence semantic edges: 1237/4058",
        "cross-area semantic edges: 441/4058"
      ]
    },
    "parser_visibility": {
      "score": 91,
      "level": "strong",
      "evidence": [
        "supported source files: 150/151",
        "source files with semantic extraction: 112/151",
        "total extracted functions/classes: 1012"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      "README.md",
      "docs/README.md",
      "docs/architecture/auth-boundary.md"
    ],
    "code_areas": [
      "rust",
      "sdk",
      "src"
    ],
    "reference_areas": [
      "docs",
      "k8s"
    ],
    "subareas": [
      "rust/crates",
      "src/api",
      "src/autofixers",
      "src/scorecard"
    ],
    "entrypoints": [
      ".github/actions/aethyme-scorecard/index.js",
      "rust/crates/aethyme-engine/src/lib.rs"
    ],
    "key_configs": [
      "rust/crates/aethyme-engine/Cargo.toml"
    ],
    "representative_code_files": [
      "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
      "scripts/eval/run_claude_eval.py",
      "scripts/eval/run_codex_eval.py",
      "src/cli.py",
      "src/indexer/export_graph.py"
    ],
    "representative_docs": [
      "README.md",
      "docs/README.md",
      "docs/architecture/auth-boundary.md",
      "docs/architecture/core-architecture.md",
      "docs/architecture/graphability-and-navigability-signals.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "rust/README.md",
      "file": "rust/README.md",
      "reason": "repository readme"
    },
    {
      "kind": "file",
      "id": "docs/architecture/auth-boundary.md",
      "file": "docs/architecture/auth-boundary.md",
      "reason": "architecture document"
    },
    {
      "kind": "folder",
      "id": "src",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "folder",
      "id": "rust",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
      "file": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "rust/README.md",
        "kind": "file",
        "reason": "anchor-adjacent file"
      },
      {
        "value": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [
      {
        "value": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::main",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::print_explanation",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::read_option",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs::run",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      }
    ],
    "areas": [
      {
        "value": "rust",
        "kind": "area",
        "reason": "primary top-level area"
      },
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
      "from": "dir:aethyme:rust",
      "to": "dir:aethyme:rust/crates",
      "kind": "contains"
    },
    {
      "from": "dir:aethyme:rust",
      "to": "rust/Cargo.lock",
      "kind": "contains"
    },
    {
      "from": "dir:aethyme:rust",
      "to": "rust/Cargo.toml",
      "kind": "contains"
    },
    {
      "from": "dir:aethyme:rust",
      "to": "rust/README.md",
      "kind": "contains"
    },
    {
      "from": "dir:aethyme:rust/crates/aethyme-engine/src",
      "to": "dir:aethyme:rust/crates/aethyme-engine/src/bin",
      "kind": "contains"
    },
    {
      "from": "dir:aethyme:rust/crates/aethyme-engine/src",
      "to": "dir:aethyme:rust/crates/aethyme-engine/src/indexer",
      "kind": "contains"
    }
  ],
  "impact": [
    {
      "symbol": ".github/actions/aethyme-scorecard/index.js",
      "file": ".github/actions/aethyme-scorecard/index.js",
      "reason": "entrypoint candidate"
    },
    {
      "symbol": "rust/crates/aethyme-engine/src/lib.rs",
      "file": "rust/crates/aethyme-engine/src/lib.rs",
      "reason": "entrypoint candidate"
    }
  ],
  "snippets": [
    {
      "file": "docs/architecture/auth-boundary.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "rust/README.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    }
  ],
  "risk_flags": [
    {
      "scope": "docs/architecture/auth-boundary.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "docs/auth-setup.md",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "k8s/helm/aethyme/templates/cache/redis-deployment.yaml",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "k8s/helm/aethyme/templates/deployment.yaml",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "k8s/helm/aethyme/templates/secret.yaml",
      "area": "secrets",
      "level": "high",
      "reason": "sensitive credential surface"
    },
    {
      "scope": "k8s/helm/aethyme/templates/workers/indexer-deployment.yaml",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "migrations/001_initial_schema.sql",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migrations/002_add_rls_hardening.sql",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migrations/002_query_optimization_indexes.sql",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migrations/003_scorecard_tables.sql",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "migrations/004_add_display_id.sql",
      "area": "migrations",
      "level": "high",
      "reason": "schema change area"
    },
    {
      "scope": "scripts/deploy/blue_green_deploy.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "scripts/deploy/canary_deploy.sh",
      "area": "infra",
      "level": "high",
      "reason": "infrastructure surface"
    },
    {
      "scope": "sdk/python/aethyme_sdk/auth.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "src/auth/__init__.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "src/auth/api_keys.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "src/auth/middleware.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "src/auth/oidc.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "tests/auth/__init__.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "tests/auth/test_isolation.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "tests/auth/test_rls.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    },
    {
      "scope": "tests/support/auth_db.py",
      "area": "auth",
      "level": "high",
      "reason": "authentication boundary"
    }
  ],
  "navigation_order": [
    "rust/README.md",
    "docs/architecture/auth-boundary.md",
    "src",
    "rust",
    "rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs"
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
Languages: javascript, python, rust
Top-level directories: .github, docs, k8s, migrations, monitoring, ops, rust, scripts, sdk, src, tests
Files indexed: 326
Functions indexed: 818
Classes indexed: 194
Docs indexed: 97
Configs indexed: 36
README: README.md

Code areas:
- rust
- sdk
- src

Reference areas:
- docs
- k8s

Key subareas:
- rust/crates
- src/api
- src/autofixers
- src/scorecard

Key configs:
- rust/crates/aethyme-engine/Cargo.toml

Entrypoints:
- .github/actions/aethyme-scorecard/index.js
- rust/crates/aethyme-engine/src/lib.rs

Representative code:
- rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs
- scripts/eval/run_claude_eval.py
- scripts/eval/run_codex_eval.py

Representative docs:
- README.md
- docs/README.md
- docs/architecture/auth-boundary.md

Navigation order:
- rust/README.md
- docs/architecture/auth-boundary.md
- src
- rust
- rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs
```
