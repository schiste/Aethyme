# Eval Report: Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-08

- Repository: `/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme`
- Generated: `2026-03-08T21:22:31.974300+00:00`

## Summary

- Baseline prompt chars: `327`
- Aethyme prompt chars: `256`
- Navigation items: `2`
- Risk items: `22`

## Repo Signals

```json
{
  "boundary_clarity": {
    "score": 68,
    "level": "mixed",
    "evidence": [
      "cross-area semantic edges: 739/5111",
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
      "low-confidence semantic edges: 2448/4050",
      "high-confidence semantic edges: 1239/4050",
      "cross-area semantic edges: 426/4050"
    ]
  },
  "parser_visibility": {
    "score": 91,
    "level": "strong",
    "evidence": [
      "supported source files: 150/151",
      "source files with semantic extraction: 112/151",
      "total extracted functions/classes: 1003"
    ]
  }
}
```

## Control

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme
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

## Aethyme

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.
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
Aethyme runner not executed.
```

### Structured Output

```json
null
```

## Comparison

- Prompt chars delta: `-71`
- Navigation items surfaced: `2`
- Risk items surfaced: `22`

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
    "Relationship chain must express both ownership and management links."
  ]
}
```

### Reference Output

```json
{
  "config_target": {
    "path": ".github/actions/aethyme-scorecard/package.json",
    "why": "manifest/config linked to the runtime entrypoint"
  },
  "code_target": {
    "path": ".github/actions/aethyme-scorecard/index.js",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": ".github",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": ".github/actions/aethyme-scorecard/package.json",
      "to": ".github",
      "relation": "configures"
    },
    {
      "from": ".github/actions/aethyme-scorecard/package.json",
      "to": ".github/actions/aethyme-scorecard/index.js",
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
  "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "reference_output": {
    "config_target": {
      "path": ".github/actions/aethyme-scorecard/package.json",
      "why": "manifest/config linked to the runtime entrypoint"
    },
    "code_target": {
      "path": ".github/actions/aethyme-scorecard/index.js",
      "why": "entrypoint file linked by the configuration graph"
    },
    "management_area": {
      "name": ".github",
      "why": "top-level area linked by the configuration graph"
    },
    "relationship_chain": [
      {
        "from": ".github/actions/aethyme-scorecard/package.json",
        "to": ".github",
        "relation": "configures"
      },
      {
        "from": ".github/actions/aethyme-scorecard/package.json",
        "to": ".github/actions/aethyme-scorecard/index.js",
        "relation": "entrypoint_for"
      }
    ],
    "rejected_candidates": [],
    "confidence": "high"
  }
}
```

## Navigation Context

