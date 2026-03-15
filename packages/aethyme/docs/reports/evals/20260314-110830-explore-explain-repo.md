# Eval Report: Explain this repo

## Meta

- Date: 2026-03-14
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore`
- Eval Type: unknown
- Conditions: control, explore, leverage
- Aethyme Commit: `a350afcb6068cd2f11ea2d284e5f929c05484618`

## Model

N/A

## Scorecard

| Condition | Score | Cost | Duration | Turns | Total Tokens | Input Tokens | Output Tokens | Cache Read | Cache Create |
|---|---|---|---|---|---|---|---|---|---|
| Control | - | - | - | - | - | - | - | - | - |
| Explore | - | - | - | - | - | - | - | - | - |
| Leverage | - | - | - | - | - | - | - | - | - |

## Prompts

### Control

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore
Explore the repository and produce a structured explanation.
```

### Explore

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore
Explore the repository and produce a structured explanation.
```

### Leverage

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore
Use Aethyme tools to navigate the repository graph. Explore the repository and produce a structured explanation.
```

## Agent Output

### Control

```json
null
```

### Explore

```json
null
```

### Leverage

```json
null
```

## Verdict

N/A

## Notes

Task: Explain this repo
Languages: javascript, python, typescript
Top-level directories: .githooks, .github, .husky, .lighthouseci, .playwright-mcp, .pnpm-store, .storybook, Agents, TODO, alerts, apps, backend, catalog, config, contracts, devops, docker, docs, e2e, functions, gcp-run-proxy, grafana-provisioning, load_tests, output, packages, patches, project, public, scripts, shared, src, stories, test-results, tests, tools
Files indexed: 77863
Functions indexed: 13048
Classes indexed: 8872
Docs indexed: 896
Configs indexed: 76
README: Agents/Skills Manager/README.md

Code areas:
- backend
- packages
- scripts

Reference areas:
- config
- docs

Key subareas:
- backend/accounts
- backend/core
- packages/app-shared
- packages/ui

Key configs:
- backend/pyproject.toml
- packages/auth/package.json

Entrypoints:
- packages/auth/src/index.ts
- packages/config/src/index.ts
- packages/types/src/index.ts

Representative code:
- Agents/skills/_meta/scripts/add_frontmatter.py
- Agents/skills/_meta/scripts/analyze_repo.py
- Agents/skills/_meta/scripts/analyze_usage_logs.py

Representative docs:
- Agents/Skills Manager/README.md
- Agents/skills/README.md
- Agents/skills/architecture/references/adr-index.md

Navigation order:
- tools/mcp-mordor/README.md
- docs/adr/010-monorepo-architecture.md
- packages
- tools
- Agents/skills/_meta/scripts/add_frontmatter.py

---

## Raw Data

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "backend",
    "packages",
    "scripts"
  ],
  "reference_areas": [
    "config",
    "docs"
  ],
  "entrypoints": [
    "packages/auth/src/index.ts",
    "packages/config/src/index.ts",
    "packages/types/src/index.ts"
  ],
  "important_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/references/adr-index.md"
  ],
  "key_configs": [
    "backend/pyproject.toml",
    "packages/auth/package.json"
  ],
  "key_languages": [
    "javascript",
    "python",
    "typescript"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "backend",
    "packages",
    "config"
  ],
  "representative_code_files": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/skills/_meta/scripts/analyze_usage_logs.py"
  ],
  "representative_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/references/adr-index.md"
  ],
  "evidence": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/Skills Manager/README.md"
  ]
}
```

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
    "Repo summary is informative but not currently machine-scored.",
    "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison."
  ]
}
```

### Per-Condition Run Records

#### Control

```json
null
```

#### Explore

```json
null
```

#### Leverage

```json
null
```

### Per-Condition Assessments

#### Control

```json
null
```

#### Explore

```json
null
```

#### Leverage

```json
null
```

