# Eval Report: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-06

- Repository: `/Users/christophehenner/Downloads/Repositories/ADD`
- Generated: `2026-03-06T21:59:31.598760+00:00`

## Summary

- Baseline prompt chars: `309`
- Aethyme prompt chars: `259`
- Navigation items: `5`
- Risk items: `0`

## Output Schema

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

## Scoring Rubric

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

## Reference Output

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

## Aethyme Structured Output

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

## Assessments

### Baseline Assessment

```json
null
```

### Aethyme Assessment

```json
null
```

## Explanation

```text

```

## Baseline Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.
Repository path: /Users/christophehenner/Downloads/Repositories/ADD
Explore the repository directly and produce a structured explanation.
```

## Aethyme Prompt

```text
Task: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.
Use `AETHYME_EVAL_NAVIGATION_CONTEXT_FILE`.
Return only the required structured output.
```

## Pack

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
        "id": "GameEngine/godot/project.godot",
        "file": "GameEngine/godot/project.godot",
        "reason": "project config anchor"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor"
      },
      {
        "kind": "file",
        "id": "godot/project.godot",
        "file": "godot/project.godot",
        "reason": "project config anchor"
      },
      {
        "kind": "symbol",
        "id": "fn:ADD:godot/tools/osm_to_hex.py:main",
        "file": "godot/tools/osm_to_hex.py",
        "reason": "function-name-match via main"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "GameEngine",
      "GameEngine/godot/project.godot",
      "GameEngine/rust/addgame/Cargo.toml",
      "godot/project.godot",
      "godot/tools/osm_to_hex.py"
    ],
    "in_scope_files": [
      "GameEngine/godot/project.godot",
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "GameEngine"
    ],
    "out_of_scope": [
      ".chau7",
      ".claude",
      "content",
      "documentation",
      "godot",
      "lore",
      "mechanics",
      "tools"
    ],
    "risks": []
  },
  "task_pack": {
    "task": {
      "raw": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "normalized": "find the manifest that manages the main code entrypoint in the gameengine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
      "kind": "unknown"
    },
    "overview": {
      "overview_docs": [],
      "top_areas": [],
      "entrypoints": [],
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
        "id": "GameEngine/godot/project.godot",
        "file": "GameEngine/godot/project.godot",
        "reason": "project config anchor"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor"
      }
    ],
    "in_scope": {
      "files": [
        {
          "value": "GameEngine/godot/project.godot",
          "kind": "file",
          "reason": "anchor-adjacent file"
        },
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
          "value": "lore",
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
      },
      {
        "from": "GameEngine",
        "to": "dir:ADD:GameEngine/godot",
        "kind": "related"
      },
      {
        "from": "GameEngine",
        "to": "dir:ADD:GameEngine/rust",
        "kind": "related"
      },
      {
        "from": "GameEngine/godot/project.godot",
        "to": "GameEngine",
        "kind": "related"
      },
      {
        "from": "GameEngine/godot/project.godot",
        "to": "GameEngine/godot/scenes/game.gd",
        "kind": "related"
      },
      {
        "from": "GameEngine/godot/project.godot",
        "to": "GameEngine/godot/scenes/game.tscn",
        "kind": "related"
      },
      {
        "from": "GameEngine/rust/addgame/Cargo.toml",
        "to": "GameEngine",
        "kind": "related"
      },
      {
        "from": "GameEngine/rust/addgame/Cargo.toml",
        "to": "GameEngine/rust/addgame/src/lib.rs",
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
      },
      {
        "symbol": "GameEngine/godot/bin/libaddgame.dylib",
        "file": "GameEngine/godot/bin/libaddgame.dylib",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/godot/maps/tours_region.db",
        "file": "GameEngine/godot/maps/tours_region.db",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/godot/project.godot",
        "file": "GameEngine/godot/project.godot",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/godot/scenes/game.gd",
        "file": "GameEngine/godot/scenes/game.gd",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/godot/scenes/game.tscn",
        "file": "GameEngine/godot/scenes/game.tscn",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/rust/addgame/Cargo.lock",
        "file": "GameEngine/rust/addgame/Cargo.lock",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/rust/addgame/src/hex_map.rs",
        "file": "GameEngine/rust/addgame/src/hex_map.rs",
        "reason": "reverse dependency"
      },
      {
        "symbol": "GameEngine/rust/addgame/src/lib.rs",
        "file": "GameEngine/rust/addgame/src/lib.rs",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/godot",
        "file": "dir:ADD:GameEngine/godot",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/godot/bin",
        "file": "dir:ADD:GameEngine/godot/bin",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/godot/maps",
        "file": "dir:ADD:GameEngine/godot/maps",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/godot/scenes",
        "file": "dir:ADD:GameEngine/godot/scenes",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/rust",
        "file": "dir:ADD:GameEngine/rust",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/rust/addgame",
        "file": "dir:ADD:GameEngine/rust/addgame",
        "reason": "reverse dependency"
      },
      {
        "symbol": "dir:ADD:GameEngine/rust/addgame/src",
        "file": "dir:ADD:GameEngine/rust/addgame/src",
        "reason": "reverse dependency"
      },
      {
        "symbol": "godot/README.md",
        "file": "godot/README.md",
        "reason": "reverse dependency"
      },
      {
        "symbol": "repo:ADD",
        "file": "repo:ADD",
        "reason": "reverse dependency"
      }
    ],
    "snippets": [
      {
        "file": "GameEngine/godot/project.godot",
        "start_line": 1,
        "end_line": 20,
        "kind": "overview"
      },
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
      "GameEngine/godot/project.godot",
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
        "id": "GameEngine/godot/project.godot",
        "file": "GameEngine/godot/project.godot",
        "reason": "project config anchor"
      },
      {
        "kind": "file",
        "id": "GameEngine/rust/addgame/Cargo.toml",
        "file": "GameEngine/rust/addgame/Cargo.toml",
        "reason": "manifest config anchor"
      },
      {
        "kind": "file",
        "id": "godot/project.godot",
        "file": "godot/project.godot",
        "reason": "project config anchor"
      },
      {
        "kind": "symbol",
        "id": "fn:ADD:godot/tools/osm_to_hex.py:main",
        "file": "godot/tools/osm_to_hex.py",
        "reason": "function-name-match via main"
      }
    ]
  },
  "scope": {
    "task": "Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.",
    "navigation_order": [
      "GameEngine",
      "GameEngine/godot/project.godot",
      "GameEngine/rust/addgame/Cargo.toml",
      "godot/project.godot",
      "godot/tools/osm_to_hex.py"
    ],
    "in_scope_files": [
      "GameEngine/godot/project.godot",
      "GameEngine/rust/addgame/Cargo.toml"
    ],
    "in_scope_symbols": [],
    "in_scope_areas": [
      "GameEngine"
    ],
    "out_of_scope": [
      ".chau7",
      ".claude",
      "content",
      "documentation",
      "godot",
      "lore",
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

## Challenge

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

## Verbose Results

### Baseline Run

```json
null
```

### Aethyme Run

```json
null
```
