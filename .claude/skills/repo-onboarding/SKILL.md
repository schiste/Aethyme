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
- Languages: `rust, python, typescript, javascript`
- Package manager: `cargo`
- Key manifests: `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/H01/a-state/package.json, packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/H02/a-state/package.json, packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P01/a-state/package.json, packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P02/a-state/package.json, packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P03/a-state/package.json, packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P04/a-state/package.json, packages/aethyme-eval/benchmarks/performance/fixture/package.json, packages/aethyme-eval/benchmarks/performance/fixture/pyproject.toml, packages/aethyme-eval/benchmarks/performance/fixture/src/rust/Cargo.toml, packages/aethyme-eval/pyproject.toml, packages/aethyme/rust/Cargo.toml, packages/aethyme/rust/crates/aethyme-broker/Cargo.toml, packages/aethyme/rust/crates/aethyme-cli/Cargo.toml, packages/aethyme/rust/crates/aethyme-config/Cargo.toml, packages/aethyme/rust/crates/aethyme-contracts/Cargo.toml, packages/aethyme/rust/crates/aethyme-engine/Cargo.toml, packages/aethyme/rust/crates/aethyme-enhance/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-indexer/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-schema/Cargo.toml, packages/aethyme/rust/crates/aethyme-graph-storage/Cargo.toml, packages/aethyme/rust/crates/aethyme-producers/Cargo.toml, packages/aethyme/rust/crates/aethyme-quality/Cargo.toml, packages/aethyme/rust/crates/aethyme-testkit/Cargo.toml, packages/aethyme/rust/vendor/libsqlite3-sys-0.30.1/Cargo.toml`

## Workspaces

- `packages/aethyme/rust` (primary; cargo; manifest `packages/aethyme/rust/Cargo.toml`; high confidence)
- `packages/aethyme-eval` (supporting; python; manifest `packages/aethyme-eval/pyproject.toml`; high confidence)
- `packages/aethyme-eval/benchmarks/performance/fixture` (supporting; python; manifest `packages/aethyme-eval/benchmarks/performance/fixture/pyproject.toml`; high confidence)
- `packages/aethyme-eval/benchmarks/performance/fixture` (supporting; npm; manifest `packages/aethyme-eval/benchmarks/performance/fixture/package.json`; high confidence)
- `packages/aethyme-eval/benchmarks/performance/fixture/src/rust` (supporting; cargo; manifest `packages/aethyme-eval/benchmarks/performance/fixture/src/rust/Cargo.toml`; high confidence)
- `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P01/a-state` (supporting; npm; manifest `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P01/a-state/package.json`; high confidence)
- `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P02/a-state` (supporting; npm; manifest `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/P02/a-state/package.json`; high confidence)
- `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/H01/a-state` (supporting; npm; manifest `packages/aethyme-eval/benchmarks/local-v3/fx08/v14-frozen/tasks/H01/a-state/package.json`; high confidence)

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
- `target` (generated_or_vendor; generated output or vendored dependencies; high confidence)

## Aethyme Recipes

- `aethyme explore --repo "$PWD" --request "<task>" --format answer-json`
  Purpose: Broad repository orientation for a user request
- `aethyme repo inspect "$PWD" --mode brief --json-output`
  Purpose: Quick deterministic repo summary
- `aethyme graph callers "$PWD" "<symbol-or-file>" --json-output`
  Purpose: Trace likely impact before editing

## Caution Zones

- `target` (likely generated, vendored, fixture, or migration-heavy area)

## Generated and Dangerous Paths

- Generated/vendor `.aethyme/generated`: tracked generated or vendored surface; verify ownership before editing
- Generated/vendor `packages/aethyme/rust/crates/aethyme-quality/tests/fixtures/quality_surface/src/generated`: tracked generated or vendored surface; verify ownership before editing
- Generated/vendor `packages/aethyme/rust/vendor`: tracked generated or vendored surface; verify ownership before editing
- Generated/vendor `target`: tracked generated or vendored surface; verify ownership before editing
- Sensitive `.aethyme/gates.toml`: repository validation policy; changes affect every broker submission
- Sensitive `.github/workflows`: repository automation; changes can affect publication or shared CI

