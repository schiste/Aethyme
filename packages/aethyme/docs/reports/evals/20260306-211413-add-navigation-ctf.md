# Eval Report: Find the manifest that manages the main code entrypoint in the GameEngine area, identify the entrypoint file it controls, and name the top-level area that owns both.

Last Updated: 2026-03-06

- Repository: `/Users/christophehenner/Downloads/Repositories/ADD`
- Generated: `2026-03-06T21:14:13.193285+00:00`

## Summary

- Baseline prompt chars: `309`
- Aethyme prompt chars: `1248`
- Navigation items: `3`
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
Repository path: /Users/christophehenner/Downloads/Repositories/ADD
Use the provided Aethyme task-context pack as the primary navigation layer.
Do not expand beyond the supplied scope unless necessary.

Start: fn:ADD:godot/tools/osm_to_hex.py:main@godot/tools/osm_to_hex.py | fn:ADD:tools/osm_to_hexmap.py:main@tools/osm_to_hexmap.py | fn:ADD:GameEngine/rust/addgame/src/hex_map.rs:handle_click@GameEngine/rust/addgame/src/hex_map.rs
Scope: GameEngine/rust/addgame/src/hex_map.rs | godot/tools/osm_to_hex.py | tools/osm_to_hexmap.py
Read: GameEngine/rust/addgame/src/hex_map.rs:1-20 | godot/tools/osm_to_hex.py:1-20 | tools/osm_to_hexmap.py:1-20
Deps: GameEngine/rust/addgame/src/hex_map.rs::handle_click->GameEngine/rust/addgame/src/hex_map.rs::draw | GameEngine/rust/addgame/src/hex_map.rs::handle_click->GameEngine/rust/addgame/src/hex_map.rs::hex_selected | GameEngine/rust/addgame/src/hex_map.rs::handle_click->GameEngine/rust/addgame/src/hex_map.rs::pixel_to_hex_static
Order: godot/tools/osm_to_hex.py -> tools/osm_to_hexmap.py -> GameEngine/rust/addgame/src/hex_map.rs
```

## Pack

```json
{
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
      "kind": "symbol",
      "id": "fn:ADD:godot/tools/osm_to_hex.py:main",
      "file": "godot/tools/osm_to_hex.py",
      "reason": "function-name-match via main"
    },
    {
      "kind": "symbol",
      "id": "fn:ADD:tools/osm_to_hexmap.py:main",
      "file": "tools/osm_to_hexmap.py",
      "reason": "function-name-match via main"
    },
    {
      "kind": "symbol",
      "id": "fn:ADD:GameEngine/rust/addgame/src/hex_map.rs:handle_click",
      "file": "GameEngine/rust/addgame/src/hex_map.rs",
      "reason": "function-name-match via and"
    }
  ],
  "in_scope": {
    "files": [
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs",
        "kind": "file",
        "reason": "anchor-adjacent file"
      },
      {
        "value": "godot/tools/osm_to_hex.py",
        "kind": "file",
        "reason": "anchor-adjacent file"
      },
      {
        "value": "tools/osm_to_hexmap.py",
        "kind": "file",
        "reason": "anchor-adjacent file"
      }
    ],
    "symbols": [
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::HexDrawData",
        "kind": "symbol",
        "reason": "class defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::HexMap",
        "kind": "symbol",
        "reason": "class defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::HexTile",
        "kind": "symbol",
        "reason": "class defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::center_on_hex",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::draw",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::draw_hex_static",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::get_selected_hex",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::get_tile_count",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::gui_input",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::handle_click",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::hex_selected",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::hex_to_pixel_static",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::init",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::load_db",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::load_from_db",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::load_tiles_from_sqlite",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::pixel_to_hex_static",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::ready",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::set_zoom",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "GameEngine/rust/addgame/src/hex_map.rs::terrain_color_static",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::add_basemap_to_meta",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::aggregate_hex_data",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::axial_round",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::classify_terrain",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::determine_dominant_terrain",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::download_basemap",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::export_to_sqlite",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::fetch_osm_data",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::generate_hex_map",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::get_way_centroid",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::haversine_distance",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::km_to_hex_axial",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::latlon_to_km",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::latlon_to_tile",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::main",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::parse_osm_elements",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "godot/tools/osm_to_hex.py::tile_to_latlon",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::UTMRef",
        "kind": "symbol",
        "reason": "class defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::_clamp",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::_cube_round",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::_simplify_points_px",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::_svg_escape",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::add_edge",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::approx_bbox_deg",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::build_basemap_svg",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::build_hex_map",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::classify_polygon",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::cube_lerp",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::cube_line",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::cube_to_pixel",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::edge_key",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::fmt_points",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::hex_distance",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::http_post",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::latlon_to_utm",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::main",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::overpass_query",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::pixel_to_cube",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::point_in_poly",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::tag_summary",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::to_px",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::utm_to_latlon",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      },
      {
        "value": "tools/osm_to_hexmap.py::utm_zone_for_lon",
        "kind": "symbol",
        "reason": "function defined in in-scope file"
      }
    ],
    "areas": []
  },
  "out_of_scope": {
    "files": [],
    "symbols": [],
    "areas": []
  },
  "dependencies": [
    {
      "from": "GameEngine/rust/addgame/src/hex_map.rs::handle_click",
      "to": "GameEngine/rust/addgame/src/hex_map.rs::draw",
      "kind": "related"
    },
    {
      "from": "GameEngine/rust/addgame/src/hex_map.rs::handle_click",
      "to": "GameEngine/rust/addgame/src/hex_map.rs::hex_selected",
      "kind": "related"
    },
    {
      "from": "GameEngine/rust/addgame/src/hex_map.rs::handle_click",
      "to": "GameEngine/rust/addgame/src/hex_map.rs::pixel_to_hex_static",
      "kind": "related"
    },
    {
      "from": "godot/tools/osm_to_hex.py::main",
      "to": "godot/tools/osm_to_hex.py::aggregate_hex_data",
      "kind": "related"
    },
    {
      "from": "godot/tools/osm_to_hex.py::main",
      "to": "godot/tools/osm_to_hex.py::download_basemap",
      "kind": "related"
    },
    {
      "from": "godot/tools/osm_to_hex.py::main",
      "to": "godot/tools/osm_to_hex.py::fetch_osm_data",
      "kind": "related"
    },
    {
      "from": "godot/tools/osm_to_hex.py::main",
      "to": "godot/tools/osm_to_hex.py::parse_osm_elements",
      "kind": "related"
    },
    {
      "from": "tools/osm_to_hexmap.py::main",
      "to": "tools/osm_to_hexmap.py::approx_bbox_deg",
      "kind": "related"
    },
    {
      "from": "tools/osm_to_hexmap.py::main",
      "to": "tools/osm_to_hexmap.py::build_hex_map",
      "kind": "related"
    },
    {
      "from": "tools/osm_to_hexmap.py::main",
      "to": "tools/osm_to_hexmap.py::latlon_to_utm",
      "kind": "related"
    },
    {
      "from": "tools/osm_to_hexmap.py::main",
      "to": "tools/osm_to_hexmap.py::overpass_query",
      "kind": "related"
    },
    {
      "from": "tools/osm_to_hexmap.py::main",
      "to": "tools/osm_to_hexmap.py::utm_zone_for_lon",
      "kind": "related"
    }
  ],
  "impact": [
    {
      "symbol": "GameEngine/rust/addgame/src/hex_map.rs",
      "file": "GameEngine/rust/addgame/src/hex_map.rs",
      "reason": "reverse dependency"
    },
    {
      "symbol": "GameEngine/rust/addgame/src/hex_map.rs::gui_input",
      "file": "GameEngine/rust/addgame/src/hex_map.rs::gui_input",
      "reason": "reverse dependency"
    },
    {
      "symbol": "documentation/crew-system.md",
      "file": "documentation/crew-system.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "documentation/general.md",
      "file": "documentation/general.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "documentation/hero-character-sheet.md",
      "file": "documentation/hero-character-sheet.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "documentation/object-types.md",
      "file": "documentation/object-types.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/scenes/ARCHITECTURE.md",
      "file": "godot/scenes/ARCHITECTURE.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/scenes/components/PANEL_INTEGRATION_GUIDE.md",
      "file": "godot/scenes/components/PANEL_INTEGRATION_GUIDE.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/scenes/components/TOOLBAR_COMPARISON.md",
      "file": "godot/scenes/components/TOOLBAR_COMPARISON.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/scenes/components/TOOLBAR_NODE_STRUCTURE.md",
      "file": "godot/scenes/components/TOOLBAR_NODE_STRUCTURE.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/scenes/components/TOOLBAR_QUICK_REFERENCE.md",
      "file": "godot/scenes/components/TOOLBAR_QUICK_REFERENCE.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "godot/tools/osm_to_hex.py",
      "file": "godot/tools/osm_to_hex.py",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/LORE_REVISION_v2.md",
      "file": "lore/LORE_REVISION_v2.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/Lore_todo.md",
      "file": "lore/Lore_todo.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/CHARACTER_TEMPLATE_PRE_SILENCE.md",
      "file": "lore/characters/CHARACTER_TEMPLATE_PRE_SILENCE.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/post_silence_traits/README.md",
      "file": "lore/characters/post_silence_traits/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/post_silence_traits/primary_role.md",
      "file": "lore/characters/post_silence_traits/primary_role.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/post_silence_traits/settlement_type.md",
      "file": "lore/characters/post_silence_traits/settlement_type.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/post_silence_traits/view_of_crystals.md",
      "file": "lore/characters/post_silence_traits/view_of_crystals.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/README.md",
      "file": "lore/characters/pre_silence_traits/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/career_stage.md",
      "file": "lore/characters/pre_silence_traits/career_stage.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/communication_preference.md",
      "file": "lore/characters/pre_silence_traits/communication_preference.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/coping_mechanisms.md",
      "file": "lore/characters/pre_silence_traits/coping_mechanisms.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/mental_health_status.md",
      "file": "lore/characters/pre_silence_traits/mental_health_status.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/noise_complaint_history.md",
      "file": "lore/characters/pre_silence_traits/noise_complaint_history.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/political_leaning.md",
      "file": "lore/characters/pre_silence_traits/political_leaning.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/register_range.md",
      "file": "lore/characters/pre_silence_traits/register_range.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/pre_silence_traits/social_style.md",
      "file": "lore/characters/pre_silence_traits/social_style.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/the_hero_origin.md",
      "file": "lore/characters/the_hero_origin.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/characters/the_unplugged/henri_marchand.md",
      "file": "lore/characters/the_unplugged/henri_marchand.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/creatures/last_humans_special_cases.md",
      "file": "lore/creatures/last_humans_special_cases.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/creatures/last_humans_years_11-20.md",
      "file": "lore/creatures/last_humans_years_11-20.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/creatures/species/ratroaches.md",
      "file": "lore/creatures/species/ratroaches.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/creatures/the_last_humans.md",
      "file": "lore/creatures/the_last_humans.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/creatures/unique/dweller_bane.md",
      "file": "lore/creatures/unique/dweller_bane.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/la_dalle_eternelle.md",
      "file": "lore/factions/paris_underground/factions/la_dalle_eternelle.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/la_gare_du_sud.md",
      "file": "lore/factions/paris_underground/factions/la_gare_du_sud.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/le_carrefour.md",
      "file": "lore/factions/paris_underground/factions/le_carrefour.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/le_desert_dore.md",
      "file": "lore/factions/paris_underground/factions/le_desert_dore.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/le_grand_nord.md",
      "file": "lore/factions/paris_underground/factions/le_grand_nord.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/le_palais_profond.md",
      "file": "lore/factions/paris_underground/factions/le_palais_profond.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/les_canaux.md",
      "file": "lore/factions/paris_underground/factions/les_canaux.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/les_montparnos.md",
      "file": "lore/factions/paris_underground/factions/les_montparnos.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/les_ossuaires.md",
      "file": "lore/factions/paris_underground/factions/les_ossuaires.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/paris_underground/factions/lest_profond.md",
      "file": "lore/factions/paris_underground/factions/lest_profond.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/channels/public_main/README.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/channels/public_main/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/channels/sounding_five/README.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/channels/sounding_five/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/2031/12_december.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/2031/12_december.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/2034/02_february.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/2034/02_february.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/README.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/channels/the_unplugged/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/private/cribrocker_x_decibella/README.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/private/cribrocker_x_decibella/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/private/naptimeinja_x_bassdrop_dad/README.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/private/naptimeinja_x_bassdrop_dad/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/sleepless_in_decibels/telegram_archives/starred_messages.md",
      "file": "lore/factions/sleepless_in_decibels/telegram_archives/starred_messages.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/the_unplugged/faction_id_card.md",
      "file": "lore/factions/the_unplugged/faction_id_card.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/the_unplugged/major_families.md",
      "file": "lore/factions/the_unplugged/major_families.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/the_unplugged/population_census.md",
      "file": "lore/factions/the_unplugged/population_census.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/the_unplugged/vinyl_collection.md",
      "file": "lore/factions/the_unplugged/vinyl_collection.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/factions/the_unplugged/vinyl_guardian.md",
      "file": "lore/factions/the_unplugged/vinyl_guardian.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/locations/touraine/README.md",
      "file": "lore/locations/touraine/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/locations/touraine/les_grottes_de_la_bresme.md",
      "file": "lore/locations/touraine/les_grottes_de_la_bresme.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/locations/touraine/studio_echo.md",
      "file": "lore/locations/touraine/studio_echo.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/naming_conventions.md",
      "file": "lore/naming_conventions.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/reference_bible.md",
      "file": "lore/reference_bible.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/the-truth.md",
      "file": "lore/the-truth.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/thesaurus.md",
      "file": "lore/thesaurus.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline.md",
      "file": "lore/timeline.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_events.md",
      "file": "lore/timeline_events.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_quiet_centuries_1-25.md",
      "file": "lore/timeline_quiet_centuries_1-25.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_quiet_centuries_151-225.md",
      "file": "lore/timeline_quiet_centuries_151-225.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_quiet_centuries_226-300.md",
      "file": "lore/timeline_quiet_centuries_226-300.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_quiet_centuries_26-75.md",
      "file": "lore/timeline_quiet_centuries_26-75.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/timeline_quiet_centuries_76-150.md",
      "file": "lore/timeline_quiet_centuries_76-150.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/books/how_to_apologize_to_your_cat.md",
      "file": "lore/worldbuilding/books/how_to_apologize_to_your_cat.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/companies_and_products/lindquist_agricultural_equipment.md",
      "file": "lore/worldbuilding/companies_and_products/lindquist_agricultural_equipment.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/food_and_brands/liquid_death_water.md",
      "file": "lore/worldbuilding/food_and_brands/liquid_death_water.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/food_and_brands/uncle_terrys_apocalypse_sauce.md",
      "file": "lore/worldbuilding/food_and_brands/uncle_terrys_apocalypse_sauce.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/geography/README.md",
      "file": "lore/worldbuilding/geography/README.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/memes_and_internet/sleepless_in_decibels.md",
      "file": "lore/worldbuilding/memes_and_internet/sleepless_in_decibels.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/memes_and_internet/the_crows_remember.md",
      "file": "lore/worldbuilding/memes_and_internet/the_crows_remember.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/movies/beef_with_the_moon.md",
      "file": "lore/worldbuilding/movies/beef_with_the_moon.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/movies/goat_lawyer.md",
      "file": "lore/worldbuilding/movies/goat_lawyer.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/movies/the_bus_that_couldnt_slow_down.md",
      "file": "lore/worldbuilding/movies/the_bus_that_couldnt_slow_down.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/music/voltage_cathedral.md",
      "file": "lore/worldbuilding/music/voltage_cathedral.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/religion_and_cults/the_assemblers.md",
      "file": "lore/worldbuilding/religion_and_cults/the_assemblers.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/religion_and_cults/the_quiet_hours.md",
      "file": "lore/worldbuilding/religion_and_cults/the_quiet_hours.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/science/the_revenge_bedtime_procrastination_study.md",
      "file": "lore/worldbuilding/science/the_revenge_bedtime_procrastination_study.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/science/the_sleep_divorce_study.md",
      "file": "lore/worldbuilding/science/the_sleep_divorce_study.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/sports/competitive_flatulence.md",
      "file": "lore/worldbuilding/sports/competitive_flatulence.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/sports/extreme_apologizing.md",
      "file": "lore/worldbuilding/sports/extreme_apologizing.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/technology/the_smart_fridge.md",
      "file": "lore/worldbuilding/technology/the_smart_fridge.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/tv_shows/divorce_court_but_for_plants.md",
      "file": "lore/worldbuilding/tv_shows/divorce_court_but_for_plants.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "lore/worldbuilding/tv_shows/naked_and_afraid_corporate_retreat.md",
      "file": "lore/worldbuilding/tv_shows/naked_and_afraid_corporate_retreat.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "mechanics/specifications.md",
      "file": "mechanics/specifications.md",
      "reason": "reverse dependency"
    },
    {
      "symbol": "tools/osm_to_hexmap.py",
      "file": "tools/osm_to_hexmap.py",
      "reason": "reverse dependency"
    }
  ],
  "snippets": [
    {
      "file": "GameEngine/rust/addgame/src/hex_map.rs",
      "start_line": 1,
      "end_line": 20,
      "kind": "definition"
    },
    {
      "file": "godot/tools/osm_to_hex.py",
      "start_line": 1,
      "end_line": 20,
      "kind": "definition"
    },
    {
      "file": "tools/osm_to_hexmap.py",
      "start_line": 1,
      "end_line": 20,
      "kind": "definition"
    }
  ],
  "risk_flags": [],
  "navigation_order": [
    "godot/tools/osm_to_hex.py",
    "tools/osm_to_hexmap.py",
    "GameEngine/rust/addgame/src/hex_map.rs"
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