```json
{
  "mode": "iterative_navigation",
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "challenge": {
    "kind": "navigation_ctf",
    "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "reference_output": {
      "config_target": {
        "path": ".github/actions/aethyme-scorecard/package.json",
        "why": "manifest/config linked to the runtime entrypoint"
      },
      "code_target": {
        "path": ".github/actions/aethyme-scorecard/index.js",
        "why": "entrypoint file linked by the configuration graph"
      },
      "management_area": {
        "name": ".github",
        "why": "top-level area linked by the configuration graph"
      },
      "relationship_chain": [
        {
          "from": ".github/actions/aethyme-scorecard/package.json",
          "to": ".github",
          "relation": "configures"
        },
        {
          "from": ".github/actions/aethyme-scorecard/package.json",
          "to": ".github/actions/aethyme-scorecard/index.js",
          "relation": "entrypoint_for"
        }
      ],
      "rejected_candidates": [],
      "confidence": "high"
    }
  },
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": ".github",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": ".github/actions/aethyme-scorecard/package.json",
        "file": ".github/actions/aethyme-scorecard/package.json",
        "reason": "manifest config anchor (score 43)"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      ".github",
      ".github/actions/aethyme-scorecard/package.json"
    ],
    "in_scope_files": [
      ".github/actions/aethyme-scorecard/package.json"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      ".github"
    ],
    "out_of_scope": [
      ".github/actions",
      ".github/workflows",
      "docs",
      "docs/architecture",
      "docs/architecture/auth-boundary.md",
      "docs/auth-setup.md",
      "docs/getting-started",
      "docs/guides",
      "docs/reference",
      "docs/reports",
      "docs/runbooks",
      "k8s",
      "k8s/helm",
      "k8s/helm/aethyme/templates/cache/redis-deployment.yaml",
      "k8s/helm/aethyme/templates/deployment.yaml",
      "k8s/helm/aethyme/templates/secret.yaml",
      "k8s/helm/aethyme/templates/workers/indexer-deployment.yaml",
      "migrations",
      "migrations/001_initial_schema.sql",
      "migrations/002_add_rls_hardening.sql",
      "migrations/002_query_optimization_indexes.sql",
      "migrations/003_scorecard_tables.sql",
      "migrations/004_add_display_id.sql",
      "monitoring",
      "ops",
      "rust",
      "rust/crates",
      "scripts",
      "scripts/deploy/blue_green_deploy.sh",
      "scripts/deploy/canary_deploy.sh",
      "scripts/eval",
      "sdk",
      "sdk/python",
      "sdk/python/aethyme_sdk/auth.py",
      "src",
      "src/api",
      "src/auth",
      "src/auth/__init__.py",
      "src/auth/api_keys.py",
      "src/auth/middleware.py",
      "src/auth/oidc.py",
      "src/autofixers",
      "src/eval",
      "src/graph",
      "src/indexer",
      "src/indexing",
      "src/models",
      "src/scorecard",
      "tests",
      "tests/auth/__init__.py",
      "tests/auth/test_isolation.py",
      "tests/auth/test_rls.py",
      "tests/queries",
      "tests/support/auth_db.py"
    ],
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
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task anchors --repo /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme --task <task> --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli task scope --repo /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme --task <task> --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph configs /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme .github --json-output",
    "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme <anchor-id> --json-output"
  ]
}
```

## Aethyme Pack

