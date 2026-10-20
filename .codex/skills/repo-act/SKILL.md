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

- For deadline-bounded cleanup, separate candidate discovery from the
  assertion that partial work completed. One short wall-clock budget may expire
  during discovery. Use a deterministic deadline seam to prove partial progress
  and a seeded persisted marker to test fresh-process resume; assert the marker
  before invoking the resume path.

- When a managed output is also an input to generated context or freshness
  digests, update it before deriving those artifacts. Verify first deployment
  and an unchanged repeat; a second-run rewrite can reveal incorrect ordering.
- Keep matching `.claude/skills` and `.codex/skills` files byte-identical when
  editing a mirrored skill; the skill-surface parity check enforces this.
- Choose focused local Cargo checks by compilation target as well as test name.
  A name filter selects cases at runtime; `--lib` still compiles the full unit-test
  target. Start with a relevant integration-test target when available, keep one
  Cargo worker, and retain the remaining unit coverage as pending qualification.
  A resource-stopped compile provides no test result.
- When substituting a prebuilt Rust test binary for Cargo, inspect the effective
  Cargo `[env]` configuration: Cargo injects those values into test child
  processes, while direct execution does not. Replay and record relevant values
  explicitly, or classify the direct run as non-equivalent.
- Smoke-test each binary against its own CLI contract. Run read-only probes
  outside a Git checkout with isolated home/state; preserve an unsupported
  command as a failed probe instead of treating it as a successful smoke check.
- For read-only GitHub status/check inspection, use `gh --repo` directly without
  opening a broker session. Isolate source research in its task worktree and
  independent reviews in their own read-only snapshot; sessions are for edits.
- For changed-path tree comparisons, pass each path as a separate argument; use
  NUL-delimited transport when paths need to cross a file boundary. A newline-
  joined list passed as one quoted pathspec can silently match no files. Retain
  the executed argv, exit status, stdout and stderr, and require a positive
  control that detects a known changed path before treating empty diff output
  as equality.
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
  process-wide environment mutation in parallel tests. Filtering
  `std::env::vars_os()` and passing the result to `Command::envs` does not
  clear inherited `GIT_*` variables; it only adds or overrides values. For
  isolated Git subprocesses, call `env_remove` for inherited `GIT_*` names,
  preserve unrelated variables such as `PATH`, then set the fixture's intended
  author/committer values. If a fixture PATH excludes real provider binaries,
  retain required CLI dependencies: install a prevalidated Git executable beside
  the fake provider instead of removing Git and failing before provider startup.
  For tests scoped to artifact caches or reclaim, set
  `[cleanup] auto_remove = false` explicitly and
  keep it enabled in whole-checkout cleanup tests. A real offline `origin` can
  make a clean branch legitimately eligible for whole-checkout cleanup; do not
  rely on a missing remote ref to preserve the fixture. Reproduce the exact CLI
  sequence and assert the checkout's provenance/policy preconditions before
  changing tracked fixtures.
  Supply fixture-owned host state, cache, and coordination database when opening
  the broker, before startup maintenance. Post-open setters cannot isolate work
  the constructor has already performed.
  Keep that storage owner alive for the whole fixture, including every broker
  drop/reopen; dropping one broker handle must not delete session checkouts.
  For a CLI launched from another directory or a session checkout, derive
  isolation paths from the owning repository fixture and set its working
  directory separately.
- In shell shims that branch on a child process's working directory, inspect it
  with `pwd -P` and compare it with an explicitly passed canonical fixture path.
  Rust `Command::current_dir` changes the child's directory without rewriting
  inherited `$PWD`, which can otherwise misclassify calls that operate on another
  repository.
- When exercising no-follow storage paths, canonicalize the fixture-owned
  temporary root before deriving its database and cache paths. Platform aliases
  such as macOS `/var` must not turn a valid fixture into a symlink traversal;
  keep the production no-follow check intact.
- Give background-writer tests an independent, fixture-owned stop condition
  and a finite write limit. Deliberately breaking production cleanup must not
  leave the test itself writing indefinitely after an assertion fails.
- Before mutating source or spawning children, freeze and verify the complete
  helper closure against its pins and preflight required process-control
  capabilities in the actual interpreter at every spawn entrypoint. For owned
  groups, retain the leader unreaped through bounded TERM/KILL cleanup, probe
  non-zombie members, reap once, then probe final group absence; never signal
  the group after reaping. A TERM/KILL error can be discharged only after the
  leader is reaped and a successful final probe proves group absence; EPERM
  itself never proves absence. Refuse when ownership or capabilities are unknown.
  Test early leader exits with open and closed pipes, setup and monitoring
  exceptions, and finite deadlines for every synthetic process. Original group
  absence proves only that group's absence; it says nothing about escaped or
  detached descendants.
