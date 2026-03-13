# Area Profiling And Change Scope Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Scope

This pass implemented two improvements:

1. Refine area profiling so documentation-heavy zones stop winning `code_areas`.
2. Improve cross-file change-task expansion so multi-file code flows show up in scope and next-step navigation.

## Files Changed

- `packages/aethyme/rust/crates/aethyme-engine/src/overview.rs`
- `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- `packages/aethyme/tests/local/test_local_workflow.py`

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
    "godot"
  ],
  "reference_areas": [
    "lore",
    "documentation"
  ],
  "subareas": [
    "GameEngine/rust",
    "GameEngine/godot",
    "godot/scenes",
    "godot/maps"
  ],
  "entrypoints": [
    "GameEngine/rust/addgame/src/lib.rs"
  ],
  "key_configs": [
    "godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml",
    "GameEngine/godot/project.godot"
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
    "tools/osm_to_hexmap.py::_clamp",
    "tools/osm_to_hexmap.py::_cube_round",
    "tools/osm_to_hexmap.py::_simplify_points_px",
    "tools/osm_to_hexmap.py::_svg_escape",
    "tools/osm_to_hexmap.py::add_edge",
    "tools/osm_to_hexmap.py::approx_bbox_deg",
    "tools/osm_to_hexmap.py::build_basemap_svg",
    "tools/osm_to_hexmap.py::build_hex_map"
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

- `documentation` no longer appears in `code_areas`.
- `godot` now correctly appears as a code-bearing area on `ADD`.
- Multi-file change navigation now keeps both osm conversion files in scope.
- Symbol scope now covers both in-scope files instead of collapsing to one primary file.

## Remaining Product Issues

1. `key_configs` still puts `godot/project.godot` ahead of `Cargo.toml` in the overview. That ranking may still need refinement depending on whether runtime ownership or repo centrality should dominate.
2. `reference_areas` still includes `documentation`, which is defensible, but the distinction between reference and operational content may need another pass later.
3. Eval reports still need Chau7-backed run records for live control versus Aethyme comparisons.
