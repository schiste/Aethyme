# Repograph Navigation Refinement

Date: 2026-03-07
Last Updated: 2026-03-07
Repo under test: `/Users/christophehenner/Downloads/Repositories/ADD`

## Summary

This cycle refined Aethyme Core in three areas:

1. Repograph ranking quality for repo-overview tasks.
2. Explain-repo runtime and output consistency.
3. Docs-pass performance on doc-heavy repositories.

## Changes

### Area and config ranking

Updated `overview.rs` to rank areas and configs using semantic weight instead of mostly `belongs_to` volume.

Main effects:
- code/config/function density now outweighs raw doc volume
- hidden top-level areas stay excluded
- key configs are filtered to the selected top areas first

### Explain-repo rendering

Removed the extra engine `explain` call from the explain-repo eval path.

The explanation is now rendered deterministically from:
- `inspect_repository`
- `build_task_pack`

This keeps explain-repo tied to the same graph slice being evaluated and avoids an additional full graph build.

### Docs-pass optimization

Refactored docs linking to use token-set membership instead of repeated full-string scans across all files/configs.

Main effect:
- lower cost on documentation-heavy repos like `ADD`

## Validation

- `cargo test`: passed (`16 passed`)
- `pytest tests/local tests/docs -q`: passed (`23 passed`)
- `ruff check src tests`: passed

## Latest `graph overview` on `ADD`

```json
{
  "repo": "ADD",
  "overview_docs": [
    "README.md",
    "content/README.md",
    "documentation/technical-architecture.md"
  ],
  "top_areas": [
    "tools",
    "GameEngine",
    "documentation"
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
    "GameEngine/godot/project.godot"
  ],
  "representative_files": [
    "README.md",
    "content/README.md",
    "documentation/technical-architecture.md",
    "godot/README.md",
    "godot/scenes/ARCHITECTURE.md"
  ]
}
```

## Latest explain-repo eval on `ADD`

Report: `packages/aethyme/docs/reports/evals/20260307-073529-add-explain-repo.md`

Key metrics:
- baseline prompt chars: `161`
- Aethyme prompt chars: `111`
- navigation items: `5`

Key output characteristics:
- main areas: `GameEngine`, `documentation`, `tools`
- key configs now prioritize `GameEngine/rust/addgame/Cargo.toml`
- explanation now reports real counts instead of zeroes
- latest report: `packages/aethyme/docs/reports/evals/20260307-073831-add-explain-repo.md`

## Latest navigation-ctf eval on `ADD`

Report: `packages/aethyme/docs/reports/evals/20260307-073442-add-navigation-ctf.md`

Key metrics:
- baseline prompt chars: `309`
- Aethyme prompt chars: `259`
- navigation items: `2`

Key output characteristics:
- anchors narrowed to `GameEngine` and `GameEngine/rust/addgame/Cargo.toml`
- scope remains area-bounded
- relationship chain still resolves correctly from manifest to entrypoint and owning area

## Remaining issues

1. `documentation` still appears as the third top area in the repo overview on `ADD`.
   - This is now a product choice rather than a graph-ranking bug, but it may still need a separate "reference/docs area" presentation mode.

2. Representative files are still doc-heavy for repo-overview tasks.
   - They should likely be split into code/runtime representatives and documentation representatives.

3. The eval system is structurally ready, but real run metrics are still `null` until Chau7-backed run records are wired in.

## Recommended next steps

1. Tighten top-area selection further by preferring code-bearing areas over pure documentation areas when enough code-bearing areas exist.
2. Add a repo-overview-specific representative-area heuristic so explain-repo can distinguish:
   - code/runtime areas
   - docs/reference areas
3. Replace null run sections in eval reports with Chau7-derived run records once the integration is ready.
