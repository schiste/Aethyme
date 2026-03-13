# Overview And Change Navigation Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Scope

This pass completed three product-facing upgrades:

1. Split repo overview into code-bearing versus reference-bearing areas.
2. Added task-kind-specific navigation improvements for change tasks.
3. Tightened change-task scope so the agent stays inside the relevant code slice.

## Files Changed

- `packages/aethyme/rust/crates/aethyme-engine/src/context_pack.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/overview.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/json.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/task.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/anchors.rs`
- `packages/aethyme/src/cli.py`
- `packages/aethyme/src/rendering/context_pack.py`
- `packages/aethyme/src/eval/schemas.py`
- `packages/aethyme/src/eval/scoring.py`
- `packages/aethyme/src/eval/explain_repo.py`
- `packages/aethyme/tests/local/test_local_workflow.py`
- `packages/aethyme/tests/local/test_engine_cache_and_eval.py`

## Validation

- `cargo test`: passed
- `pytest tests/local tests/docs -q`: passed
- `ruff check src tests`: passed

## Real Output: Graph Overview

```json
{
  "repo": "ADD",
  "overview_docs": [
    "README.md",
    "content/README.md",
    "documentation/technical-architecture.md"
  ],
  "code_areas": [
    "tools",
    "GameEngine",
    "documentation"
  ],
  "reference_areas": [
    "lore",
    "content"
  ],
  "subareas": [
    "GameEngine/rust",
    "GameEngine/godot"
  ],
  "entrypoints": [
    "GameEngine/rust/addgame/src/lib.rs"
  ],
  "key_configs": [
    "GameEngine/rust/addgame/Cargo.toml",
    "GameEngine/godot/project.godot",
    "documentation/object-templates.json"
  ],
  "representative_code_files": [
    "godot/tools/osm_to_hex.py",
    "tools/osm_to_hexmap.py"
  ],
  "representative_docs": [
    "README.md",
    "content/README.md",
    "documentation/technical-architecture.md",
    "godot/README.md",
    "godot/scenes/ARCHITECTURE.md"
  ]
}
```

## Real Output: Change Task Scope

Task:

`Update osm_to_hex conversion flow`

```json
{
  "task": "Update osm_to_hex conversion flow",
  "navigation_order": [
    "godot/tools/osm_to_hex.py",
    "tools/osm_to_hexmap.py"
  ],
  "in_scope_files": [
    "godot/tools/osm_to_hex.py",
    "tools/osm_to_hexmap.py"
  ],
  "in_scope_symbols": [
    "godot/tools/osm_to_hex.py::add_basemap_to_meta",
    "godot/tools/osm_to_hex.py::aggregate_hex_data",
    "godot/tools/osm_to_hex.py::axial_round",
    "godot/tools/osm_to_hex.py::classify_terrain",
    "godot/tools/osm_to_hex.py::determine_dominant_terrain",
    "godot/tools/osm_to_hex.py::download_basemap",
    "godot/tools/osm_to_hex.py::export_to_sqlite",
    "godot/tools/osm_to_hex.py::fetch_osm_data",
    "godot/tools/osm_to_hex.py::generate_hex_map",
    "godot/tools/osm_to_hex.py::get_way_centroid",
    "godot/tools/osm_to_hex.py::haversine_distance",
    "godot/tools/osm_to_hex.py::km_to_hex_axial",
    "godot/tools/osm_to_hex.py::latlon_to_km",
    "godot/tools/osm_to_hex.py::latlon_to_tile",
    "godot/tools/osm_to_hex.py::main",
    "godot/tools/osm_to_hex.py::parse_osm_elements"
  ],
  "in_scope_areas": [
    "godot",
    "tools"
  ],
  "out_of_scope": [
    ".chau7",
    ".claude",
    "GameEngine",
    "GameEngine/godot",
    "GameEngine/rust",
    "content",
    "documentation",
    "godot/maps",
    "godot/scenes",
    "lore",
    "lore/characters",
    "lore/creatures",
    "lore/factions",
    "lore/locations",
    "lore/worldbuilding",
    "mechanics"
  ],
  "risks": []
}
```

## Real Output: Change Task Next

```json
{
  "target": "Update osm_to_hex conversion flow",
  "relation": "next",
  "items": [
    {
      "id": "file:ADD:godot/tools/osm_to_hex.py",
      "kind": "file",
      "display": "godot/tools/osm_to_hex.py",
      "relation": "next",
      "confidence": 1000
    },
    {
      "id": "file:ADD:tools/osm_to_hexmap.py",
      "kind": "file",
      "display": "tools/osm_to_hexmap.py",
      "relation": "next",
      "confidence": 1000
    }
  ]
}
```

## Outcome

- Repo overview is now structurally clearer.
- Change-task anchoring is now code-file first.
- Change-task scope no longer leaks into markdown documentation.
- Navigation order for the osm conversion task is tight and actionable.

## Remaining Product Issues

1. `documentation` still appears as a code area on `ADD`. This is probably not the final desired classification.
2. Change-task symbol scope is capped and cleaner now, but it is still centered on one file; cross-file symbol expansion should improve further.
3. Eval reports still need Chau7-backed run records for live control versus Aethyme comparisons.
