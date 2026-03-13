# Eval Report: Explain this repo

Last Updated: 2026-03-11

- Repository: `/Users/christophehenner/Downloads/Repositories/Playground Aethyme`
- Generated: `2026-03-11T20:36:24.909868+00:00`
- Conditions: `control, explore, leverage`

## Summary

- Control prompt chars: `167`
- Explore prompt chars: `167`
- Leverage prompt chars: `219`
- Navigation items: `5`
- Risk items: `0`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 41610/264763",
      "source files with area assignment: 5401/5420",
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
    "score": 23,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 209685/236039",
      "high-confidence semantic edges: 14717/236039",
      "cross-area semantic edges: 31018/236039"
    ]
  },
  "parser_visibility": {
    "score": 87,
    "level": "strong",
    "evidence": [
      "supported source files: 5130/5420",
      "source files with semantic extraction: 3828/5420",
      "total extracted functions/classes: 16089"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
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
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Explore the repository and produce a structured explanation.
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
Repository path: /Users/christophehenner/Downloads/Repositories/Playground Aethyme
Use Aethyme tools to navigate the repository graph. Explore the repository and produce a structured explanation.
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


## Context Pack Audit

### Pack Summary

- Anchors: `5`
- Navigation order items: `5`
- In-scope files: `0`
- CLI commands: `3`

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground Aethyme",
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
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' <anchor-id> --json-output"
  ]
}
```

<!-- Signal-to-Noise Assessment
Rate the relevance of the navigation context provided to the leverage condition:
- Anchors: were the starting points useful?
- Scope: did in-scope files cover what the agent needed?
- Navigation order: was the reading order helpful?
- Noise: what was included but not needed?
-->
## Comparison

| Metric | Control | Explore | Leverage |
| --- | --- | --- | --- |
| Prompt chars | `167` | `167` | `219` |

- Navigation items surfaced: `5`
- Risk items surfaced: `0`

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
    "Repo summary is informative but not currently machine-scored.",
    "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison."
  ]
}
```

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
    "docs",
    "test-results"
  ],
  "entrypoints": [
    "packages/auth/src/index.ts",
    "packages/config/src/index.ts",
    "packages/types/src/index.ts"
  ],
  "important_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/SKILL.md"
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
    "docs"
  ],
  "representative_code_files": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/skills/_meta/scripts/analyze_usage_logs.py"
  ],
  "representative_docs": [
    "Agents/Skills Manager/README.md",
    "Agents/skills/README.md",
    "Agents/skills/architecture/SKILL.md"
  ],
  "evidence": [
    "Agents/skills/_meta/scripts/add_frontmatter.py",
    "Agents/skills/_meta/scripts/analyze_repo.py",
    "Agents/Skills Manager/README.md"
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
        "typescript"
      ],
      "top_level_dirs": [
        ".gcloud_tmp",
        ".githooks",
        ".github",
        ".husky",
        ".hypothesis",
        ".lighthouseci",
        ".playwright-mcp",
        ".pnpm-store",
        ".storybook",
        ".wrangler",
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
        "logs",
        "output",
        "packages",
        "patches",
        "playwright-report",
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
    "files_count": 106111,
    "functions_count": 12818,
    "classes_count": 3271,
    "docs_count": 1073,
    "configs_count": 79
  },
  "signals": {
    "boundary_clarity": {
      "score": 68,
      "level": "mixed",
      "evidence": [
        "cross-area semantic edges: 41610/264763",
        "source files with area assignment: 5401/5420",
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
      "score": 23,
      "level": "weak",
      "evidence": [
        "low-confidence semantic edges: 209685/236039",
        "high-confidence semantic edges: 14717/236039",
        "cross-area semantic edges: 31018/236039"
      ]
    },
    "parser_visibility": {
      "score": 87,
      "level": "strong",
      "evidence": [
        "supported source files: 5130/5420",
        "source files with semantic extraction: 3828/5420",
        "total extracted functions/classes: 16089"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      "Agents/Skills Manager/README.md",
      "Agents/skills/README.md",
      "Agents/skills/architecture/SKILL.md"
    ],
    "code_areas": [
      "backend",
      "packages",
      "scripts"
    ],
    "reference_areas": [
      "docs",
      "test-results"
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
      "Agents/skills/architecture/SKILL.md",
      "Agents/skills/architecture/references/adr-index.md",
      "Agents/skills/architecture/references/data-flow.md"
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
    "activated_node_count": 120678,
    "max_depth_reached": 3,
    "top_activated": [
      {
        "id": "area:Playground Aethyme:packages",
        "activation": 1.0
      },
      {
        "id": "area:Playground Aethyme:tools",
        "activation": 1.0
      },
      {
        "id": "file:Playground Aethyme:Agents/skills/_meta/scripts/add_frontmatter.py",
        "activation": 1.0
      },
      {
        "id": "doc:Playground Aethyme:tools/mcp-mordor/README.md",
        "activation": 1.0
      },
      {
        "id": "file:Playground Aethyme:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "doc:Playground Aethyme:docs/adr/010-monorepo-architecture.md",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:packages",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:tools/mcp-mordor/src/tools",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:tools",
        "activation": 1.0
      },
      {
        "id": "dir:Playground Aethyme:scripts/tools",
        "activation": 1.0
      }
    ]
  }
}
```

## Explanation

```text
Task: Explain this repo
Languages: javascript, python, typescript
Top-level directories: .gcloud_tmp, .githooks, .github, .husky, .hypothesis, .lighthouseci, .playwright-mcp, .pnpm-store, .storybook, .wrangler, Agents, TODO, alerts, apps, backend, catalog, config, contracts, devops, docker, docs, e2e, functions, gcp-run-proxy, grafana-provisioning, load_tests, logs, output, packages, patches, playwright-report, project, public, scripts, shared, src, stories, test-results, tests, tools
Files indexed: 106111
Functions indexed: 12818
Classes indexed: 3271
Docs indexed: 1073
Configs indexed: 79
README: Agents/Skills Manager/README.md

Code areas:
- backend
- packages
- scripts

Reference areas:
- docs
- test-results

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
- Agents/skills/architecture/SKILL.md

Navigation order:
- tools/mcp-mordor/README.md
- docs/adr/010-monorepo-architecture.md
- packages
- tools
- Agents/skills/_meta/scripts/add_frontmatter.py
```

## Graph Quality Notes

<!-- Post-run analysis of graph quality:
- Did the graph capture the right structural relationships?
- Were important edges missing or spurious?
- How did graph coverage affect each condition's performance?
-->

## Prompt Effectiveness

<!-- Post-run analysis of prompt design:
- Did the control prompt give the agent enough to work with?
- Did the explore prompt's CLI commands get used effectively?
- Did the leverage prompt's context file provide the right framing?
- What prompt changes would improve the next run?
-->

## Lessons & Action Items

<!-- Post-run action items:
- [ ] 
- [ ] 
- [ ] 
-->