- When verifying mutable file inputs, open with no-follow and nonblocking flags
  before checking the descriptor is a regular file. A path replaced with a FIFO
  can block an ordinary read-only open before the type check. Verify bounded,
  stable bytes through that same descriptor; include a finite FIFO refusal
  control that cannot start the expensive work.
- Before adding strict validation or recovery for persisted data, inspect every
  production writer and test its exact serialized bytes. Accept only known
  producer encodings, and bind the exact observed bytes across plan/apply when
  an operator confirmation spans invocations; test re-encoding drift and
  rollback after the write-side recheck.
- For recovery fixtures, exercise the production entry point that persists the
  recovery intent, assert that the intent exists, and only then simulate the
  interruption. An option that skips cleanup or a hand-seeded event does not
  test the producer-to-consumer path.
- For storage ownership and reclamation changes, test replacement of a root and
  its origin at the same pathname, both before admission and before recording.
  A canonical path locates an object; it does not prove the object or filesystem
  is unchanged. Refusal must preserve the previous identity record and data.
- Carry the selected worktree-root layout through planning, attribution, and
  apply. A `LibraryOverride` with no `root_container` is a direct repository
  root: inventory that root itself, not its children as keyed repositories.
  Test direct-root and keyed-container layouts, preserving existing keyed
  confirmation digests.
- Keep authoritative frozen plans, validation helpers, inputs and receipts on
  durable storage before execution; use temporary directories for scratch only.
  Before a long validation, choose a unique durable output directory outside
  frozen input or proposal bundles and preflight that its parent can accept
  output with an exclusive-create canary that is then removed. Refuse before
  expensive work if the preflight fails; never chmod or unfreeze prior evidence
  to make it writable. After a reboot, mark missing evidence interrupted or
  unavailable. Recorded hashes do not recover missing bytes or recreate a trust
  record.
- Before an expensive run, invoke the exact frozen helper entrypoint in its
  plan and offline-control modes. Require nonempty parsed output with the
  expected identity, schedule and zero-Cargo result; exit zero alone can hide a
  missing entrypoint. Bind redundant manifest fields to the same final bytes.
- Give each validation attempt, including failed setup and guard-removal runs,
  unique log and manifest paths. Check which filenames the delegated monitor
  owns; store caller summaries under distinct names instead of overwriting or
  exclusively creating those monitor outputs. Preserve earlier outputs before retrying.
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
- For guard-removal controls that mutate source in place, reject sources with
  multiple links. Before the first write, create an exclusive no-follow backup,
  fsync and read it back to verify the exact original bytes, then create and
  fsync an exclusive recovery-intent record. Sync the run directory and its
  parent when newly created. After mutation, restore exact bytes, verify the
  source hash, and refresh the owned source file's mtime before the restored
  positive Cargo run; a `cp -p` restore can otherwise reuse mutation output. If
  restoration cannot be proven, retain the lock and backup and report cleanup
  as unknown. SIGKILL or power loss may require manual recovery.
  Bound mutation readback and restoration by the larger of the exact original
  and mutant byte lengths plus one byte. Before touching source, exercise the
  actual mutation and restoration on scratch files seeded with pinned source bytes;
  equal-sized toy replacements cannot validate a size-changing mutation.
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
- For resumable GC row deletion, recheck current candidate metadata and every
  retention protection inside the same immediate database transaction that
  deletes each bounded batch. A saved journal records prior authorization but
  does not freeze current eligibility; accept missing rows for crash replay only
  after confirming they are absent. Test resume after a public state mutation
  changes a row's protection.
- For runtime rollout and dogfooding, resolve the executable actually selected
  by PATH and compare its version with the repository runtime pointer. Pin the
  verified executable for subsequent commands; the pointer alone does not prove
  that the shell is using the deployed build. Pass the same absolute executable
  and required sibling identity to delegated agents; selecting a repository
  runtime does not rewrite their inherited PATH.
- When resolving stacked merges, preserve the stronger assertions from both
  parents. Adapt tests to the merged API; do not replace explicit failure or
  completeness checks with weaker count-only assertions.
