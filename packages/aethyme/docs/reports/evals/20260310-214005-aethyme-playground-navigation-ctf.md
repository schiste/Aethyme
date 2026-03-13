# Eval Report: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-10

- Repository: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- Generated: `2026-03-10T21:40:05.315126+00:00`

## Summary

- Control prompt chars: `322`
- Explore prompt chars: `1633`
- Leverage prompt chars: `257`
- Navigation items: `1`
- Risk items: `0`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 41401/263859",
      "source files with area assignment: 5348/5367",
      "generic source file names: 13"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 1192",
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
      "low-confidence semantic edges: 209023/235269",
      "high-confidence semantic edges: 14639/235269",
      "cross-area semantic edges: 30868/235269"
    ]
  },
  "parser_visibility": {
    "score": 87,
    "level": "strong",
    "evidence": [
      "supported source files: 5099/5367",
      "source files with semantic extraction: 3816/5367",
      "total extracted functions/classes: 16018"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
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
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme Playground
Explore the repository and produce a structured explanation.

You have access to the following graph navigation commands.
Use them via Bash to explore the repository graph:

  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' packages --json-output
  cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output

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
Task: Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.
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


## Context Pack Audit

### Pack Summary

- Anchors: `2`
- Navigation order items: `1`
- In-scope files: `0`
- CLI commands: `4`

### Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme Playground",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": "backend/controls",
        "file": null,
        "reason": "area match"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "backend/controls"
    ],
    "in_scope_files": [],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "backend/controls"
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
      "packages",
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
    "risks": []
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' --task <task> --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' packages --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Aethyme Playground' <anchor-id> --json-output"
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
| Prompt chars | `322` | `1633` | `257` |

- Navigation items surfaced: `1`
- Risk items surfaced: `0`

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
    "path": "packages/ui/src/tokens/index.ts",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": "packages",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": "packages/auth/package.json",
      "to": "packages",
      "relation": "configures"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/api/mapping-intelligence/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/rbac-canonical.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/auth/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/adn/local-entries/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/adn/taxonomy/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/admin/db/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/adn/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/adn/widgets/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/chrome/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/chrome/layout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/domain/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/ComplianceStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/PolicyStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/information/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/integrity/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/layout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/mapping-intelligence/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/assessments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/changes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/comments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/exceptions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/incidents/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/components/suppliers/signals/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/config/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/config/page-actions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/analytics/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/assignments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/integrations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/menu-manager/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/risk-rules/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/components/users/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/admin/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/auth/utils/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/components/builder-v2/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/automations/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/bulk/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/campaigns/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/collaboration/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/controls/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/environment/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/ComplianceStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/PolicyStudio/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/components/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/information/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/notifications/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/__stories__/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/components/steps/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/onboarding2/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/org/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/platform/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/platform/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/policy/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/policy/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/profile/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/reporting/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/inspectors/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/builder/nodes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/calculator/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/manager/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/components/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/services/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/risk/utils/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/assessments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/changes/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/comments/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/exceptions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/incidents/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/components/signals/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/pages/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/suppliers/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/tasks/components/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/features/tasks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/admin/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/chrome/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/collaboration/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/environment/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/information/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/menu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/hooks/permissions/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/lib/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/preauth/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/preauth/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/providers/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/schemas/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/schemas/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/shared/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/shared/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/stores/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/api/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/automations/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/app-shared/src/types/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/ability.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/can.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/logout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/permissionGrouping.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/rbac-canonical.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/auth/src/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/config/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/eslint-plugin-aeptus/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/types/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/types/src/session.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Avatar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/BackButton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Badge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Button/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Card/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Checkbox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/DataSourceBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Divider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/DropZone/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/HelpTrigger/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Icon/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/IconButton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Input/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/LazyImage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Logo/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ManagedChip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/NumericStepper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/OptionList/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/PhoneInput/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ScopeRiskBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ScrollArea/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/SemanticBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Skeleton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Spinner/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/StatusIndicator/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Switch/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Table/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Textarea/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/TierBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/ToolbarSelect/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/atoms/Tooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/AreaChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/BarChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/ChartCard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/KpiWidget/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/LineChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/PieChart/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/charts/Sparkline/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Accordion/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ActionBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ActionForm/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Alert/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AlertDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AsyncCombobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/AvatarUpload/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Breadcrumbs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/CodeSnippet/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Collapsible/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ColorPicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Combobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ComboboxWithRefs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Command/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ConfirmDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DatePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DateRangePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DateTimePicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DescriptionList/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DialogSection/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Drawer/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/DropdownMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/EmptyState/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/FieldWrapper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/FloatingPanel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/InlineEdit/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Input/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Label/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Modal/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/MultiSelect/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/MultiTypeCombobox/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/OverlayWidget/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/OwnerSelectBadge/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Pagination/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Popover/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ProgressBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/PullToRefresh/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RHFForm/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RadioGroup/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/RangeSlider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SavedViews/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SearchInput/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Section/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SegmentedControl/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Select/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/SensitiveValueDisplay/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ServerPagination/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Sheet/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Skeleton/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Skeleton/variants/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Slider/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/StatusBanner/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Stepper/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Switch/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/TableSortHeader/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Tabs/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Toast/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ToastContainer/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/TokenPicker/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Toolbar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/Tooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ValidationSummary/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/molecules/ViewToggle/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Carousel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/ColumnVisibilityMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/CommandPalette/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/DataTable/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/FileUpload/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Form/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/RichTextEditor/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/organisms/Sidebar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/BulkActionsToolbar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/FilterBuilder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/IntegrationCard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ActionBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ActivityFeed/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/BulkEditDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/CardFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/CollectionView/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/EnhancedSearchBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ExportDialog/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FacetFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FilterBar/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/FilterPanel/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/KanbanBoard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/LogicBuilder/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ModalFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/NotificationCenter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/RangeSliderFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/Timeline/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/ToggleFilter/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/compositions/UserMenu/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/OnboardingTooltip/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/OnboardingTooltips/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/flows/Wizard/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/DashboardLayout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/DetailPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/FormPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/ListPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/MasterDetail/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/ResourceLayout/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/SettingsPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/patterns/templates/WizardPage/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/utilities/VirtualizedTableBody/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/components/utilities/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/hooks/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/index.ts",
      "relation": "entrypoint_for"
    },
    {
      "from": "packages/auth/package.json",
      "to": "packages/ui/src/tokens/index.ts",
      "relation": "entrypoint_for"
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
      "path": "packages/ui/src/tokens/index.ts",
      "why": "entrypoint file linked by the configuration graph"
    },
    "management_area": {
      "name": "packages",
      "why": "top-level area linked by the configuration graph"
    },
    "relationship_chain": [
      {
        "from": "packages/auth/package.json",
        "to": "packages",
        "relation": "configures"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/api/mapping-intelligence/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/rbac-canonical.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/auth/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/adn/local-entries/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/adn/taxonomy/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/admin/db/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/adn/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/adn/widgets/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/chrome/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/chrome/layout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/domain/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/ComplianceStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/PolicyStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/information/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/integrity/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/layout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/mapping-intelligence/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/assessments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/changes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/comments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/exceptions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/incidents/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/components/suppliers/signals/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/config/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/config/page-actions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/analytics/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/assignments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/integrations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/menu-manager/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/risk-rules/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/components/users/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/admin/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/auth/utils/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/components/builder-v2/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/automations/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/bulk/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/campaigns/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/collaboration/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/controls/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/environment/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/ComplianceStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/PolicyStudio/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/components/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/information/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/notifications/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/__stories__/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/components/steps/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/onboarding2/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/org/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/platform/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/platform/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/policy/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/policy/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/profile/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/reporting/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/inspectors/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/builder/nodes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/calculator/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/manager/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/components/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/services/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/risk/utils/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/assessments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/changes/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/comments/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/exceptions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/incidents/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/components/signals/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/pages/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/suppliers/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/tasks/components/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/features/tasks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/admin/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/chrome/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/collaboration/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/environment/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/information/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/menu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/hooks/permissions/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/lib/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/preauth/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/preauth/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/providers/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/schemas/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/schemas/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/shared/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/shared/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/stores/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/api/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/automations/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/app-shared/src/types/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/ability.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/can.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/logout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/permissionGrouping.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/rbac-canonical.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/auth/src/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/config/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/eslint-plugin-aeptus/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/types/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/types/src/session.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Avatar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/BackButton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Badge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Button/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Card/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Checkbox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/DataSourceBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Divider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/DropZone/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/HelpTrigger/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Icon/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/IconButton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Input/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/LazyImage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Logo/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ManagedChip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/NumericStepper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/OptionList/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/PhoneInput/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ScopeRiskBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ScrollArea/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/SemanticBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Skeleton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Spinner/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/StatusIndicator/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Switch/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Table/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Textarea/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/TierBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/ToolbarSelect/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/atoms/Tooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/AreaChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/BarChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/ChartCard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/KpiWidget/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/LineChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/PieChart/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/charts/Sparkline/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Accordion/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ActionBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ActionForm/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Alert/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AlertDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AsyncCombobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/AvatarUpload/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Breadcrumbs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/CodeSnippet/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Collapsible/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ColorPicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Combobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ComboboxWithRefs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Command/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ConfirmDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DatePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DateRangePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DateTimePicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DescriptionList/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DialogSection/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Drawer/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/DropdownMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/EmptyState/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/FieldWrapper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/FloatingPanel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/InlineEdit/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Input/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Label/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Modal/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/MultiSelect/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/MultiTypeCombobox/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/OverlayWidget/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/OwnerSelectBadge/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Pagination/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Popover/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ProgressBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/PullToRefresh/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RHFForm/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RadioGroup/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/RangeSlider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SavedViews/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SearchInput/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Section/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SegmentedControl/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Select/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/SensitiveValueDisplay/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ServerPagination/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Sheet/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Skeleton/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Skeleton/variants/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Slider/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/StatusBanner/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Stepper/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Switch/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/TableSortHeader/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Tabs/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Toast/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ToastContainer/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/TokenPicker/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Toolbar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/Tooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ValidationSummary/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/molecules/ViewToggle/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Carousel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/ColumnVisibilityMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/CommandPalette/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/DataTable/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/FileUpload/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Form/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/RichTextEditor/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/organisms/Sidebar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/BulkActionsToolbar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/FilterBuilder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/IntegrationCard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ActionBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ActivityFeed/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/BulkEditDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/CardFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/CollectionView/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/EnhancedSearchBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ExportDialog/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FacetFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FilterBar/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/FilterPanel/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/KanbanBoard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/LogicBuilder/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ModalFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/NotificationCenter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/RangeSliderFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/Timeline/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/ToggleFilter/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/compositions/UserMenu/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/OnboardingTooltip/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/OnboardingTooltips/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/flows/Wizard/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/DashboardLayout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/DetailPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/FormPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/ListPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/MasterDetail/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/ResourceLayout/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/SettingsPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/patterns/templates/WizardPage/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/utilities/VirtualizedTableBody/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/components/utilities/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/hooks/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/index.ts",
        "relation": "entrypoint_for"
      },
      {
        "from": "packages/auth/package.json",
        "to": "packages/ui/src/tokens/index.ts",
        "relation": "entrypoint_for"
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
        "id": "backend/controls",
        "file": null,
        "reason": "area match"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the packages area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "backend/controls"
    ],
    "in_scope_files": [],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "backend/controls"
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
      "packages",
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
    "risks": []
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
      "files_count": 106052,
      "functions_count": 12763,
      "classes_count": 3255,
      "docs_count": 1070,
      "configs_count": 79
    },
    "signals": {
      "boundary_clarity": {
        "score": 68,
        "level": "mixed",
        "evidence": [
          "cross-area semantic edges: 41401/263859",
          "source files with area assignment: 5348/5367",
          "generic source file names: 13"
        ]
      },
      "entrypoint_clarity": {
        "score": 100,
        "level": "strong",
        "evidence": [
          "direct code entrypoint edges: 1192",
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
          "low-confidence semantic edges: 209023/235269",
          "high-confidence semantic edges: 14639/235269",
          "cross-area semantic edges: 30868/235269"
        ]
      },
      "parser_visibility": {
        "score": 87,
        "level": "strong",
        "evidence": [
          "supported source files: 5099/5367",
          "source files with semantic extraction: 3816/5367",
          "total extracted functions/classes: 16018"
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
        "id": "backend/controls",
        "file": null,
        "reason": "area match"
      }
    ],
    "in_scope": {
      "files": [],
      "symbols": [],
      "areas": [
        {
          "value": "backend/controls",
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
          "reason": "outside the matched primary area"
        },
        {
          "value": ".githooks",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": ".lighthouseci",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".playwright-mcp",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".pnpm-store",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".storybook",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".wrangler",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "Agents",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "alerts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "apps",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "config",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "contracts/config",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "devops",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docker",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "load_tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "logs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "output",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "packages",
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "project",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "public",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts",
          "kind": "area",
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
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
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests/contract",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tools",
          "kind": "area",
          "reason": "outside the matched primary area"
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
    "snippets": [],
    "risk_flags": [],
    "navigation_order": [
      "backend/controls"
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
      "activated_node_count": 4909,
      "max_depth_reached": 3,
      "top_activated": [
        {
          "id": "dir:Aethyme Playground:backend/controls",
          "activation": 1.0
        },
        {
          "id": "area:Aethyme Playground:backend/controls",
          "activation": 1.0
        },
        {
          "id": "file:Aethyme Playground:backend/controls/tests/test_scope_seeding.py",
          "activation": 0.275
        },
        {
          "id": "file:Aethyme Playground:backend/controls/tests/test_e2e_access_review_flow.py",
          "activation": 0.275
        },
        {
          "id": "dir:Aethyme Playground:backend/controls/models",
          "activation": 0.275
        },
        {
          "id": "file:Aethyme Playground:backend/controls/serializer_modules/bulk.py",
          "activation": 0.275
        },
        {
          "id": "file:Aethyme Playground:backend/controls/tests/test_validity_decisions.py",
          "activation": 0.275
        },
        {
          "id": "file:Aethyme Playground:backend/controls/migrations/0014_add_composite_indexes.py",
          "activation": 0.275
        },
        {
          "id": "dir:Aethyme Playground:backend/controls/tests",
          "activation": 0.275
        },
        {
          "id": "file:Aethyme Playground:backend/controls/models/exception.py",
          "activation": 0.275
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
