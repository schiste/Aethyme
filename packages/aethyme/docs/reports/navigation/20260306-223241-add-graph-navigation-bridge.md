# Graph Navigation Bridge Report

- Last Updated: 2026-03-06
- Date: 2026-03-06 22:32:41 Europe/Paris
- Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`
- Scope: graph-mediated navigation bridge for Aethyme Core

## Summary

Implemented the missing bridge between the repograph and agent-facing navigation.

The engine now exposes first-class graph and task navigation commands:
- `graph node`
- `graph children`
- `graph parents`
- `graph callers`
- `graph callees`
- `graph docs`
- `graph configs`
- `task anchors`
- `task scope`
- `task next`
- `task expand`

The immediate goal was to move from graph-backed preprocessing to graph-mediated navigation.

## Code Changes

### Rust engine
- Added `packages/aethyme/rust/crates/aethyme-engine/src/navigation.rs`
- Exported navigation module from `packages/aethyme/rust/crates/aethyme-engine/src/lib.rs`
- Extended `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs`
- Fixed serializer collision in `packages/aethyme/rust/crates/aethyme-engine/src/json.rs`
- Tightened task anchoring in `packages/aethyme/rust/crates/aethyme-engine/src/anchors.rs`

### Python adapter/CLI
- Extended `packages/aethyme/src/indexing/engine.py`
- Extended `packages/aethyme/src/cli.py`

### Tests
- Extended `packages/aethyme/tests/local/test_local_workflow.py`

## Verification

### Rust tests

```text
running 13 tests
test passes::code::tests::cross_file_python_call_resolution_links_imported_function ... ok
test navigation::tests::node_view_returns_function_metadata ... ok
test passes::docs::tests::docs_link_to_documented_files_and_functions ... ok
test passes::code::tests::rust_extraction_finds_structs_and_functions ... ok
test passes::configs::tests::cargo_manifest_links_to_rust_entrypoint ... ok
test anchors::tests::change_symbol_task_extracts_useful_symbol_token ... ok
test navigation::tests::navigation_views_expose_children_docs_and_task_scope ... ok
test map::tests::build_map_creates_graph_layers ... ok
test anchors::tests::manifest_navigation_task_prefers_config_and_area_anchors ... ok
test anchors::tests::explain_repo_prefers_structural_folder_anchors ... ok
test search::tests::symbol_search_prefers_exact_matches ... ok
test pipeline::tests::change_symbol_pack_uses_anchor_file_for_dependency_frontier ... ok
test pipeline::tests::explain_repo_pack_includes_readme_anchor ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### Python local/docs tests

```text
23 passed in 0.67s
```

## Smoke Results On ADD

### `graph node GameEngine`

```json
{
  "id": "area:ADD:GameEngine",
  "kind": "area",
  "label": "GameEngine",
  "path": "GameEngine",
  "language": null,
  "source": "structure",
  "confidence": 1000,
  "area": "GameEngine",
  "annotations": []
}
```

### `graph children GameEngine`

```json
{
  "target": "GameEngine",
  "relation": "children",
  "items": [
    {
      "id": "dir:ADD:GameEngine",
      "kind": "directory",
      "display": "GameEngine",
      "relation": "contains",
      "confidence": 1000
    }
  ]
}
```

### `graph configs GameEngine`

```json
{
  "target": "GameEngine",
  "relation": "configs",
  "items": [
    {
      "id": "config:ADD:GameEngine/godot/project.godot",
      "kind": "config",
      "display": "GameEngine/godot/project.godot",
      "relation": "configures",
      "confidence": 800
    },
    {
      "id": "config:ADD:GameEngine/godot/project.godot",
      "kind": "config",
      "display": "GameEngine/godot/project.godot",
      "relation": "entrypoint_for",
      "confidence": 800
    },
    {
      "id": "config:ADD:GameEngine/rust/addgame/Cargo.toml",
      "kind": "config",
      "display": "GameEngine/rust/addgame/Cargo.toml",
      "relation": "configures",
      "confidence": 800
    },
    {
      "id": "config:ADD:GameEngine/rust/addgame/Cargo.toml",
      "kind": "config",
      "display": "GameEngine/rust/addgame/Cargo.toml",
      "relation": "entrypoint_for",
      "confidence": 700
    }
  ]
}
```

### `task anchors`
Task:
`Find the manifest that manages the main code entrypoint in the GameEngine area`

```json
{
  "task": "Find the manifest that manages the main code entrypoint in the GameEngine area",
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
}
```

### `task scope`

```json
{
  "task": "Find the manifest that manages the main code entrypoint in the GameEngine area",
  "navigation_order": [
    "GameEngine",
    "GameEngine/godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml",
    "godot/project.godot",
    "godot/tools/osm_to_hex.py"
  ],
  "in_scope_files": [
    "GameEngine/godot/project.godot",
    "GameEngine/rust/addgame/Cargo.toml",
    "godot/project.godot",
    "godot/tools/osm_to_hex.py"
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
    "godot/tools/osm_to_hex.py::parse_osm_elements",
    "godot/tools/osm_to_hex.py::tile_to_latlon"
  ],
  "in_scope_areas": [
    "GameEngine"
  ],
  "out_of_scope": [],
  "risks": []
}
```

## Assessment

### What is now working
- The graph is no longer only used internally for one-shot packs.
- There is now a real navigation command surface over the repograph.
- Area/config-aware task anchoring is live.
- The `GameEngine` area and its manifests/configs are now exposed directly.

### Remaining issues
1. `task anchors` still emits one unrelated project config and one raw `main()` hit after the primary area/config anchors.
2. `task scope` is still too permissive and leaks into `godot/project.godot` and `godot/tools/osm_to_hex.py` instead of staying tightly inside the requested `GameEngine` slice.
3. There is still no compact navigation-specific prompt/rendering path for iterative agent use. The graph interface exists, but the prompt layer has not caught up.

## Recommended next move
1. Tighten task-scope derivation to respect matched area boundaries first.
2. Add `graph expand`-style compact graph slices for iterative agent use.
3. Update evals so the runner can call graph navigation commands iteratively instead of consuming one large prebuilt prompt.