```json
{
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": ".github",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": ".github/actions/aethyme-scorecard/package.json",
        "file": ".github/actions/aethyme-scorecard/package.json",
        "reason": "manifest config anchor (score 43)"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      ".github",
      ".github/actions/aethyme-scorecard/package.json"
    ],
    "in_scope_files": [
      ".github/actions/aethyme-scorecard/package.json"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      ".github"
    ],
    "out_of_scope": [
      ".github/actions",
      ".github/workflows",
      "docs",
      "docs/architecture",
      "docs/architecture/auth-boundary.md",
      "docs/auth-setup.md",
      "docs/getting-started",
      "docs/guides",
      "docs/reference",
      "docs/reports",
      "docs/runbooks",
      "k8s",
      "k8s/helm",
      "k8s/helm/aethyme/templates/cache/redis-deployment.yaml",
      "k8s/helm/aethyme/templates/deployment.yaml",
      "k8s/helm/aethyme/templates/secret.yaml",
      "k8s/helm/aethyme/templates/workers/indexer-deployment.yaml",
      "migrations",
      "migrations/001_initial_schema.sql",
      "migrations/002_add_rls_hardening.sql",
      "migrations/002_query_optimization_indexes.sql",
      "migrations/003_scorecard_tables.sql",
      "migrations/004_add_display_id.sql",
      "monitoring",
      "ops",
      "rust",
      "rust/crates",
      "scripts",
      "scripts/deploy/blue_green_deploy.sh",
      "scripts/deploy/canary_deploy.sh",
      "scripts/eval",
      "sdk",
      "sdk/python",
      "sdk/python/aethyme_sdk/auth.py",
      "src",
      "src/api",
      "src/auth",
      "src/auth/__init__.py",
      "src/auth/api_keys.py",
      "src/auth/middleware.py",
      "src/auth/oidc.py",
      "src/autofixers",
      "src/eval",
      "src/graph",
      "src/indexer",
      "src/indexing",
      "src/models",
      "src/scorecard",
      "tests",
      "tests/auth/__init__.py",
      "tests/auth/test_isolation.py",
      "tests/auth/test_rls.py",
      "tests/queries",
      "tests/support/auth_db.py"
    ],
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
  "task_pack": {
    "task": {
      "raw": "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "normalized": "find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "kind": "navigate_config_ownership"
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
      "files_count": 322,
      "functions_count": 809,
      "classes_count": 194,
      "docs_count": 93,
      "configs_count": 36
    },
    "signals": {
      "boundary_clarity": {
        "score": 68,
        "level": "mixed",
        "evidence": [
          "cross-area semantic edges: 739/5111",
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
          "low-confidence semantic edges: 2448/4050",
          "high-confidence semantic edges: 1239/4050",
          "cross-area semantic edges: 426/4050"
        ]
      },
      "parser_visibility": {
        "score": 91,
        "level": "strong",
        "evidence": [
          "supported source files: 150/151",
          "source files with semantic extraction: 112/151",
          "total extracted functions/classes: 1003"
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
        "id": ".github",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": ".github/actions/aethyme-scorecard/package.json",
        "file": ".github/actions/aethyme-scorecard/package.json",
        "reason": "manifest config anchor (score 43)"
      }
    ],
    "in_scope": {
      "files": [
        {
          "value": ".github/actions/aethyme-scorecard/package.json",
          "kind": "file",
          "reason": "anchor-adjacent file"
        }
      ],
      "symbols": [],
      "areas": [
        {
          "value": ".github",
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
          "value": ".github/actions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".github/workflows",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/architecture",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/architecture/auth-boundary.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "docs/auth-setup.md",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "docs/getting-started",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "docs/guides",
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
          "value": "k8s",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "k8s/helm",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "k8s/helm/aethyme/templates/cache/redis-deployment.yaml",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "k8s/helm/aethyme/templates/deployment.yaml",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "k8s/helm/aethyme/templates/secret.yaml",
          "kind": "area",
          "reason": "high-risk area: sensitive credential surface"
        },
        {
          "value": "k8s/helm/aethyme/templates/workers/indexer-deployment.yaml",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "migrations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "migrations/001_initial_schema.sql",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migrations/002_add_rls_hardening.sql",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migrations/002_query_optimization_indexes.sql",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migrations/003_scorecard_tables.sql",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "migrations/004_add_display_id.sql",
          "kind": "area",
          "reason": "high-risk area: schema change area"
        },
        {
          "value": "monitoring",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "ops",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "rust",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "rust/crates",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "scripts/deploy/blue_green_deploy.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/deploy/canary_deploy.sh",
          "kind": "area",
          "reason": "high-risk area: infrastructure surface"
        },
        {
          "value": "scripts/eval",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "sdk",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "sdk/python",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "sdk/python/aethyme_sdk/auth.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "src",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/api",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/auth",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/auth/__init__.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "src/auth/api_keys.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "src/auth/middleware.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "src/auth/oidc.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "src/autofixers",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/eval",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/graph",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/indexer",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/indexing",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/models",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "src/scorecard",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests/auth/__init__.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "tests/auth/test_isolation.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "tests/auth/test_rls.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        },
        {
          "value": "tests/queries",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tests/support/auth_db.py",
          "kind": "area",
          "reason": "high-risk area: authentication boundary"
        }
      ]
    },
    "dependencies": [
      {
        "from": ".github/actions/aethyme-scorecard/package.json",
        "to": ".github",
        "kind": "related"
      },
      {
        "from": ".github/actions/aethyme-scorecard/package.json",
        "to": ".github/actions/aethyme-scorecard/action.yml",
        "kind": "related"
      }
    ],
    "impact": [
      {
        "symbol": ".github/actions/aethyme-scorecard/action.yml",
        "file": ".github/actions/aethyme-scorecard/action.yml",
        "reason": "reverse dependency"
      },
      {
        "symbol": ".github/actions/aethyme-scorecard/index.js",
        "file": ".github/actions/aethyme-scorecard/index.js",
        "reason": "reverse dependency"
      }
    ],
    "snippets": [
      {
        "file": ".github/actions/aethyme-scorecard/package.json",
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
      ".github",
      ".github/actions/aethyme-scorecard/package.json"
    ],
    "budget": {
      "max_anchors": 3,
      "max_files": 5,
      "max_snippets": 8,
      "dependency_depth": 1,
      "impact_depth": 1
    },
    "confidence": {
      "anchor_confidence": 0.75,
      "scope_confidence": 0.7
    }
  }
}
```
