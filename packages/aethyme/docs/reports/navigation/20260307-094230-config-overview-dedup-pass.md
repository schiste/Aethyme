# Config Overview Dedup Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Scope

This pass improved repo-overview config selection so the overview returns a representative config slice rather than duplicate project configs or noisy content JSON.

## Files Changed

- `packages/aethyme/rust/crates/aethyme-engine/src/overview.rs`

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

## Outcome

- `Cargo.toml` remains first because it owns a direct code entrypoint.
- The duplicate Godot-style project config slice is now deduplicated.
- Autosave/content JSON no longer pollutes the repo overview config list.
- The resulting overview is smaller and more representative.

## Remaining Product Issues

1. The overview now returns only two key configs on `ADD`. That is acceptable for this repo, but the product should still be validated across other repo archetypes.
2. Overview ranking is improved, but still heuristic and should be cross-repo tested.
3. Eval reports still need Chau7-backed run records for live control versus Aethyme comparisons.
