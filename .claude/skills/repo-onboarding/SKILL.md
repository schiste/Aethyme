---
name: repo-onboarding
description: Use when starting work in an unfamiliar repository, when the task asks for repo overview, setup, architecture, entrypoints, test commands, or where to begin. Skip for narrow file-scoped edits once the relevant paths are already known.
---

# Repo Onboarding: Aethyme

## When to Use

- Load this skill first when the repository is unfamiliar or the request is broad.
- Recommended when: first task in repo, repo overview, setup or run instructions, architecture or entrypoints, where should I start, broad debugging or feature-localization request.
- Skip when: known file-scoped edit, follow-up inside already identified area, task already localized to concrete files.
- Use `.codex/skills/aethyme/SKILL.md` or `.claude/skills/aethyme/SKILL.md` for Aethyme's short operating contract after orientation; load its `references/` files only when needed.

## Repo Identity

- Kind: `monorepo`
- Languages: `rust, python`
- Package manager: `cargo`
- Key manifests: `packages/aethyme-eval/pyproject.toml, packages/aethyme/Makefile, packages/aethyme/package.json, packages/aethyme/rust/Cargo.toml, packages/aethyme/rust/crates/aethyme-broker/Cargo.toml, packages/aethyme/rust/crates/aethyme-cli/Cargo.toml, packages/aethyme/rust/crates/aethyme-engine/Cargo.toml, packages/aethyme/rust/crates/aethyme-enhance/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-indexer/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-schema/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-storage/Cargo.toml, packages/aethyme/rust/crates/aethyme-producers/Cargo.toml, packages/aethyme/rust/crates/aethyme-quality/Cargo.toml, packages/aethyme/rust/crates/aethyme-testkit/Cargo.toml`

## Workspaces

- `packages/aethyme/rust` (primary; cargo; manifest `packages/aethyme/rust/Cargo.toml`; high confidence)
- `packages/aethyme` (supporting; npm; manifest `packages/aethyme/package.json`; high confidence)
- `packages/aethyme-eval` (supporting; python; manifest `packages/aethyme-eval/pyproject.toml`; high confidence)

## Start Here

- `fast_test`: `cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace`
- `build`: `cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace`

## Supporting Commands

- `python -m pip install -e packages/aethyme-eval` (install; medium confidence from `packages/aethyme-eval/pyproject.toml`)
  Workspace: `packages/aethyme-eval`
- `cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace` (fast_test; high confidence from `packages/aethyme/rust/Cargo.toml`)
  Workspace: `packages/aethyme/rust`
- `python -m pytest packages/aethyme-eval` (fast_test; medium confidence from `packages/aethyme-eval/pyproject.toml`)
  Workspace: `packages/aethyme-eval`
- `cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace` (build; high confidence from `packages/aethyme/rust/Cargo.toml`)
  Workspace: `packages/aethyme/rust`

## Entrypoints

- `cli`: `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)

## Additional Entrypoints

- `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (file; role=cli; tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)
  Executable: `aethyme`
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` (file; role=cli; tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)
  Executable: `aethyme-engine-cli`
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` (file; role=cli; tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)
  Executable: `aethyme-graph-index`
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-link.rs` (file; role=cli; tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)
  Executable: `aethyme-graph-link`
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-query.rs` (file; role=cli; tracked Rust binary entrypoint in `packages/aethyme/rust`; high confidence)
  Executable: `aethyme-graph-query`

## Repo Map

- `.github` (automation; automation and CI configuration; high confidence)
- `docs` (docs; documentation area; high confidence)
- `packages` (workspace; workspace-style package container; high confidence)

## Aethyme Recipes

- `aethyme explore --repo "$PWD" --request "<task>" --format answer-json`
  Purpose: Broad repository orientation for a user request
- `aethyme repo inspect "$PWD" --mode brief --json-output`
  Purpose: Quick deterministic repo summary
- `aethyme graph callers "$PWD" "<symbol-or-file>" --json-output`
  Purpose: Trace likely impact before editing

## Generated and Dangerous Paths

- Generated/vendor `.aethyme/generated`: tracked generated or vendored surface; verify ownership before editing
- Sensitive `.aethyme/gates.toml`: repository validation policy; changes affect every broker submission
- Sensitive `.github/workflows`: repository automation; changes can affect publication or shared CI

## Freshness

- Source digest: `4a8e740640ae8599b14705fbc658e0a506b5739819fccc4e145017e6265798a9`
- Tracked source files: `536`
- Overrides applied: `False`
- Sections generated: `repo, workspaces, primary_workspace, commands, areas, entrypoints, caution_zones, generated_paths, dangerous_paths, navigation_recipes, summon, freshness`
