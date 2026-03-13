# Key Config Ranking Pass

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Scope

This pass refined repo-overview config ranking so direct code-entrypoint ownership outranks broad project-level configuration breadth.

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
    "GameEngine/godot/project.godot",
    "godot/project.godot"
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

- `GameEngine/rust/addgame/Cargo.toml` now ranks ahead of the broader Godot project configs.
- Ranking now favors direct code-entrypoint ownership over unbounded counts of configured files.
- The scoring is capped by evidence category, which is more general than summing every related edge.

## Remaining Product Issues

1. There are still two Godot project configs in the top three. That may be correct for `ADD`, but later we may want config deduplication by subsystem or role.
2. Repo-overview ranking is now more general, but still heuristic. It should be validated across more repo archetypes.
3. Eval reports still need Chau7-backed run records for live control versus Aethyme comparisons.
