# Eval Report: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-11

- Repository: `/Users/christophehenner/Downloads/Repositories/Playground Aethyme`
- Generated: `2026-03-11T13:09:22.461836+00:00`

## Summary

- Control prompt chars: `313`
- Explore prompt chars: `313`
- Leverage prompt chars: `365`
- Navigation items: `2`
- Risk items: `1`

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
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
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
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
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
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
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

- Anchors: `2`
- Navigation order items: `2`
- In-scope files: `1`
- CLI commands: `4`

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground Aethyme",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "primary_area": "packages",
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "packages/auth/package.json",
        "file": "packages/auth/package.json",
        "reason": "manifest config anchor (score 63)"
      }
    ]
  },
  "anchor_expansions": {
    "packages": {
      "target": {
        "id": "area:Playground Aethyme:packages",
        "kind": "area",
        "label": "packages",
        "path": "packages",
        "language": null,
        "source": "structure",
        "confidence": 1000,
        "area": "packages",
        "annotations": []
      },
      "parents": [
        {
          "id": "area:Playground Aethyme:packages/app-shared",
          "kind": "area",
          "display": "packages/app-shared",
          "relation": "belongs_to",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/auth",
          "kind": "area",
          "display": "packages/auth",
          "relation": "belongs_to",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/config",
          "kind": "area",
          "display": "packages/config",
          "relation": "belongs_to",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/eslint-plugin-aeptus",
          "kind": "area",
          "display": "packages/eslint-plugin-aeptus",
          "relation": "belongs_to",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/types",
          "kind": "area",
          "display": "packages/types",
          "relation": "belongs_to",
          "confidence": 700
        }
      ],
      "children": [
        {
          "id": "area:Playground Aethyme:packages/app-shared",
          "kind": "area",
          "display": "packages/app-shared",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/auth",
          "kind": "area",
          "display": "packages/auth",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/config",
          "kind": "area",
          "display": "packages/config",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/eslint-plugin-aeptus",
          "kind": "area",
          "display": "packages/eslint-plugin-aeptus",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/types",
          "kind": "area",
          "display": "packages/types",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "area:Playground Aethyme:packages/ui",
          "kind": "area",
          "display": "packages/ui",
          "relation": "contains",
          "confidence": 700
        },
        {
          "id": "dir:Playground Aethyme:packages",
          "kind": "directory",
          "display": "packages",
          "relation": "contains",
          "confidence": 1000
        }
      ],
      "callers": [],
      "callees": [],
      "docs": [
        {
          "id": "doc:Playground Aethyme:AGENTS.md",
          "kind": "doc",
          "display": "AGENTS.md",
          "relation": "documents",
          "confidence": 700
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/README.md",
          "kind": "doc",
          "display": "Agents/skills/README.md",
          "relation": "documents",
          "confidence": 700
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/_meta/references/repo-conventions.md",
          "kind": "doc",
          "display": "Agents/skills/_meta/references/repo-conventions.md",
          "relation": "documents",
          "confidence": 700
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/architecture/SKILL.md",
          "kind": "doc",
          "display": "Agents/skills/architecture/SKILL.md",
          "relation": "documents",
          "confidence": 700
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/architecture/references/decisions.md",
          "kind": "doc",
          "display": "Agents/skills/architecture/references/decisions.md",
          "relation": "documents",
          "confidence": 700
        }
      ],
      "configs": [
        {
          "id": "config:Playground Aethyme:packages/app-shared/package.json",
          "kind": "config",
          "display": "packages/app-shared/package.json",
          "relation": "configures",
          "confidence": 800
        },
        {
          "id": "config:Playground Aethyme:packages/app-shared/package.json",
          "kind": "config",
          "display": "packages/app-shared/package.json",
          "relation": "entrypoint_for",
          "confidence": 700
        },
        {
          "id": "config:Playground Aethyme:packages/auth/package.json",
          "kind": "config",
          "display": "packages/auth/package.json",
          "relation": "configures",
          "confidence": 800
        },
        {
          "id": "config:Playground Aethyme:packages/auth/package.json",
          "kind": "config",
          "display": "packages/auth/package.json",
          "relation": "entrypoint_for",
          "confidence": 700
        },
        {
          "id": "config:Playground Aethyme:packages/auth/tsconfig.json",
          "kind": "config",
          "display": "packages/auth/tsconfig.json",
          "relation": "configures",
          "confidence": 800
        }
      ],
      "risks": []
    },
    "packages/auth/package.json": {
      "target": {
        "id": "config:Playground Aethyme:packages/auth/package.json",
        "kind": "config",
        "label": "manifest",
        "path": "packages/auth/package.json",
        "language": null,
        "source": "config",
        "confidence": 900,
        "area": "packages",
        "annotations": [
          "config_type: manifest",
          "navigation: entrypoint"
        ]
      },
      "parents": [
        {
          "id": "file:Playground Aethyme:packages/auth/package.json",
          "kind": "file",
          "display": "packages/auth/package.json",
          "relation": "defines",
          "confidence": 1000
        }
      ],
      "children": [],
      "callers": [],
      "callees": [],
      "docs": [
        {
          "id": "doc:Playground Aethyme:AGENTS.md",
          "kind": "doc",
          "display": "AGENTS.md",
          "relation": "documents",
          "confidence": 650
        },
        {
          "id": "doc:Playground Aethyme:Agents/Skills Manager/README.md",
          "kind": "doc",
          "display": "Agents/Skills Manager/README.md",
          "relation": "documents",
          "confidence": 650
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/architecture/references/decisions.md",
          "kind": "doc",
          "display": "Agents/skills/architecture/references/decisions.md",
          "relation": "documents",
          "confidence": 650
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/architecture/references/learn-log.md",
          "kind": "doc",
          "display": "Agents/skills/architecture/references/learn-log.md",
          "relation": "documents",
          "confidence": 650
        },
        {
          "id": "doc:Playground Aethyme:Agents/skills/architecture/references/structure.md",
          "kind": "doc",
          "display": "Agents/skills/architecture/references/structure.md",
          "relation": "documents",
          "confidence": 650
        }
      ],
      "configs": [
        {
          "id": "area:Playground Aethyme:packages",
          "kind": "area",
          "display": "packages",
          "relation": "configures",
          "confidence": 800
        },
        {
          "id": "area:Playground Aethyme:packages",
          "kind": "area",
          "display": "packages",
          "relation": "entrypoint_for",
          "confidence": 700
        },
        {
          "id": "file:Playground Aethyme:.storybook/main.js",
          "kind": "file",
          "display": ".storybook/main.js",
          "relation": "configures",
          "confidence": 750
        },
        {
          "id": "file:Playground Aethyme:Agents/skills/auth/references/authentication.md",
          "kind": "file",
          "display": "Agents/skills/auth/references/authentication.md",
          "relation": "configures",
          "confidence": 750
        },
        {
          "id": "file:Playground Aethyme:Agents/skills/auth/references/rbac.md",
          "kind": "file",
          "display": "Agents/skills/auth/references/rbac.md",
          "relation": "configures",
          "confidence": 750
        }
      ],
      "risks": [
        "packages/auth/package.json (Auth): authentication boundary"
      ]
    }
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "packages",
      "packages/auth/package.json"
    ],
    "in_scope_files": [
      "packages/auth/package.json"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages"
    ],
    "out_of_scope": [
      ".gcloud_tmp",
      ".githooks",
      ".github",
      ".github/PULL_REQUEST_TEMPLATE",
      ".github/workflows",
      ".husky",
      ".hypothesis",
      ".lighthouseci",
      ".playwright-mcp",
      ".pnpm-store",
      ".storybook",
      ".wrangler",
      "Agents",
      "Agents/Skills Manager",
      "Agents/skills",
      "Agents/tasks",
      "TODO",
      "alerts",
      "apps",
      "apps/customer",
      "apps/mordor",
      "apps/organizations",
      "backend",
      "backend/accounts",
      "backend/adn",
      "backend/aep_backend",
      "backend/ai_providers",
      "backend/analytics",
      "backend/api_keys",
      "backend/api_usage",
      "backend/audit",
      "backend/automations",
      "backend/collaboration",
      "backend/common",
      "backend/community",
      "backend/controls",
      "backend/core",
      "backend/directory",
      "backend/docs",
      "backend/documents",
      "backend/environment",
      "backend/events",
      "backend/frameworks",
      "backend/information",
      "backend/integrations",
      "backend/k8s",
      "backend/knowledge",
      "backend/localization",
      "backend/mapping_intelligence",
      "backend/menu_overrides",
      "backend/middleware",
      "backend/onboarding",
      "backend/operational",
      "backend/page_actions",
      "backend/posture",
      "backend/project",
      "backend/reports",
      "backend/scripts",
      "backend/tasks",
      "backend/templates",
      "backend/test-results",
      "backend/tests",
      "backend/thirdparties",
      "backend/webhooks",
      "catalog",
      "config",
      "config/bundle",
      "config/lighthouse",
      "config/observability",
      "config/quality",
      "contracts",
      "contracts/config",
      "devops",
      "docker",
      "docs",
      "docs/adr",
      "docs/agents",
      "docs/api",
      "docs/architecture",
      "docs/badges",
      "docs/collaboration",
      "docs/contracts",
      "docs/db",
      "docs/design-system",
      "docs/development",
      "docs/docker",
      "docs/engineering",
      "docs/feature-specs",
      "docs/guides",
      "docs/observability",
      "docs/onboarding",
      "docs/openapi",
      "docs/otlp",
      "docs/performance",
      "docs/planning",
      "docs/plans",
      "docs/prd",
      "docs/reference",
      "docs/reports",
      "docs/runbooks",
      "docs/security",
      "docs/testing",
      "e2e",
      "e2e/fixtures",
      "e2e/page-objects",
      "functions",
      "gcp-run-proxy",
      "gcp-run-proxy/src",
      "gcp-run-proxy/test",
      "grafana-provisioning",
      "load_tests",
      "logs",
      "output",
      "packages/app-shared",
      "packages/auth",
      "packages/config",
      "packages/eslint-plugin-aeptus",
      "packages/types",
      "packages/ui",
      "patches",
      "playwright-report",
      "project",
      "public",
      "scripts",
      "scripts/a11y",
      "scripts/adr",
      "scripts/ai",
      "scripts/archive",
      "scripts/assets",
      "scripts/catalog",
      "scripts/checks",
      "scripts/ci",
      "scripts/contracts",
      "scripts/design-system",
      "scripts/dev",
      "scripts/docs",
      "scripts/generate",
      "scripts/help",
      "scripts/i18n",
      "scripts/k6",
      "scripts/maintenance",
      "scripts/naming",
      "scripts/observability",
      "scripts/openapi",
      "scripts/perf",
      "scripts/security",
      "scripts/tools",
      "scripts/trace",
      "scripts/validation",
      "scripts/ws",
      "shared",
      "src",
      "src/i18n",
      "stories",
      "test-results",
      "tests",
      "tests/contract",
      "tools",
      "tools/mcp-mordor"
    ],
    "risks": [
      "packages/auth/package.json"
    ]
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs '/Users/christophehenner/Downloads/Repositories/Playground Aethyme' packages --json-output",
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
| Prompt chars | `313` | `313` | `365` |

