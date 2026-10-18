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
- Smoke-test each binary against its own CLI contract. Run read-only probes
  outside a Git checkout with isolated home/state; preserve an unsupported
  command as a failed probe instead of treating it as a successful smoke check.
- For read-only GitHub status/check inspection, use `gh --repo` directly without
  opening a broker session. Isolate source research in its task worktree and
  independent reviews in their own read-only snapshot; sessions are for edits.
- For routine broker status checks, prefer `aethyme broker status --summary
  --json`; capture full `status --json` only when its additional detail is needed.
  Retain complete stdout and stderr before parsing exact session IDs and
  structured advice fields such as severity and commands. Never infer absence
  from truncated output or suppressed stderr.
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
- Treat frozen source and review artifacts as immutable. When source changes
  after an artifact is bound to a published hash, keep the earlier artifact
  intact and create a new version bound to the new exact hashes.
- For disk-growth investigations, separate file-size totals, path allocation,
  and filesystem free space. Shared extents and hardlinks can make summed
  per-worktree sizes overstate physical use. Preserve scan completion status;
  never present a partial walk as full-volume accounting. Measure free space
  before and after authorized cleanup, and record concurrent writers rather
  than claiming removed file sizes are physical bytes freed.
- For broker worktree storage audits, retain bounded per-worktree records with
  exact repository identity, path, and first-registration time alongside
  aggregates. Join cleanup evidence by repository identity and exact path;
  session IDs are only unique within a repository. Keep reported file bytes,
  summed path allocation, deduplicated physical allocation, and observed
  volume-free-space change distinct. Registration time does not establish
  worktree creation or preparation time.
- For guard-removal controls that temporarily mutate source, restore exact bytes
  and verify the source hash, then refresh the owned source file's mtime before
  the restored positive Cargo run. A `cp -p` restore can otherwise let Cargo
  reuse output from the mutation build.
- When recording cleanup results, validate the pinned progress record and exact
  removed set before updating progress. Hold the progress lock through an atomic
  replacement, directory sync, and readback; publish the verification proof only
  after that commit, bound to its digest. If proof publication then fails,
  reconcile the committed record instead of repeating the deletion.
- For cleanup plan/apply workflows, capture the environment from the actual
  planning and apply subprocesses; do not reconstruct it later. Resolve
  `AETHYME_WORKTREE_ROOT` as the parent container plus the broker's repository
  ID, and require the resolved broker root to match the plan root before apply.
  If they differ, stop and create a fresh plan from the captured environment.
- For runtime rollout and dogfooding, resolve the executable actually selected
  by PATH and compare its version with the repository runtime pointer. Pin the
  verified executable for subsequent commands; the pointer alone does not prove
  that the shell is using the deployed build.
- When resolving stacked merges, preserve the stronger assertions from both
  parents. Adapt tests to the merged API; do not replace explicit failure or
  completeness checks with weaker count-only assertions.
- For build-cache identity changes, start with a dependency-free Cargo fixture.
  Verify the embedded identity and Cargo freshness across an unchanged repeat,
  linked-worktree switches, and source advances. Also hold the current source
  identity fixed while adding a sibling worktree and making a sibling-only
  commit; compare Cargo's build-script rerun output. A fixed build-date shim can
  distinguish Git-admin watch invalidation from time-derived build output.
  Keep production probes and oracles from changing watched inputs: Git dirty
  checks can refresh Cargo-watched index and split-index shared-index mtimes,
  even with optional locks disabled. Trace those paths before and after rather
  than inferring read-only behavior from flags; an isolated index must also
  protect its split-index backing files.
- For routine Rust validation, reuse a matching warm target and run one Cargo
  process at a time with `--locked -j1`. Prefer a selected package/binary or
  focused test filter. On a resource-constrained target, use `debug=0` and
  `incremental=0` from the outset; avoid changing profile settings on a warm
  target. Expand to verbose or workspace-wide runs only when explicitly needed
  or after a focused failure.
- Run the fastest relevant test command first.
  Check the selected test names and a nonzero executed count before treating a
  focused run as evidence. Rust module aliases can differ from filenames; a
  successful build with zero matching tests does not validate the change.
  Automated focused-test runners should refuse a successful zero-test result;
  preserve the process exit separately from that validation refusal.
- Check likely entrypoints and impacted callers before finishing.
- Re-run lint/build only after the focused validation passes.
- Document any override-based maintainer note that affected the change plan.

## Useful Commands

- `fast_test`: `packages/aethyme/scripts/test-like-ci.sh`
- `full_test`: `packages/aethyme/scripts/test-like-ci.sh --full`
- `focused_build`: `cargo build --locked -j1 --manifest-path packages/aethyme/rust/Cargo.toml --package <package> --bin <binary>`
- `full_workspace_build` (explicit): `cargo build --locked -j1 --manifest-path packages/aethyme/rust/Cargo.toml --workspace`

## Primary Entrypoints

- `cli`: `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)

## Likely Entrypoints

- `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
