---
name: repo-onboarding
description: Use when starting work in an unfamiliar repository, when the task asks for repo overview, setup, architecture, entrypoints, test commands, or where to begin. Skip for narrow file-scoped edits once the relevant paths are already known.
---

# Repo Onboarding: Aethyme

## When to Use

- Load this skill first when the repository is unfamiliar or the request is broad.
- Recommended when: first touch, broad task, touching .aethyme/gates.toml or .aethyme/config.toml, adding a package, manifest, or workflow directory.
- Skip when: single known file.
- Use `.codex/skills/aethyme/SKILL.md` or `.claude/skills/aethyme/SKILL.md` for Aethyme's short operating contract after orientation; load its `references/` files only when needed.

## Repo Identity

- Kind: `monorepo`
- Languages: `rust, python`
- Package manager: `cargo`
- Key manifests: `packages/aethyme-eval/pyproject.toml, packages/aethyme/rust/Cargo.toml, packages/aethyme/rust/crates/aethyme-broker/Cargo.toml, packages/aethyme/rust/crates/aethyme-cli/Cargo.toml, packages/aethyme/rust/crates/aethyme-engine/Cargo.toml, packages/aethyme/rust/crates/aethyme-enhance/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-indexer/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-schema/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-storage/Cargo.toml, packages/aethyme/rust/crates/aethyme-producers/Cargo.toml, packages/aethyme/rust/crates/aethyme-quality/Cargo.toml, packages/aethyme/rust/crates/aethyme-testkit/Cargo.toml`

## Workspaces

- `packages/aethyme/rust` (primary; cargo; manifest `packages/aethyme/rust/Cargo.toml`; high confidence)
- `packages/aethyme-eval` (supporting; python; manifest `packages/aethyme-eval/pyproject.toml`; high confidence)

## Start Here

- `install`: `python -m pip install -e packages/aethyme-eval`
- `fast_test`: `packages/aethyme/scripts/test-like-ci.sh`
- `full_test`: `packages/aethyme/scripts/test-like-ci.sh --full`
- `build`: `cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace`

## Supporting Commands

- `packages/aethyme/scripts/test-like-ci.sh` (fast_test; high confidence from `manual-override`)
  Workspace: `packages/aethyme/rust`
- `packages/aethyme/scripts/test-like-ci.sh --full` (full_test; high confidence from `manual-override`)
  Workspace: `packages/aethyme/rust`
- `cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace` (build; high confidence from `manual-override`)
  Workspace: `packages/aethyme/rust`
- `python -m pip install -e packages/aethyme-eval` (install; medium confidence from `manual-override`)
  Workspace: `packages/aethyme-eval`
- `python -m pytest packages/aethyme-eval` (test; medium confidence from `manual-override`)
  Workspace: `packages/aethyme-eval`

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
- `scripts` (tooling; developer tooling or scripts; high confidence)

## Aethyme Recipes

- `aethyme explore --repo "$PWD" --request "<task>" --format answer-json`
  Purpose: Broad repository orientation for a user request
- `aethyme repo inspect "$PWD" --mode brief --json-output`
  Purpose: Quick deterministic repo summary
- `aethyme graph callers "$PWD" "<symbol-or-file>" --json-output`
  Purpose: Trace likely impact before editing

## Generated and Dangerous Paths

- Generated/vendor `.aethyme/generated`: tracked generated or vendored surface; verify ownership before editing
- Generated/vendor `packages/aethyme/rust/crates/aethyme-quality/tests/fixtures/quality_surface/src/generated`: tracked generated or vendored surface; verify ownership before editing
- Sensitive `.aethyme/gates.toml`: repository validation policy; changes affect every broker submission
- Sensitive `.github/workflows`: repository automation; changes can affect publication or shared CI

## Maintainer Notes

- Validate a change with `packages/aethyme/scripts/test-like-ci.sh`: it runs the tests your diff affects exactly as CI does (nextest `--profile ci`, parallel, one retry, CI environment) and prints the slowest tests. CI runs the whole workspace on every pull request, so push and let it. Never run the suite with `--test-threads=1` or `--no-capture`: serially it took two hours on 2026-10-07 against eight minutes in CI. Re-run one failing test by name instead.
- `packages/aethyme` is 100% Rust. `python -m src.cli` was removed with no shim and fails with `No module named src`; run `aethyme <same thing>` instead. The only Python left is `packages/aethyme-eval`, which is Python by design. Do not add Python build files under `packages/aethyme` (the `Makefile` and `package.json` there were Python-era residue, removed 2026-09-29).
- Gate selection is by path trigger, and a trigger that outlives the paths it names silently disables its gate while the entry still auto-promotes. Four instances have already shipped here. `gate_policy.rs` fails on the config-only path, so a new package, manifest, or workflow directory needs its trigger added in the same change.
- `.aethyme/gates.toml` and `.aethyme/config.toml` are security-sensitive paths in `.aethyme/config.toml`'s own review rules. Narrowing a `triggers` glob disables a gate for a whole class of change and reads exactly like a tidy-up.
- Any gate or test that parses git output must export `CHAU7_CTO_OPTIM_ACTIVE=1`. Chau7's CTO git wrapper compresses `git log`/`git status`, and a gate that read it rejected a correctly-labelled entry on 2026-07-28.
- Gate commands that touch a test database or any external namespace must suffix it with `$AETHYME_TEST_DB_SUFFIX` (and `$AETHYME_GATE_WORKER_ID` where relevant). Fixed shared names are unsafe under broker load.
- Graph authority is `disabled` in `.aethyme/config.toml`, which is the intended default: `aethyme graph status` reports that posture as healthy and takes no action. A missing `.aethyme/graph` fragment store is therefore a posture, not a fault. `repo inspect`, `ingest`, and `warm` all read the graph and will fail without one; `aethyme graph refresh` materializes a local store and `aethyme deploy --repo . --with-graph` enrolls one.
- A gate failing with `resource_contention`, or a test failing only under concurrent load, is a host-capacity signal rather than a product defect. Re-run the single failing test in isolation before changing code; a `quick_test` or `gate_doctor` failure that passes alone has told you about the machine, not the change.

## Freshness

- Source digest: `401f12e0d06a9abe653901a2c76f534030b5f223c34d5f43e07f3d44eb90be31`
- Tracked source files: `1003`
- Overrides applied: `True`
- Sections generated: `repo, workspaces, primary_workspace, commands, areas, entrypoints, caution_zones, generated_paths, dangerous_paths, navigation_recipes, summon, freshness`