- Navigation items surfaced: `2`
- Risk items surfaced: `1`

## Reference

### Output Schema

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "config_target",
    "code_target",
    "management_area",
    "relationship_chain",
    "rejected_candidates",
    "confidence"
  ],
  "properties": {
    "config_target": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "path",
        "why"
      ],
      "properties": {
        "path": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "code_target": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "path",
        "why"
      ],
      "properties": {
        "path": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "management_area": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "name",
        "why"
      ],
      "properties": {
        "name": {
          "type": "string"
        },
        "why": {
          "type": "string"
        }
      }
    },
    "relationship_chain": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "from",
          "to",
          "relation"
        ],
        "properties": {
          "from": {
            "type": "string"
          },
          "to": {
            "type": "string"
          },
          "relation": {
            "type": "string"
          }
        }
      }
    },
    "rejected_candidates": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "path",
          "reason"
        ],
        "properties": {
          "path": {
            "type": "string"
          },
          "reason": {
            "type": "string"
          }
        }
      }
    },
    "confidence": {
      "type": "string"
    }
  }
}
```

### Scoring Rubric

```json
{
  "weights": {
    "config_target": 30,
    "code_target": 30,
    "management_area": 20,
    "relationship_chain": 20
  },
  "notes": [
    "Exact config/code path matches carry most of the score.",
    "Relationship chain must express both ownership and management links.",
    "Path normalization strips markdown links, line anchors, absolute prefixes, and leading ./ before comparison."
  ]
}
```

### Reference Output

```json
{
  "config_target": {
    "path": "packages/auth/package.json",
    "why": "manifest/config linked to the runtime entrypoint"
  },
  "code_target": {
    "path": "packages/auth/src/index.ts",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": "packages",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages",
      "relation": "configures"
    }
  ],
  "rejected_candidates": [],
  "confidence": "high"
}
```

### Challenge

```json
{
  "kind": "navigation_ctf",
  "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "management_area": "packages",
  "reference_output": {
    "config_target": {
      "path": "packages/auth/package.json",
      "why": "manifest/config linked to the runtime entrypoint"
    },
    "code_target": {
      "path": "packages/auth/src/index.ts",
      "why": "entrypoint file linked by the configuration graph"
    },
    "management_area": {
      "name": "packages",
      "why": "top-level area linked by the configuration graph"
    },
    "relationship_chain": [
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages",
        "relation": "configures"
      }
    ],
    "rejected_candidates": [],
    "confidence": "high"
  }
}
```

## Aethyme Pack

```json
{
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "packages/auth/package.json",
        "file": "packages/auth/package.json",
        "reason": "manifest config anchor (score 63)"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "packages",
      "packages/auth/package.json"
    ],
    "in_scope_files": [
      "packages/auth/package.json"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "packages"
    ],
    "out_of_scope": [
      ".gcloud_tmp",
      ".githooks",
      ".github",
      ".github/PULL_REQUEST_TEMPLATE",
      ".github/workflows",
      ".husky",
      ".hypothesis",
      ".lighthouseci",
      ".playwright-mcp",
      ".pnpm-store",
      ".storybook",
      ".wrangler",
      "Agents",
      "Agents/Skills Manager",
      "Agents/skills",
      "Agents/tasks",
      "TODO",
      "alerts",
      "apps",
      "apps/customer",
      "apps/mordor",
      "apps/organizations",
      "backend",
      "backend/accounts",
      "backend/adn",
      "backend/aep_backend",
      "backend/ai_providers",
      "backend/analytics",
      "backend/api_keys",
      "backend/api_usage",
      "backend/audit",
      "backend/automations",
      "backend/collaboration",
      "backend/common",
      "backend/community",
      "backend/controls",
      "backend/core",
      "backend/directory",
      "backend/docs",
      "backend/documents",
      "backend/environment",
      "backend/events",
      "backend/frameworks",
      "backend/information",
      "backend/integrations",
      "backend/k8s",
      "backend/knowledge",
      "backend/localization",
      "backend/mapping_intelligence",
      "backend/menu_overrides",
      "backend/middleware",
      "backend/onboarding",
      "backend/operational",
      "backend/page_actions",
      "backend/posture",
      "backend/project",
      "backend/reports",
      "backend/scripts",
      "backend/tasks",
      "backend/templates",
      "backend/test-results",
      "backend/tests",
      "backend/thirdparties",
      "backend/webhooks",
      "catalog",
      "config",
      "config/bundle",
      "config/lighthouse",
      "config/observability",
      "config/quality",
      "contracts",
      "contracts/config",
      "devops",
      "docker",
      "docs",
      "docs/adr",
      "docs/agents",
      "docs/api",
      "docs/architecture",
      "docs/badges",
      "docs/collaboration",
      "docs/contracts",
      "docs/db",
      "docs/design-system",
      "docs/development",
      "docs/docker",
      "docs/engineering",
      "docs/feature-specs",
      "docs/guides",
      "docs/observability",
      "docs/onboarding",
      "docs/openapi",
      "docs/otlp",
      "docs/performance",
      "docs/planning",
      "docs/plans",
      "docs/prd",
      "docs/reference",
      "docs/reports",
      "docs/runbooks",
      "docs/security",
      "docs/testing",
      "e2e",
      "e2e/fixtures",
      "e2e/page-objects",
      "functions",
      "gcp-run-proxy",
      "gcp-run-proxy/src",
      "gcp-run-proxy/test",
      "grafana-provisioning",
      "load_tests",
      "logs",
      "output",
      "packages/app-shared",
      "packages/auth",
      "packages/config",
      "packages/eslint-plugin-aeptus",
      "packages/types",
      "packages/ui",
      "patches",
      "playwright-report",
      "project",
      "public",
      "scripts",
      "scripts/a11y",
      "scripts/adr",
      "scripts/ai",
      "scripts/archive",
      "scripts/assets",
      "scripts/catalog",
      "scripts/checks",
      "scripts/ci",
      "scripts/contracts",
      "scripts/design-system",
      "scripts/dev",
      "scripts/docs",
      "scripts/generate",
      "scripts/help",
      "scripts/i18n",
      "scripts/k6",
      "scripts/maintenance",
      "scripts/naming",
      "scripts/observability",
      "scripts/openapi",
      "scripts/perf",
      "scripts/security",
      "scripts/tools",
      "scripts/trace",
      "scripts/validation",
      "scripts/ws",
      "shared",
      "src",
      "src/i18n",
      "stories",
      "test-results",
      "tests",
      "tests/contract",
      "tools",
      "tools/mcp-mordor"
    ],
    "risks": [
      "packages/auth/package.json"
    ]
  },
  "task_pack": {
    "task": {
      "raw": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "normalized": "find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "kind": "navigate_config_ownership"
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
      "overview_docs": [],
      "code_areas": [],
      "reference_areas": [],
      "subareas": [],
      "entrypoints": [],
      "key_configs": [],
      "representative_code_files": [],
      "representative_docs": []
    },
    "anchors": [
      {
        "kind": "folder",
        "id": "packages",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "packages/auth/package.json",
        "file": "packages/auth/package.json",
        "reason": "manifest config anchor (score 63)"
      }
    ],
    "in_scope": {
      "files": [
        {
          "value": "packages/auth/package.json",
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
        }
      ]
    },
    "out_of_scope": {
      "files": [],
      "symbols": [],
      "areas": [
        {
          "value": ".gcloud_tmp",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".githooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".github/PULL_REQUEST_TEMPLATE",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github/workflows",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".husky",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".hypothesis",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".lighthouseci",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".playwright-mcp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".pnpm-store",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".storybook",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": ".wrangler",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "Agents/Skills Manager",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/skills",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents/tasks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "TODO",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "alerts",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "apps",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "apps/customer",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/mordor",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps/organizations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "backend/accounts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/adn",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/aep_backend",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/ai_providers",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/analytics",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/api_keys",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/api_usage",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/audit",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/automations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/collaboration",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/common",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/community",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/controls",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/core",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/directory",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/documents",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/environment",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/events",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/frameworks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/information",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/integrations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/k8s",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/knowledge",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/localization",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/mapping_intelligence",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/menu_overrides",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/middleware",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/onboarding",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/operational",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/page_actions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/posture",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/project",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/reports",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/scripts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/tasks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/templates",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/test-results",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/thirdparties",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "backend/webhooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "catalog",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "config",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "config/bundle",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/lighthouse",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "config/quality",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "contracts",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "contracts/config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "devops",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "docker",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "docs",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "docs/adr",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/agents",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/api",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/architecture",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/badges",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/collaboration",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/contracts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/db",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/design-system",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/development",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/docker",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/engineering",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/feature-specs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/guides",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/onboarding",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/openapi",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/otlp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/performance",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/planning",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/plans",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/prd",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/reference",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/reports",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/runbooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/security",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/testing",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "e2e/fixtures",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "e2e/page-objects",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "functions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "gcp-run-proxy",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "gcp-run-proxy/src",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "gcp-run-proxy/test",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "grafana-provisioning",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "load_tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "logs",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "output",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/app-shared",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/auth",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/eslint-plugin-aeptus",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/types",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages/ui",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "patches",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "playwright-report",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "project",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "public",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "scripts",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "scripts/a11y",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/adr",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/ai",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/archive",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/assets",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/catalog",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/checks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/ci",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/contracts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/design-system",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/dev",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/generate",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/help",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/i18n",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/k6",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/maintenance",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/naming",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/observability",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/openapi",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/perf",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/security",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/tools",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/trace",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/validation",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/ws",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "shared",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "src/i18n",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "stories",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "test-results",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "tests",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "tests/contract",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tools",
          "kind": "area",
          "reason": "partially activated \u2014 exercise caution"
        },
        {
          "value": "tools/mcp-mordor",
          "kind": "area",
          "reason": "outside the matched primary area"
        }
      ]
    },
    "dependencies": [],
    "impact": [],
    "snippets": [
      {
        "file": "packages/auth/package.json",
        "start_line": 1,
        "end_line": 20,
        "kind": "overview"
      }
    ],
    "risk_flags": [
      {
        "scope": "packages/auth/package.json",
        "area": "auth",
        "level": "high",
        "reason": "authentication boundary"
      }
    ],
    "navigation_order": [
      "packages",
      "packages/auth/package.json"
    ],
    "budget": {
      "max_anchors": 3,
      "max_files": 5,
      "max_snippets": 8,
      "dependency_depth": 1,
      "impact_depth": 1,
      "snippet_window": 20
    },
    "confidence": {
      "anchor_confidence": 0.75,
      "scope_confidence": 0.7
    },
    "activation_summary": {
      "activated_node_count": 13795,
      "max_depth_reached": 4,
      "top_activated": [
        {
          "id": "config:Playground Aethyme:packages/auth/package.json",
          "activation": 1.0
        },
        {
          "id": "file:Playground Aethyme:packages/auth/package.json",
          "activation": 1.0
        },
        {
          "id": "area:Playground Aethyme:packages",
          "activation": 1.0
        },
        {
          "id": "dir:Playground Aethyme:packages",
          "activation": 1.0
        },
        {
          "id": "file:Playground Aethyme:apps/mordor/src/help/types.ts",
          "activation": 0.5225
        },
        {
          "id": "file:Playground Aethyme:test-results/run-2026-02-24-19-58-47-174-83282/vitest.log",
          "activation": 0.5225
        },
        {
          "id": "file:Playground Aethyme:packages/app-shared/src/schemas/session.api.ts",
          "activation": 0.5225
        },
        {
          "id": "file:Playground Aethyme:test-results/run-2026-02-06-09-48-58-100-95927/vitest.log",
          "activation": 0.5225
        },
        {
          "id": "file:Playground Aethyme:apps/organizations/src/main.tsx",
          "activation": 0.5225
        },
        {
          "id": "file:Playground Aethyme:vitest.config.ts",
          "activation": 0.5225
        }
      ]
    }
  }
}
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