## Maintainer Notes

- `packages/aethyme/scripts/test-like-ci.sh` defaults to changed crates with the bounded local profile (one Cargo build job, two nextest workers, no retries); `--full` selects the full workspace and CI profile, including retries. Investigate contention when failures appear only under concurrency, and rerun the failing test in isolation before attributing cause; a passing isolated run does not rule out a race. Report interrupted test runs accurately.
- `packages/aethyme` is 100% Rust; `python -m src.cli` has no shim, and `packages/aethyme-eval` remains Python by design. Do not add Python build files under `packages/aethyme`. `rust-toolchain.toml` pins local, gate, and CI builds; it wins over an action's `toolchain: stable`, so install targets and components on the pinned toolchain separately.
- Keep gate triggers aligned with the paths they cover; stale globs can silently disable a gate, so add triggers when adding packages, manifests, or workflow directories. Treat `.aethyme/gates.toml` and `.aethyme/config.toml` as security-sensitive. Graph authority `disabled` is the intended default; a missing fragment store is healthy in that posture, though graph-dependent commands fail until local refresh.
- Tests that parse Git output should export `CHAU7_CTO_OPTIM_ACTIVE=1` because the host wrapper can compress `git log`/`git status`. Tests touching a shared database or external namespace must use `$AETHYME_TEST_DB_SUFFIX` and `$AETHYME_GATE_WORKER_ID` where relevant.
- Before a host-wide rollout, inventory router and engine binaries on agent `PATH`, repo-local selectors, and maintenance-service runtime pins. Verify both identities at every active entry point, and retain old immutable bundles until existing clients drain.
- Storage audits must account for the primary checkout's shared Git directory as well as linked/session worktrees. Report objects and packs, conflict-resolution cache, worktree administration, and recovery history separately from dependencies and local broker state/logs. Worktree cleanup does not compact shared Git data; keep unknown sizes unknown and distinguish logical bytes from unique physical allocation. On macOS, a remount can change `st_dev` or `fsid`; diagnose retained-root mismatches by comparing the exact failed fields with the recorded original root generation, inode/path, and stable volume IDs. For new roots, persist stable volume UUIDs; never synthesize a missing UUID into a legacy ownership record or weaken its identity checks—legacy recovery requires a reviewed archive. Scope cleanup quiescence to the target repository root and database, its lifecycle lock, and workers detached from that repository; a GC process for an unrelated repository is not a blocker.
- Before hosted qualification of native filesystem changes, verify each platform-specific FFI field/type against the dependency version pinned in `Cargo.lock`. C headers and Linux builds do not establish Rust macOS declarations. Inspect production code, including inactive `cfg` branches, for discarded results such as `let _ =`; fix call sites rather than raising discard baselines.
- Before repeating a Cargo command after unexpected wrapper behavior, inspect inherited `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, and `CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER` plus applicable ancestor `.cargo/config.toml` files and `$CARGO_HOME/config.toml` (commonly `~/.cargo/config.toml`). Removing `RUSTC_WRAPPER` alone does not disable `[build].rustc-wrapper` from config. Cargo documents `RUSTC_WRAPPER='' cargo ...` as a command-local override that disables that wrapper. Do not edit global Cargo config or stop shared cache servers for one task. For Cargo or other tool behavior, consult the pinned docs/source or a metadata-only probe, and distinguish that evidence from build/test results.

## Freshness

- Source digest: `53f17d326cc138aed4fb474a5aef064b696555a0a6d711e1196a315ab55680fc`
- Tracked source files: `3912`
- Overrides applied: `True`
- Sections generated: `repo, workspaces, primary_workspace, commands, areas, entrypoints, caution_zones, generated_paths, dangerous_paths, navigation_recipes, summon, freshness`
