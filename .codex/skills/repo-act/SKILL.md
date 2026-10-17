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
  Supply fixture-owned host state, cache, and coordination database when opening
  the broker, before startup maintenance. Post-open setters cannot isolate work
  the constructor has already performed.
  Keep that storage owner alive for the whole fixture, including every broker
  drop/reopen; dropping one broker handle must not delete session checkouts.
- When exercising no-follow storage paths, canonicalize the fixture-owned
  temporary root before deriving its database and cache paths. Platform aliases
  such as macOS `/var` must not turn a valid fixture into a symlink traversal;
  keep the production no-follow check intact.
- Give background-writer tests an independent, fixture-owned stop condition
  and a finite write limit. Deliberately breaking production cleanup must not
  leave the test itself writing indefinitely after an assertion fails.
- For storage ownership and reclamation changes, test replacement of a root and
  its origin at the same pathname, both before admission and before recording.
  A canonical path locates an object; it does not prove the object or filesystem
  is unchanged. Refusal must preserve the previous identity record and data.
- Give each validation attempt, including failed setup and guard-removal runs,
  unique log and manifest paths. Preserve earlier outputs before retrying.
  Freeze source for each invocation and record the exact head and dirty-state
  hashes before and after it. If relevant source changes during a run, retain
  that run as diagnostic evidence and rerun the committed source.
- For disk-growth investigations, separate file-size totals, path allocation,
  and filesystem free space. Shared extents and hardlinks can make summed
  per-worktree sizes overstate physical use. Preserve scan completion status;
  never present a partial walk as full-volume accounting. Measure free space
  before and after authorized cleanup, and record concurrent writers rather
  than claiming removed file sizes are physical bytes freed.
- For runtime rollout and dogfooding, resolve the executable actually selected
  by PATH and compare its version with the repository runtime pointer. Pin the
  verified executable for subsequent commands; the pointer alone does not prove
  that the shell is using the deployed build.
- When resolving stacked merges, preserve the stronger assertions from both
  parents. Adapt tests to the merged API; do not replace explicit failure or
  completeness checks with weaker count-only assertions.
- For build-cache identity changes, check an unchanged repeat build as well as
  switching between linked worktrees and advancing a branch. Verify freshness
  from Cargo output; correct embedded identity alone does not rule out forced
  recompilation on every invocation. Use a dependency-free fixture first.
  Keep the oracle from changing watched inputs: `git describe --dirty` can
  refresh the index even with optional locks disabled. Compare source metadata
  before and after, or give the oracle a fixture-owned copy of its Git index.
- Run the fastest relevant test command first.
  Check the selected test names and a nonzero executed count before treating a
  focused run as evidence. Rust module aliases can differ from filenames; a
  successful build with zero matching tests does not validate the change.
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
