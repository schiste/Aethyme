# Eval Report: Explain this repo

## Meta

- Date: 2026-03-14
- Repository: `/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme`
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
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme
Explore the repository and produce a structured explanation.
```

### Explore

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme
Explore the repository and produce a structured explanation.
```

### Leverage

```text
Task: Explain this repo
Repository path: /Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme
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
Languages: javascript, php, python
Top-level directories: .phan, cache, docs, extensions, images, includes, languages, maintenance, mw-config, resources, skins, sql, tests
Files indexed: 12498
Functions indexed: 30116
Classes indexed: 3058
Docs indexed: 92
Configs indexed: 28
README: README.md

Code areas:
- includes
- resources

Reference areas:
- extensions
- skins

Key subareas:
- includes/Api
- includes/libs
- resources/lib
- resources/src

Entrypoints:
- .svgo.config.js
- tests/jest/jest.config.js
- tests/selenium/wdio-mediawiki/index.js

Representative code:
- maintenance/language/zhtable/Makefile.py

Representative docs:
- README.md
- docs/licenses/README.md
- includes/ExternalStore/README.md

Navigation order:
- .phan/stubs/README
- docs/Events.md
- .phan
- includes
- maintenance/generateSitemap.php

---

## Raw Data

### Reference Output

```json
{
  "repo_summary": "Task: Explain this repo",
  "code_areas": [
    "includes",
    "resources"
  ],
  "reference_areas": [
    "extensions",
    "skins"
  ],
  "entrypoints": [
    ".svgo.config.js",
    "tests/jest/jest.config.js",
    "tests/selenium/wdio-mediawiki/index.js"
  ],
  "important_docs": [
    "README.md",
    "docs/licenses/README.md",
    "includes/ExternalStore/README.md"
  ],
  "key_configs": [],
  "key_languages": [
    "javascript",
    "php",
    "python"
  ],
  "high_risk_areas": [],
  "navigation_order": [
    "README.md",
    "docs/licenses/README.md",
    "includes",
    "resources",
    "extensions"
  ],
  "representative_code_files": [
    "maintenance/language/zhtable/Makefile.py"
  ],
  "representative_docs": [
    "README.md",
    "docs/licenses/README.md",
    "includes/ExternalStore/README.md"
  ],
  "evidence": [
    "maintenance/language/zhtable/Makefile.py",
    "README.md"
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
        "php",
        "python"
      ],
      "top_level_dirs": [
        ".phan",
        "cache",
        "docs",
        "extensions",
        "images",
        "includes",
        "languages",
        "maintenance",
        "mw-config",
        "resources",
        "skins",
        "sql",
        "tests"
      ],
      "readme_path": ".phan/stubs/README"
    },
    "files_count": 12498,
    "functions_count": 30116,
    "classes_count": 3058,
    "docs_count": 92,
    "configs_count": 28
  },
  "signals": {
    "boundary_clarity": {
      "score": 54,
      "level": "weak",
      "evidence": [
        "cross-area semantic edges: 1782327/3767132",
        "source files with area assignment: 7175/7186",
        "generic source file names: 1"
      ]
    },
    "entrypoint_clarity": {
      "score": 100,
      "level": "strong",
      "evidence": [
        "direct code entrypoint edges: 7",
        "configs with entrypoints: 2",
        "areas with ambiguous entrypoints: 0"
      ]
    },
    "config_hygiene": {
      "score": 53,
      "level": "weak",
      "evidence": [
        "operational configs: 4",
        "linked configs: 4/4",
        "duplicate config families: 1"
      ]
    },
    "hidden_coupling": {
      "score": 20,
      "level": "weak",
      "evidence": [
        "low-confidence semantic edges: 3140530/3751984",
        "high-confidence semantic edges: 565273/3751984",
        "cross-area semantic edges: 1782186/3751984"
      ]
    },
    "parser_visibility": {
      "score": 74,
      "level": "mixed",
      "evidence": [
        "supported source files: 6372/7186",
        "source files with semantic extraction: 3166/7186",
        "total extracted functions/classes: 33174"
      ]
    }
  },
  "overview": {
    "overview_docs": [
      "README.md",
      "docs/licenses/README.md",
      "includes/ExternalStore/README.md"
    ],
    "code_areas": [
      "includes",
      "resources"
    ],
    "reference_areas": [
      "extensions",
      "skins"
    ],
    "subareas": [
      "includes/Api",
      "includes/libs",
      "resources/lib",
      "resources/src"
    ],
    "entrypoints": [
      ".svgo.config.js",
      "tests/jest/jest.config.js",
      "tests/selenium/wdio-mediawiki/index.js"
    ],
    "key_configs": [],
    "representative_code_files": [
      "maintenance/language/zhtable/Makefile.py"
    ],
    "representative_docs": [
      "README.md",
      "docs/licenses/README.md",
      "includes/ExternalStore/README.md",
      "includes/FileBackend/README.md",
      "includes/FileRepo/README.md"
    ]
  },
  "anchors": [
    {
      "kind": "file",
      "id": ".phan/stubs/README",
      "file": ".phan/stubs/README",
      "reason": "repository readme"
    },
    {
      "kind": "file",
      "id": "docs/Events.md",
      "file": "docs/Events.md",
      "reason": "documentation document"
    },
    {
      "kind": "folder",
      "id": ".phan",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "folder",
      "id": "includes",
      "file": null,
      "reason": "top-level area"
    },
    {
      "kind": "file",
      "id": "maintenance/generateSitemap.php",
      "file": "maintenance/generateSitemap.php",
      "reason": "likely entrypoint"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": ".phan/stubs/README",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [],
    "areas": [
      {
        "value": ".phan",
        "kind": "area",
        "reason": "primary top-level area"
      },
      {
        "value": "includes",
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
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Actions",
      "kind": "contains"
    },
    {
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Api",
      "kind": "contains"
    },
    {
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Auth",
      "kind": "contains"
    },
    {
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Autoload",
      "kind": "contains"
    },
    {
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Block",
      "kind": "contains"
    },
    {
      "from": "dir:Mediawiki - Aethyme:includes",
      "to": "dir:Mediawiki - Aethyme:includes/Cache",
      "kind": "contains"
    }
  ],
  "impact": [
    {
      "symbol": ".svgo.config.js",
      "file": ".svgo.config.js",
      "reason": "entrypoint candidate"
    },
    {
      "symbol": "tests/selenium/wdio-mediawiki/index.js",
      "file": "tests/selenium/wdio-mediawiki/index.js",
      "reason": "entrypoint candidate"
    }
  ],
  "snippets": [
    {
      "file": ".phan/stubs/README",
      "start_line": 1,
      "end_line": 3,
      "kind": "overview"
    },
    {
      "file": "docs/Events.md",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    },
    {
      "file": "maintenance/generateSitemap.php",
      "start_line": 1,
      "end_line": 20,
      "kind": "overview"
    }
  ],
  "file_contents": [],
  "risk_flags": [],
  "navigation_order": [
    ".phan/stubs/README",
    "docs/Events.md",
    ".phan",
    "includes",
    "maintenance/generateSitemap.php"
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
    "activated_node_count": 44565,
    "max_depth_reached": 3,
    "top_activated": [
      {
        "id": "dir:Mediawiki - Aethyme:tests/phpunit/data/FindDeprecated/includes",
        "activation": 1.0
      },
      {
        "id": "dir:Mediawiki - Aethyme:tests/phpunit/integration/includes",
        "activation": 1.0
      },
      {
        "id": "dir:Mediawiki - Aethyme:.phan",
        "activation": 1.0
      },
      {
        "id": "doc:Mediawiki - Aethyme:.phan/stubs/README",
        "activation": 1.0
      },
      {
        "id": "area:Mediawiki - Aethyme:includes",
        "activation": 1.0
      },
      {
        "id": "file:Mediawiki - Aethyme:.phan/stubs/README",
        "activation": 1.0
      },
      {
        "id": "area:Mediawiki - Aethyme:.phan",
        "activation": 1.0
      },
      {
        "id": "dir:Mediawiki - Aethyme:maintenance/includes",
        "activation": 1.0
      },
      {
        "id": "dir:Mediawiki - Aethyme:tests/phpunit/unit/includes",
        "activation": 1.0
      },
      {
        "id": "fn:Mediawiki - Aethyme:resources/lib/vue/vue.global.js:includes",
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
  "repo_path": "/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme",
  "tool_repo_path": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme",
  "tool_python": "/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python",
  "task": "Explain this repo",
  "anchors": {
    "task": "Explain this repo",
    "anchors": [
      {
        "kind": "file",
        "id": ".phan/stubs/README",
        "file": ".phan/stubs/README",
        "reason": "repository readme"
      },
      {
        "kind": "file",
        "id": "docs/Events.md",
        "file": "docs/Events.md",
        "reason": "documentation document"
      },
      {
        "kind": "folder",
        "id": ".phan",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "folder",
        "id": "includes",
        "file": null,
        "reason": "top-level area"
      },
      {
        "kind": "file",
        "id": "maintenance/generateSitemap.php",
        "file": "maintenance/generateSitemap.php",
        "reason": "likely entrypoint"
      }
    ]
  },
  "scope": {
    "task": "Explain this repo",
    "navigation_order": [
      ".phan/stubs/README",
      "docs/Events.md",
      ".phan",
      "includes",
      "maintenance/generateSitemap.php"
    ],
    "in_scope_files": [
      ".phan/stubs/README"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      ".phan",
      "includes"
    ],
    "out_of_scope": [],
    "risks": []
  },
  "commands": [
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli repo inspect '/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph overview '/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme' --json-output",
    "cd /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme && /Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/.venv/bin/python -m src.cli graph expand '/Users/christophehenner/Downloads/Repositories/Playground/Mediawiki/Mediawiki - Aethyme' <anchor-id> --json-output"
  ]
}
```

### Repo Signals

```json
{
  "boundary_clarity": {
    "score": 54,
    "level": "weak",
    "evidence": [
      "cross-area semantic edges: 1782327/3767132",
      "source files with area assignment: 7175/7186",
      "generic source file names: 1"
    ]
  },
  "entrypoint_clarity": {
    "score": 100,
    "level": "strong",
    "evidence": [
      "direct code entrypoint edges: 7",
      "configs with entrypoints: 2",
      "areas with ambiguous entrypoints: 0"
    ]
  },
  "config_hygiene": {
    "score": 53,
    "level": "weak",
    "evidence": [
      "operational configs: 4",
      "linked configs: 4/4",
      "duplicate config families: 1"
    ]
  },
  "hidden_coupling": {
    "score": 20,
    "level": "weak",
    "evidence": [
      "low-confidence semantic edges: 3140530/3751984",
      "high-confidence semantic edges: 565273/3751984",
      "cross-area semantic edges: 1782186/3751984"
    ]
  },
  "parser_visibility": {
    "score": 74,
    "level": "mixed",
    "evidence": [
      "supported source files: 6372/7186",
      "source files with semantic extraction: 3166/7186",
      "total extracted functions/classes: 33174"
    ]
  }
}
```

