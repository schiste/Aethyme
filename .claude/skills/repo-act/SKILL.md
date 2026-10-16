---
name: repo-act
description: Use after repo-onboarding or Explore when moving from orientation into execution planning, debugging checklists, and validation steps. Skip when only broad repo orientation is needed.
---

# Repo Act: Aethyme

## When to Use

- Load this after repo-onboarding when the next step is execution or validation planning.
- Skip it for broad repo overview questions with no action plan yet.

## Debugging Checklist

- Load repo-onboarding first if the repository or area is unfamiliar.
- Run Aethyme Explore on the user task before broad manual search.
- Verify answer candidates before concluding when trust_policy is not answer-safe.
- Identify the nearest fast test or validation command before editing.
- Inspect caution zones before changing generated or vendored areas.

## Validation Checklist

- Keep matching `.claude/skills` and `.codex/skills` files byte-identical when
  editing a mirrored skill; the skill-surface parity check enforces this.
- For tests that create broker worktrees, configure a fixture-owned worktree root
  outside the temporary repository (for example, a sibling under the fixture
  TempDir); pass it explicitly to CLI children too. An inherited
  `AETHYME_WORKTREE_ROOT` can otherwise place worktrees outside the fixture and
  leave stale Git pointers after teardown. Assert paths and cleanup; avoid
  process-wide environment mutation in parallel tests.
- Isolate `AETHYME_HOST_STATE_DIR` and `AETHYME_HOST_CACHE_DIR` for CLI children
  too: a temporary worktree root alone does not isolate startup cache cleanup
  or the shared resource database. Library startup needs a process-owned host
  state/cache capsule before opening a broker; later builder overrides are
  too late to isolate that startup work.
- Run the fastest relevant test command first.
- For each deliberate guard-removal check, run the same named regression on
  unmodified source, with the deliberate break, and after byte-for-byte
  restoration. Require positive, intended runtime failure, positive; a compile
  failure or a test that also fails without the break does not validate the
  guard. A passing adjacent suite cannot replace either positive run.
- Before reclaiming a completed worktree, inspect ignored paths for qualification
  records; a clean Git status does not cover them. Preserve irreplaceable
  reports outside the worktree and build target, verify copied file sizes and
  hashes, and check symlink targets before cleanup. Keep regenerable build
  output separate from the evidence archive.
- Check likely entrypoints and impacted callers before finishing.
- Re-run lint/build only after the focused validation passes.
- Document any override-based maintainer note that affected the change plan.

## Useful Commands

- `fast_test`: `packages/aethyme/scripts/test-like-ci.sh`
- `full_test`: `packages/aethyme/scripts/test-like-ci.sh --full`
- `build`: `cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace`

## Primary Entrypoints

- `cli`: `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)

## Likely Entrypoints

- `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