### Context Pack

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
        "typescript"
      ],
      "top_level_dirs": [
        ".githooks",
        ".github",
        ".husky",
        ".lighthouseci",
        ".playwright-mcp",
        ".pnpm-store",
        ".storybook",
        "Agents",
        "TODO",
        "alerts",
        "apps",
        "backend",
        "catalog",
        "config",
        "contracts",
        "devops",
        "docker",
        "docs",
        "e2e",
        "functions",
        "gcp-run-proxy",
        "grafana-provisioning",
        "load_tests",
        "output",
        "packages",
        "patches",
        "project",
        "public",
        "scripts",
        "shared",
        "src",
        "stories",
        "test-results",
        "tests",
        "tools"
      ],
      "readme_path": "tools/mcp-mordor/README.md"
    },
    "files_count": 77863,
    "functions_count": 13048,
    "classes_count": 8872,
    "docs_count": 896,
    "configs_count": 76
  },
  "signals": {
    "boundary_clarity": {
      "score": 66,
      "level": "mixed",
      "evidence": [
        "cross-area semantic edges: 56081/290572",
        "source files with area assignment: 5162/5180",
        "generic source file names: 13"
      ]
    },
    "entrypoint_clarity": {
      "score": 100,
      "level": "strong",
      "evidence": [
        "direct code entrypoint edges: 1197",
        "configs with entrypoints: 6",
        "areas with ambiguous entrypoints: 1"
      ]
    },
    "config_hygiene": {
      "score": 21,
      "level": "weak",
      "evidence": [
        "operational configs: 39",
        "linked configs: 39/39",
        "duplicate config families: 26"
      ]
    },
    "hidden_coupling": {
      "score": 24,
      "level": "weak",
      "evidence": [
        "low-confidence semantic edges: 226388/266129",
        "high-confidence semantic edges: 19939/266129",
        "cross-area semantic edges: 47620/266129"
      ]
    },
    "parser_visibility": {
      "score": 88,
      "level": "strong",
      "evidence": [
        "supported source files: 5129/5180",
        "source files with semantic extraction: 3518/5180",
        "total extracted functions/classes: 21920"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/references/adr-index.md"
    ],
    "code_areas": [
      "backend",
      "packages",
      "scripts"
    ],
    "reference_areas": [
      "config",
      "docs"
    ],
    "subareas": [
      "backend/accounts",
      "backend/core",
      "packages/app-shared",
      "packages/ui"
    ],
    "entrypoints": [
      "packages/auth/src/index.ts",
      "packages/config/src/index.ts",
      "packages/types/src/index.ts"
    ],
    "key_configs": [
      "backend/pyproject.toml",
      "packages/auth/package.json"
    ],
    "representative_code_files": [
      "Agents/skills/_meta/scripts/add_frontmatter.py",
      "Agents/skills/_meta/scripts/analyze_repo.py",
      "Agents/skills/_meta/scripts/analyze_usage_logs.py",
      "Agents/skills/_meta/scripts/build_learning_index.py",
      "Agents/skills/_meta/scripts/build_skills_registry.py"
    ],
    "representative_docs": [
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/references/adr-index.md",
      "Agents/skills/architecture/references/data-flow.md",
      "Agents/skills/architecture/references/decisions.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": "tools/mcp-mordor/README.md",
      "file": "tools/mcp-mordor/README.md",
      "reason": "repository readme"
    },
    {
      "kind": "file",
      "id": "docs/adr/010-monorepo-architecture.md",
      "file": "docs/adr/010-monorepo-architecture.md",
      "reason": "architecture document"
    },
    {
      "kind": "folder",
      "id": "packages",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "folder",
      "id": "tools",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "tools/mcp-mordor/README.md",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [],
    "areas": [
      {
        "value": "packages",
        "kind": "area",
        "reason": "primary top-level area"
      },
      {
        "value": "tools",
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
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::generate_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::has_frontmatter",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::main",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "to": "Agents/skills/_meta/scripts/add_frontmatter.py::process_file",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::RepoAnalyzer",
      "kind": "defines"
    },
    {
      "from": "Agents/skills/_meta/scripts/analyze_repo.py",
      "to": "Agents/skills/_meta/scripts/analyze_repo.py::__init__",
      "kind": "defines"
    }
  ],
  "impact": [
    {
      "symbol": "packages/auth/src/index.ts",
      "file": "packages/auth/src/index.ts",
      "reason": "entrypoint candidate"
    },
    {
      "symbol": "packages/config/src/index.ts",
      "file": "packages/config/src/index.ts",
      "reason": "entrypoint candidate"
    }
  ],
  "snippets": [
    {
      "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "docs/adr/010-monorepo-architecture.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "tools/mcp-mordor/README.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    }
  ],
  "file_contents": [],
  "risk_flags": [],
  "navigation_order": [
    "tools/mcp-mordor/README.md",
    "docs/adr/010-monorepo-architecture.md",
    "packages",
    "tools",
    "Agents/skills/_meta/scripts/add_frontmatter.py"
  ],
  "budget": {
    "max_anchors": 5,
    "max_files": 8,
    "max_snippets": 8,
    "dependency_depth": 1,
    "impact_depth": 1,
    "snippet_window": 20,
    "content_budget": 80000,
    "max_content_files": 15,
    "max_lines_per_file": 500
  },
  "confidence": {
    "anchor_confidence": 0.85,
    "scope_confidence": 0.8
  },
  "activation_summary": {
    "activated_node_count": 97458,
    "max_depth_reached": 3,
    "top_activated": [
      {
        "id": "file:explore:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "file:explore:tools/mcp-mordor/README.md",
        "activation": 1.0
      },
      {
        "id": "dir:explore:packages",
        "activation": 1.0
      },
      {
        "id": "dir:explore:tools",
        "activation": 1.0
      },
      {
        "id": "doc:explore:tools/mcp-mordor/README.md",
        "activation": 1.0
      },
      {
        "id": "dir:explore:tools/mcp-mordor/src/tools",
        "activation": 1.0
      },
      {
        "id": "doc:explore:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "dir:explore:scripts/tools",
        "activation": 1.0
      },
      {
        "id": "area:explore:packages",
        "activation": 1.0
      },
      {
        "id": "file:explore:Agents/skills/_meta/scripts/add_frontmatter.py",
        "activation": 1.0
      }
    ]
  }
}
```

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": "tools/mcp-mordor/README.md",
        "file": "tools/mcp-mordor/README.md",
        "reason": "repository readme"
      },
      {
        "kind": "file",
        "id": "docs/adr/010-monorepo-architecture.md",
        "file": "docs/adr/010-monorepo-architecture.md",
        "reason": "architecture document"
      },
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "folder",
        "id": "tools",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "file": "Agents/skills/_meta/scripts/add_frontmatter.py",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      "tools/mcp-mordor/README.md",
      "docs/adr/010-monorepo-architecture.md",
      "packages",
      "tools",
      "Agents/skills/_meta/scripts/add_frontmatter.py"
    ],
    "in_scope_files": [
      "tools/mcp-mordor/README.md"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages",
      "tools"
    ],
    "out_of_scope": [],
    "risks": []
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand /Users/christophehenner/Downloads/Repositories/Playground/GRC/eval-benchmark/explore <anchor-id> --json-output"
  ]
}
```

### Repo Signals

```json
{
  "boundary_clarity": {
    "score": 66,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 56081/290572",
      "source files with area assignment: 5162/5180",
      "generic source file names: 13"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 1197",
      "configs with entrypoints: 6",
      "areas with ambiguous entrypoints: 1"
    ]
  },
  "config_hygiene": {
    "score": 21,
    "level": "weak",
    "evidence": [
      "operational configs: 39",
      "linked configs: 39/39",
      "duplicate config families: 26"
    ]
  },
  "hidden_coupling": {
    "score": 24,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 226388/266129",
      "high-confidence semantic edges: 19939/266129",
      "cross-area semantic edges: 47620/266129"
    ]
  },
  "parser_visibility": {
    "score": 88,
    "level": "strong",
    "evidence": [
      "supported source files: 5129/5180",
      "source files with semantic extraction: 3518/5180",
      "total extracted functions/classes: 21920"
    ]
  }
}
```

