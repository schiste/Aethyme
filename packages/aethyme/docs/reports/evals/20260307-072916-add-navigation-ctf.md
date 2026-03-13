# Eval Report: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-07

- Repository: `/Users/christophehenner/Downloads/Repositories/ADD`
- Generated: `2026-03-07T07:29:16.182543+00:00`

## Summary

- Baseline prompt chars: `309`
- Aethyme prompt chars: `259`
- Navigation items: `2`
- Risk items: `0`

## Control

### Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/ADD
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
Task: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.
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

- Prompt chars delta: `-50`
- Navigation items surfaced: `2`
- Risk items surfaced: `0`

## Reference

### Output Schema

```json
{
  "type": "object",
  "required": [
    "config_target",
    "code_target",
    "management_area",
    "relationship_chain"
  ],
  "properties": {
    "config_target": {
      "type": "object",
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
    "path": "GameEngine/rust/addgame/Cargo.toml",
    "why": "manifest/config linked to the runtime entrypoint"
  },
  "code_target": {
    "path": "GameEngine/rust/addgame/src/lib.rs",
    "why": "entrypoint file linked by the configuration graph"
  },
  "management_area": {
    "name": "GameEngine",
    "why": "top-level area linked by the configuration graph"
  },
  "relationship_chain": [
    {
      "from": "GameEngine/rust/addgame/Cargo.toml",
      "to": "GameEngine",
      "relation": "configures"
    },
    {
      "from": "GameEngine/rust/addgame/Cargo.toml",
      "to": "GameEngine/rust/addgame/src/lib.rs",
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
  "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "reference_output": {
    "config_target": {
      "path": "GameEngine/rust/addgame/Cargo.toml",
      "why": "manifest/config linked to the runtime entrypoint"
    },
    "code_target": {
      "path": "GameEngine/rust/addgame/src/lib.rs",
      "why": "entrypoint file linked by the configuration graph"
    },
    "management_area": {
      "name": "GameEngine",
      "why": "top-level area linked by the configuration graph"
    },
    "relationship_chain": [
      {
        "from": "GameEngine/rust/addgame/Cargo.toml",
        "to": "GameEngine",
        "relation": "configures"
      },
      {
        "from": "GameEngine/rust/addgame/Cargo.toml",
        "to": "GameEngine/rust/addgame/src/lib.rs",
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
  "repo_path": "/Users/christophehenner/Downloads/Repositories/ADD",
  "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
  "challenge": {
    "kind": "navigation_ctf",
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "reference_output": {
      "config_target": {
        "path": "GameEngine/rust/addgame/Cargo.toml",
        "why": "manifest/config linked to the runtime entrypoint"
      },
      "code_target": {
        "path": "GameEngine/rust/addgame/src/lib.rs",
        "why": "entrypoint file linked by the configuration graph"
      },
      "management_area": {
        "name": "GameEngine",
        "why": "top-level area linked by the configuration graph"
      },
      "relationship_chain": [
        {
          "from": "GameEngine/rust/addgame/Cargo.toml",
          "to": "GameEngine",
          "relation": "configures"
        },
        {
          "from": "GameEngine/rust/addgame/Cargo.toml",
          "to": "GameEngine/rust/addgame/src/lib.rs",
          "relation": "entrypoint_for"
        }
      ],
      "rejected_candidates": [],
      "confidence": "high"
    }
  },
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": "GameEngine",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor (score 43)"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "GameEngine",
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_files": [
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "GameEngine"
    ],
    "out_of_scope": [
      ".chau7",
      ".claude",
      "GameEngine/godot",
      "GameEngine/rust",
      "content",
      "documentation",
      "godot",
      "godot/maps",
      "godot/scenes",
      "lore",
      "lore/characters",
      "lore/creatures",
      "lore/factions",
      "lore/locations",
      "lore/worldbuilding",
      "mechanics",
      "tools"
    ],
    "risks": []
  },
  "commands": [
    "python -m src.cli task anchors --repo <repo> --task <task> --json-output",
    "python -m src.cli task scope --repo <repo> --task <task> --json-output",
    "python -m src.cli graph configs <repo> 'GameEngine' --json-output",
    "python -m src.cli graph expand <repo> <anchor-id> --json-output"
  ]
}
```

## Aethyme Pack

```json
{
  "anchors": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "anchors": [
      {
        "kind": "folder",
        "id": "GameEngine",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor (score 43)"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "GameEngine",
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_files": [
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "GameEngine"
    ],
    "out_of_scope": [
      ".chau7",
      ".claude",
      "GameEngine/godot",
      "GameEngine/rust",
      "content",
      "documentation",
      "godot",
      "godot/maps",
      "godot/scenes",
      "lore",
      "lore/characters",
      "lore/creatures",
      "lore/factions",
      "lore/locations",
      "lore/worldbuilding",
      "mechanics",
      "tools"
    ],
    "risks": []
  },
  "task_pack": {
    "task": {
      "raw": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "normalized": "find the manifest that manages the main code entrypoint in the gameengine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "kind": "navigate_config_ownership"
    },
    "overview": {
      "overview_docs": [],
      "top_areas": [],
      "subareas": [],
      "entrypoints": [],
      "key_configs": [],
      "representative_files": []
    },
    "anchors": [
      {
        "kind": "folder",
        "id": "GameEngine",
        "file": null,
        "reason": "area match"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor (score 43)"
      }
    ],
    "in_scope": {
      "files": [
        {
          "value": "GameEngine/rust/addgame/Cargo.toml",
          "kind": "file",
          "reason": "anchor-adjacent file"
        }
      ],
      "symbols": [],
      "areas": [
        {
          "value": "GameEngine",
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
          "value": ".chau7",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": ".claude",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "GameEngine/godot",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "GameEngine/rust",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "content",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "documentation",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "godot",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "godot/maps",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "godot/scenes",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore/characters",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore/creatures",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore/factions",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore/locations",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "lore/worldbuilding",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "mechanics",
          "kind": "area",
          "reason": "outside the matched primary area"
        },
        {
          "value": "tools",
          "kind": "area",
          "reason": "outside the matched primary area"
        }
      ]
    },
    "dependencies": [
      {
        "from": "GameEngine",
        "to": "GameEngine/build.sh",
        "kind": "related"
      }
    ],
    "impact": [
      {
        "symbol": "GameEngine/build.sh",
        "file": "GameEngine/build.sh",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/godot/addgame.gdextension",
        "file": "GameEngine/godot/addgame.gdextension",
        "reason": "reverse dependency"
      }
    ],
    "snippets": [
      {
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "start_line": 1,
        "end_line": 12,
        "kind": "overview"
      }
    ],
    "risk_flags": [],
    "navigation_order": [
      "GameEngine",
      "GameEngine/rust/addgame/Cargo.toml"
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