- For build-cache identity changes, start with a dependency-free Cargo fixture.
  Verify the embedded identity and Cargo freshness across an unchanged repeat,
  linked-worktree switches, and source advances. Also hold the current source
  identity fixed while adding a sibling worktree and making a sibling-only
  commit; compare Cargo's build-script rerun output. A fixed build-date shim can
  distinguish Git-admin watch invalidation from time-derived build output.
  Inspect actual Cargo build-script output when switching worktrees on a shared
  target: a stale compiled script can be newer than the current source file.
  If that happens, refresh the owned build-script input timestamps and record
  unchanged byte hashes before validation. Refresh the affected crate entrypoint
  too when required to invalidate stale compiled source. Repeat warm probes
  without refreshing inputs between runs.
  Keep production probes and oracles from changing watched inputs: Git dirty
  checks can refresh Cargo-watched index and split-index shared-index mtimes,
  even with optional locks disabled. Trace those paths before and after rather
  than inferring read-only behavior from flags; an isolated index must also
  protect its split-index backing files.
  Use the same isolated-index probes for operator status checks during pinned
  validation; an ordinary Git status command can refresh the watched index.
- For routine Rust validation, reuse a matching warm target and run one Cargo
  process at a time with `--locked -j1`. Prefer a selected package/binary or
  focused test filter. On a resource-constrained target, use `debug=0` and
  `incremental=0` from the outset; avoid changing profile settings on a warm
  target. Expand to verbose or workspace-wide runs only when explicitly needed
  or after a focused failure.
- Before a long or frozen Rust inventory, verify each selected
  `--package`/`--test` pair against the owning package's effective Cargo
  targets: the `tests/` defaults and any `[[test]]` or `autotests` settings
  in `Cargo.toml`, and bind the exact lib/bin/test binary identity. A missing
  target is a setup failure; stop before Cargo instead of treating it as a
  product failure or zero-test result.
- Bound compilation and test execution separately. With `cargo nextest run`,
  use `--build-jobs 1` for Cargo and `--test-threads 1` or `2` for nextest;
  nextest's `-j` aliases test parallelism, so never use it as the Cargo worker
  cap or combine `-j1` with `--test-threads`. Select nextest’s `local` profile.
  For the Cargo test harness use `-- --test-threads=2`. Verify the profile and
  actual argv used by the gate; a configured local profile has no effect when it
  is not selected.
- Run the fastest relevant test command first.
  Check the complete selected test names and a nonzero executed count before
  treating a focused run as evidence. Rust filters match substrings, including
  nested or suffixed module names; prefer `--exact` or explicit `--skip`
  exclusions. Require the expected names and counts, and fail on zero runs,
  missing or unexpected names, or ignored tests outside an explicit allowlist.
  Some harnesses split test-name headers from result lines, including
  interleaved output. Count ordinary and split forms, and use finite parser
  controls that reject ambiguous pairings, missing/unexpected/duplicate names,
  unapproved ignored tests, and summary mismatches.
  Rust module aliases can differ from filenames; a successful build with zero
  matching tests does not validate the change. Automated focused-test runners
  should refuse a successful zero-test result; preserve the process exit
  separately from that validation refusal.
- For changes to externally consumed CLI JSON, select the PR contract decision
  that matches the new surface. Mark added fields as `introduce` and document
  their presence, absence, and empty-value semantics with the serialized schema.
  Derive fixture field paths from the actual serde producer, including
  `flatten`, and verify the complete real CLI JSON response; do not infer wire
  nesting from Rust struct nesting.
- For CLI JSON stream tests, parse complete stdout as one JSON value and
  check child output on the intended stream separately. A report may retain
  the original command arguments, so searching its serialized text for a child
  marker cannot establish output isolation. Assert command metadata separately.
- For a production guard that must prevent side effects, test the real entrypoint
  with a fixture command that writes a unique sentinel. Assert the typed refusal
  and sentinel absence, then remove only the call-site guard in a mutation run
  and verify that the sentinel assertion fails. A helper-only test cannot prove
  that the production path invokes the guard.
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
- `focused_build`: `cargo build --locked -j1 --manifest-path packages/aethyme/rust/Cargo.toml --package <package> --bin <binary>`
- `full_workspace_build` (explicit): `cargo build --locked -j1 --manifest-path packages/aethyme/rust/Cargo.toml --workspace`

## Primary Entrypoints

- `cli`: `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)

## Likely Entrypoints

- `packages/aethyme/rust/crates/aethyme-cli/src/main.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-engine/src/bin/aethyme-engine-cli.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
- `packages/aethyme/rust/crates/aethyme-graph-indexer/src/bin/aethyme-graph-index.rs` (tracked Rust binary entrypoint in `packages/aethyme/rust`)
