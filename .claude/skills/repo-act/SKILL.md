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
- Before reporting an emitted setting or guard as missing, inspect its complete
  producer or template, including lines beyond the initial excerpt. A missing
  value in a selected span does not establish that the output omits it.
- Identify the nearest fast test or validation command before editing.
- For implementation and version audits, search production source and maintained
  guides before archived qualification payloads or raw logs. Frozen transcripts
  can match unrelated version strings and overwhelm source results; search them
  separately only when they are the evidence being requested.
- Before declaring a regression missing during issue triage, inspect likely
  test bodies and their assertions, not only test names. An interruption or
  recovery test may already exercise the exact state under a broader name.
- During issue-status audits, treat plan and architecture prose as a claim:
  map each acceptance criterion to current implementation and test assertions
  at the audited commit before marking it absent or complete.
- Inspect caution zones before changing generated or vendored areas.

## Validation Checklist

- When changing a retention default, inspect fixtures that inherit the default
  as well as tests with explicit overrides. Update their age assumptions and
  reference documentation; exercise retention and expiry on both sides of the
  new window. Distinguish automatic grace from explicit reviewed cleanup, and
  report live policy overrides separately from an unreleased code default.
- For graph consumer changes, include a real producer-to-materialization-to-query
  fixture alongside hand-built store tests. Verify relationship endpoints resolve
  to the expected typed nodes, and move persisted fragments to a differently
  named checkout before materialization. Overlay identities can depend on the
  current checkout name while fragment identities retain the indexing name.
  Invalidate derived caches when correcting their persisted semantic projection.
  When adding finer-grained nodes to an existing collection, audit every
  consumer's granularity: file counts, overview ranking, excerpts and file
  diagnostics must not count each key or section as another physical file.
  Add a regression comparing file-level behavior with those extra nodes removed.
- For commands protected by repository-relative artifact claims, verify that
  the named session owns the checkout and the generator runs at its root. Test
  nested-directory and wrong-worktree refusals before dispatch; a normalized
  resource key alone cannot align a child command's relative output paths.
- Git configuration values such as `remote.*.uploadpack` and `core.sshCommand`
  contain shell commands. Quote embedded executable paths for that shell,
  separately from the argv used to set the value. Exercise transport fixtures
  under paths containing spaces and apostrophes, even when CI paths are simple.
  After a quoting failure, audit sibling transport command setters before
  starting another CI or local qualification cycle.
- Cargo integration tests can replace a previously built CLI artifact. Record
  binary hashes before and after tests; matching source revisions alone do not
  prove that the bytes stayed unchanged. Qualify the exact artifacts selected
  for deployment separately from test-built outputs.
- When local compilation is deferred, verify API visibility from the test
  crate, receiver mutability, helper return types and asserted field definitions
  before adding regressions. Integration tests cannot call crate-private APIs.
  Preserve optional-value types in assertions; a compile failure supplies no
  runtime test evidence and needlessly repeats the remote validation cycle.
- For Rust compile review, trace the actual `mod` declarations, including
  `#[path]`, and item visibility from each use site; filenames alone do not
  establish module namespaces or sibling access.
- Check newly emitted next-action commands against the public CLI resolver,
  including product diagnostics, harness argv, and mirrored skills. Internal
  broker help can contain legacy spellings that are rewritten at the public
  boundary. Audit the whole changed batch after a spelling failure; fixing
  the first reported caller does not establish that other scan groups pass.
- Validate a command's complete action, arity, and required confirmation before
  opening broker state or scanning host or worktree inventories. Add a CLI
  regression that checks the structured refusal and confirms no repository or
  host state or review snapshot was created; isolate optional telemetry so it
  cannot invalidate that boundary check.
- Before committing a substantive change, use colon-terminated section headings (`Problem:`, `Decision:`, `Rationale:`, `Validation:`) and require `aethyme repo lint-commit-message <file>` to report `Valid: yes`; the linter does not recognize bare headings.
- For broker SQLite legacy-migration fixtures, restrict schema-downgrade
  helpers to disposable fixtures and refuse to drop later-schema authority
  tables that contain rows. Remove empty later-schema objects before lowering
  the version marker. Seed constraint-valid legacy authority rows in refusal
  tests, since empty authority tables can classify as fresh.
- When hardening SQLite filesystem checks on POSIX, account for process-owned
  locks: closing another descriptor for the same inode can release a live
  connection's locks, including on a refusal path. Preserve no-follow and
  metadata checks; verify lock preservation with a separate conflicting writer.
  A successful integrity check alone does not establish safe descriptor lifetime.
- Before standalone `rustfmt` or `rustc` validation, resolve the owning
  crate's Cargo edition, including workspace inheritance, and pass that edition.
  Formatting rules vary by edition even when the source compiles in both; a
  successful check with a guessed edition can fail workspace CI.
  When adding a Rust module, include its parent declaration file in formatting
  validation: formatting only the new file misses reordered `mod` declarations.
  For scoped checks, use `--config skip_children=true` on the parent; retain
  the repository's required formatting check before publishing.
- Keep latency budgets in versioned machine-readable data. Validate the warning
  policy with deterministic tests; assess elapsed time in dedicated performance
  runs rather than wall-clock assertions in ordinary tests. Match the recorded
  workload to the claimed scaling dimension: released lease history does not
  establish behavior with many sessions or worktrees. Retain complete CLI
  wall time, workload and binary identity, and raw samples. Do not equate an
  internal phase or coordinated-operation interval with end-to-end or network
  time. Use the existing non-gating performance workflow before adding builds.
- When adding non-Rust controls to a fast lane, verify that mixed and full
  changes also execute them. Test positive fast-lane and negative/full-lane
  classification cases; a green Rust suite does not establish that the
  non-Rust controls ran. Keep the lane conditions exclusive so the full path
  does not repeat the fast validator and its extra tooling. When changing CI
  workflow steps, inspect the Rust CI-contract tests alongside the shell
  classifier tests and keep both aligned.
- For broker performance fixtures, command success is not enough: assert the
  intended production profile with its recorded operation/profile witness and
  verify the exact expected effects. A successful broad fallback is a
  different scenario, not evidence for a narrow-path optimization. Validate
  semantic results and source, binary, and fixture provenance before comparing
  timings; unsupported or mismatched profiles, incomplete output, and missing
  witnesses are unavailable samples, never eligible timings.
- When investigating Git worktreeConfig migration, stage config.worktree files
  with explicit config --file paths before enabling the extension. Before enable,
  config --worktree can write shared local config in a single checkout; installed
  clients refuse it when multiple worktrees exist. Verify both cases.
  Verify primary and existing linked checkouts at each interruption checkpoint,
  including explicit core.worktree and a bare primary; keep new session settings
  isolated and distinguish quiescent byte rollback from safe live migration.
  Stage core.repositoryformatversion=1 with the extension when relying on
  unknown-extension refusal, and verify refusal plus exact-byte restoration.
  A probe with a current client does not qualify an actual unsupported client;
  test the real older executable separately, never a substituted version string.
- For Git ref-backend benchmarks, compare both loose and packed files-backend
  refs with reftable at matching counts. Record actual ref layout and shared
  object history; the backend name alone does not define a fair baseline.
- In promoted-history fixtures, separate source-content preservation,
  all-parent reachability and first-parent candidate enumeration. Promotion
  may rewrite source ancestry; verify the exact expected tree and record both
  commit sets rather than assuming a merge topology from a commit subject.
  Retain those checks with the measurement provenance.
- For startup profiling, use Git Trace2 event logs to count actual Git
  subprocess starts and group repeated argv. Stack-sample counts measure time in
  sampled frames, not invocation counts; keep instrumented traces and timings
  separate from uninstrumented benchmark samples. Enable tracing through the
  child process's `GIT_TRACE2_EVENT`, not repository-local `trace2.eventTarget`,
  which Git ignores. Require nonempty start events and a positive control that
  captures the exact Git command shape the regression intends to forbid.
- When combining Git identity queries, preserve checkout-context checks.
  `--git-common-dir` succeeds in bare repositories too; require an explicit
  true `--is-inside-work-tree` result when checkout evidence is needed. Compare
  main, nested and linked worktrees, and test bare and unavailable contexts
  before removing a separate discovery call.
- For concurrency-ordering regressions, exercise the real entrypoint and
  synchronize on an explicit test hook at the protected boundary. Assert the
  competing operation's state there; do not infer lock order from sleeps or
  timeouts, or simulate only one side of the race.
- In nonblocking readiness readers, distinguish available bytes from
  `WouldBlock`: wait only after `WouldBlock`, and drain buffered input without
  a polling sleep after every byte. Assert injected wait-callback counts for
  buffered reads, retries and expired deadlines; measure elapsed gains with a
  rebuilt pair in a separate uninstrumented Playground run.

- For bounded searches that stop on success, charge the base work and actual
  batches rather than rejecting the complete hypothetical search window.
  Exercise an early exact match whose full window exceeds the budget, and a
  partially searched window that exhausts it. Exhaustion must remain unknown,
  never a negative or a successful cleanup proof; keep independent type/mode
  checks and advance any cached search-profile version when behavior changes.

- For deadline-bounded cleanup, separate candidate discovery from the
  assertion that partial work completed. One short wall-clock budget may expire
  during discovery. Use a deterministic deadline seam to prove partial progress
  and a seeded persisted marker to test fresh-process resume; assert the marker
  before invoking the resume path.
- In a deadline-bounded loop, recheck the monotonic deadline after every
  callback or heartbeat and before accepting a result. That work can consume the
  remaining budget even when the loop checked immediately beforehand; test this
  with an injected callback that runs past the deadline.

- For whole-checkout cleanup, finish canonical grouping of every retained row
  and build complete live-path evidence before scheduling. Rotate a durable
  cursor across complete cohorts before per-cohort Git, witness and ownership
  proofs; never let a partial cohort authorize removal. Keep exact closed-set
  and fresh apply-time proofs. State that grouping remains O(history), and that
  one database or filesystem operation can exceed a wall-clock budget.

- When bounding cleanup over session history, page candidate records before
  decoding them and check the deadline before fetching another page. Keep live
  protection complete and preserve recovery authority for owners outside the
  current page. Exercise the production sweep with an expired deadline and
  invalid metadata on a later page; keep corruption fixtures within database
  constraints so they reach the intended read path. Inspect background pressure
  sampling too: a bounded foreground loop can still leave eager worker reads.

- When a managed output is also an input to generated context or freshness
  digests, update it before deriving those artifacts. Verify first deployment
  and an unchanged repeat; a second-run rewrite can reveal incorrect ordering.
- After `aethyme deploy`, review the full skill-file diff before committing:
  deployment can replace repository-specific lessons while freshness checks
  still pass. For files classified as customized by `aethyme upgrade plan`, use
  an explicit `preserve` or `merge` resolution when available; otherwise restore
  the intended content and inspect the resulting diff. Verify the deployed text
  and mirrored-skill parity; never commit a template overwrite blindly.
- Keep matching `.claude/skills` and `.codex/skills` files byte-identical when
  editing a mirrored skill; the skill-surface parity check enforces this.
- Before adding broker-owned persistent sidecars, verify each generated path
  matches `runtime_paths.rs` and the standard runtime `.gitignore` prefix. Add
  a regression from the path producer to the runtime-path classifier; keep
  private-file checks strict if an ignore mismatch is found.
- Choose focused local Cargo checks by compilation target as well as test name.
  A name filter selects cases at runtime; `--lib` still compiles the full unit-test
  target. Start with a relevant integration-test target when available, keep one
  Cargo worker, and retain the remaining unit coverage as pending qualification.
  A resource-stopped compile provides no test result.
- If CI cancels older runs on push, retain useful in-progress workspace test
  results before pushing a formatting or diagnostic follow-up. Commit the
  correction locally while waiting; cancel early when the run is no longer
  useful. Qualify the final head separately and label evidence by its source head.
  If a completed job's log is unavailable through the workflow view, fetch
  `repos/<owner>/<repo>/actions/jobs/<job-id>/logs` with `gh api` while other
  jobs continue. Capture raw output to a file and strip control sequences before
  display. If gh refuses escape bytes, use `--allow-escape-sequences` only for
  that file capture.
- For formatter failures, use the complete formatter output or, when available,
  a CI-generated patch bound to the exact head. Do not reconstruct edits from
  truncated excerpts or treat `git diff --check` as formatter validation; during
  a local-check pause, use that remote evidence and rerun remote checks on the
  corrected head.
- When reusing a CI build cache, compare the producer and consumer fingerprints,
  including toolchain, lockfiles and environment variables, then confirm the log
  shows a cache restore. Inspect the pinned action's defaults and effective
  artifact policy too: a dependency-only hit does not prove workspace crates or
  executables were restored. Record archive size and restore, compile and total
  job durations before claiming a gain; a matching shared key alone is insufficient.
- For package-manager cache growth, inspect the effective store path from the
  package manager's install metadata and compare its filesystem with the
  worktree's. A shared configured path can still cause per-worktree copies when
  the store is on another filesystem; pnpm documents this for `storeDir` and
  `packageImportMethod` in its [v10 settings](https://github.com/pnpm/pnpm.io/blob/main/versioned_docs/version-10.x/settings.md).
- When substituting a prebuilt Rust test binary for Cargo, inspect the effective
  Cargo `[env]` configuration: Cargo injects those values into test child
  processes, while direct execution does not. Replay and record relevant values
  explicitly, or classify the direct run as non-equivalent.
- Before optimizing a release workflow's version-contract test, trace its child
  binary helper: a test helper can invoke a Cargo binary build and compile a
  second debug copy before the shipping release build. When the workflow already
  produces the exact target binaries, point only that contract test at the
  profile/target output directory and fail closed if either artifact is missing.
  Preserve the version/source assertions and downstream packaging/signature
  gates; leave generic testkit fallback behavior unchanged.
- Treat full-repository static audits as validation work during a local build
  pause; route them to remote checks instead of running them locally. A prebuilt
  executable can still consume substantial CPU across bulk imports and scans.
- Smoke-test each binary against its own CLI contract. Run read-only probes
  outside a Git checkout with isolated home/state; preserve an unsupported
  command as a failed probe instead of treating it as a successful smoke check.
- For read-only GitHub status/check inspection, use `gh --repo` directly without
  opening a broker session. Isolate source research in its task worktree and
  independent reviews in their own read-only snapshot; sessions are for edits.
- A completed GitHub job can have logs while its workflow is still running.
  If `gh run view --log-failed` refuses for that reason, fetch the completed
  job's logs endpoint directly. Capture stdout, stderr and exit status; an empty
  file after an error is not an empty log. If `gh api` rejects escape sequences,
  use `--allow-escape-sequences` only into a retained raw file and strip control
  sequences before displaying a projection. In check rollups, distinguish old
  failures from refreshed attempts using the latest start time per check name;
  verify current required checks again before merging.
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
- When bounding subprocess output, audit every consumer for parsing or evidence
  use. Keep draining after the cap, expose an explicit truncation signal, and
  refuse to treat a valid-looking prefix as a complete listing or proof.
  Preserve the provider exit status and test both the cap boundary and a
  syntactically valid truncated prefix. For command-specific field protocols,
  capture one bounded sample from the exact argv and test its real framing and
  context fields alongside malformed and truncated fixtures.
- Before writing a broker PR body, use the canonical PR-template Contract
  checkbox (for example, `- [x] **none**`) so the decision is explicit and
  matches the template. The current parser also recognizes a plain
  `Contract decision: <label>` line in PR bodies and commit messages; the key
  must start the line after whitespace. If CI reports an undeclared decision,
  inspect its exact captured body and `Looked in` output before diagnosing a
  formatting problem.
  Prepare the exact body file. When local validation is allowed, run the pinned
  CLI's `broker check-contract --base <exact-base> --pr-body <file>`
  from that PR's worktree. Resolve `<exact-base>` with `git merge-base` between
  the verified target-branch SHA and the reviewed PR HEAD, matching the contract
  workflow. A two-dot comparison with the latest target tree can misclassify
  target-only changes as removals when the PR is behind. Record both SHAs and
  recompute after syncing. Verify HEAD matches the reviewed source and retain
  the result before publication. The body path and base do not select a worktree.
  During a local-check pause, use the
  PR's remote contract check instead of running it locally. Review each refusal;
  use a `Contract justification:` of at least 24 characters
  only for a verified false-positive, while preserving the actual retirement
  label. Confirm the actual verdict is `none` or every reported symbol has an
  accurate contract decision; exit status or help output alone is not evidence.
  Retain the local result before the public body write.
- For tests that create broker worktrees, configure a fixture-owned worktree root
  outside the temporary repository (for example, a sibling under the fixture
  TempDir); create that root before the first broker child and pass it explicitly
  to CLI children too. Assert the precondition at the real fixture entrypoint,
  before subprocess dispatch; configuring a path does not create its directory.
  An inherited
  `AETHYME_WORKTREE_ROOT` can otherwise place worktrees outside the fixture and
  leave stale Git pointers after teardown. The environment override is a
  container: create it before startup, leaving its per-repository child absent
  so fresh binding can create it. For storage-preparation tests
  that require broker placement ownership, use `start --task ... --json`
  to create a managed worktree. Read `worktree_placement.root`,
  canonicalize it and the returned worktree path, assert containment, and
  check `.aethyme-worktree-root.json` at the placement root. Derive cache
  paths from the returned `worktree_path`. `start --adopt` reuses an
  existing checkout and does not create that placement marker; do not
  synthesize it in the fixture. Assert paths and cleanup; avoid
  process-wide environment mutation in parallel tests. Filtering
  `std::env::vars_os()` and passing the result to `Command::envs` does not
  clear inherited `GIT_*` variables; it only adds or overrides values. For
  isolated Git subprocesses, call `env_remove` for inherited `GIT_*` names,
  preserve unrelated variables such as `PATH`, then set the fixture's intended
  author/committer values. For Git-backed fixtures that generate managed files
  before staging, set fixture-owned `HOME`, empty `GIT_CONFIG_GLOBAL` and
  `GIT_CONFIG_NOSYSTEM=1` on setup children that access Git before initial
  deployment and `git add`; a host `core.excludesFile` can hide generated files
  from `git add -A`. If a broker CLI rejects fixture-owned `GIT_CONFIG_*`
  selectors during preflight, remove them only from that CLI invocation and
  restore the same isolation inside a fixture-owned PATH `git` proxy that
  executes the pinned Git binary. Keep the proxy and PATH scoped to that
  invocation; never mutate a shared runner environment in concurrent tests.
  Do not compensate with fixture ignore rules or force-adds. For byte-for-byte
  Git fixture snapshots, disable automatic maintenance during setup with
  command-local `maintenance.auto=false` and `gc.auto=0`; an asynchronously
  removed maintenance lock can otherwise look like a product write. Preserve
  the complete snapshot assertion rather than excluding Git lock paths. If a fixture PATH
  excludes real provider binaries,
  retain required CLI dependencies: install a prevalidated Git executable beside
  the fake provider instead of removing Git and failing before provider startup.
  Treat `core.sshCommand` as shell syntax: quote fixture executable paths for
  POSIX shell and exercise a temporary root containing spaces and apostrophes.
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
  directory separately. Initialize Git before deriving paths under its metadata:
  a helper called before initialization may select a different fallback directory.
  Complete fixture branch/ref setup before opening the broker whenever
  possible: its live default database may be .aethyme/broker.db, and a later
  git add -A can commit it. Before any post-open branch switch, assert the
  database and WAL paths are absent from git ls-files; stage exact fixture
  paths so a checkout cannot unlink a live SQLite database.
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
- Recheck a long-lived controller's compatibility after acquiring a mutation
  lock; an open-time check can go stale during the wait. State the transition
  protocol separately: this check does not serialize migrations or protect
  clients that predate it. Exercise the real mutation entrypoint with a
  deterministic floor change at the lock boundary and assert data preservation.
- Before adding strict validation or recovery for persisted data, inspect every
  production writer and test its exact serialized bytes. Accept only known
  producer encodings, and bind the exact observed bytes across plan/apply when
  an operator confirmation spans invocations; test re-encoding drift and
  rollback after the write-side recheck.
- For recovery code that returns private control material, distinguish request
  identity from caller authority. Keep recovery internal until a durable journal
  and recovery lock bind ownership; public inventory must not confer token
  recovery authority. Read grant identity, state, allocations and retaining holds
  from one database snapshot.
- For recovery fixtures, exercise the production entry point that persists the
  recovery intent, assert that the intent exists, and only then simulate the
  interruption. An option that skips cleanup or a hand-seeded event does not
  test the producer-to-consumer path.
- For bounded persistent writers, bound scratch-file count across repeated
  interruptions as well as each file's bytes. Exercise the real publication
  boundary, preserve the previous record, and verify safe resume and writer
  contention. Prefer serialized fixed scratch ownership over accumulating names;
  never recover by deleting unrelated files matching a prefix.
- For storage ownership and reclamation changes, test replacement of a root and
  its origin at the same pathname, both before admission and before recording.
  A canonical path locates an object; it does not prove the object or filesystem
  is unchanged. Refusal must preserve the previous identity record and data.
- Before pruning a shared cache, prove that consumer discovery covers the
  cache's entire namespace. A bound repository root proves ownership, not all
  consumers; other repositories and same-origin clones may share cache keys.
  Without complete coverage, retain generic entries and unproven retired stores,
  while allowing independently witnessed rotation recovery under its existing
  identity and lease checks. Track recovery completion separately from consumer
  deferral so safe recovery can settle without claiming pruning completed.
- For session-worktree cleanup, treat creation provenance separately from current
  adoption records. Require a retained `Spawned` session as owner, exclude rows
  marked `Cleaned` because the path may have been reused, and recheck that row
  plus Git/root ownership at apply. A path, branch, root marker, or common Git
  directory can be recreated in the same repository; persist a creator-time
  worktree/admin-directory identity with the spawned row and compare it at
  selection and apply. Never infer missing identity for legacy or adopted
  rows. Cover spawn→close→adopt→close, adopted-only history, and same-path,
  same-branch replacements both within the same repository and from another
  repository.
- Carry the selected worktree-root layout through planning, attribution, and
  apply. A `LibraryOverride` with no `root_container` is a direct repository
  root: inventory that root itself, not its children as keyed repositories.
  Test direct-root and keyed-container layouts, preserving existing keyed
  confirmation digests. In CLI fixtures, derive the actual bound root from
  `advanced worktree-root --json`'s `preferred_root` or the parent of a
  worktree the CLI created; do not assume the configured root or
  `root_container` is the directory a command scans. Reclaim review snapshots
  are saved under the bound root. When an operation should preserve the
  binding, compare stable identity fields, not transient availability
  diagnostics.
- When building a filesystem inventory, treat directory-iteration, metadata,
  and required-measurement errors as incomplete. Do not silently skip unknown
  entries or substitute zero for a failed measurement; keep partial findings
  useful for diagnosis, but do not use them as complete evidence downstream.
  Checkpoint completed roots during long multi-root scans. A root can disappear
  after discovery: record that as incomplete and continue without discarding
  earlier totals. Exercise a missing-root control before a long live scan.
- Keep authoritative frozen plans, validation helpers, inputs and receipts on
  durable storage before execution; use temporary directories for scratch only.
  Before a long validation, choose a unique durable output directory outside
  frozen input or proposal bundles and preflight that its parent can accept
  output with an exclusive-create canary that is then removed. Refuse before
  expensive work if the preflight fails; never chmod or unfreeze prior evidence
  to make it writable. After a reboot, mark missing evidence interrupted or
  unavailable. Recorded hashes do not recover missing bytes or recreate a trust
  record. Before expensive runs, preflight serialization of the complete final
  source and provenance snapshot against receipt limits; place large inventories
  in bounded, hashed sidecars.
- Preserve the complete outer `broker advanced resources run <request> -- ...`
  invocation for supervised native checks, bound to the frozen resource request.
  The helper must refuse before resource probes or Cargo when the host lease ID
  or generation is absent or malformed, and record both
  in its result. A local Cargo lock and headroom monitor do not prove host
  admission. Exercise the missing-lease refusal without starting Cargo.
- Before an expensive run, invoke the exact frozen helper entrypoint in its
  plan and offline-control modes. Require nonempty parsed output with the
  expected identity, schedule and zero-Cargo result; exit zero alone can hide a
  missing entrypoint. Bind redundant manifest fields to the same final bytes.
- Before sealing a validation packet, require every input referenced by its
  plan, argv, or README to exist and match its pinned hash; hashing only the
  files that happen to be present can silently bless an incomplete packet.
  Recheck every required path after copying the packet to durable storage.
  Before freezing execution argv, check it against the actual helper parser
  and environment allowlist; do not pass defaults already fixed by the runner as
  caller overrides. Successful plan or intent generation alone does not prove
  that execution accepts the same argv.
- Give each validation attempt, including failed setup and guard-removal runs,
  unique log and manifest paths. Check which filenames the delegated monitor
  owns; store caller summaries under distinct names instead of overwriting or
  exclusively creating those monitor outputs. Preserve earlier outputs before retrying.
  If the helper exclusively creates its output root, preflight its parent with a
  removable canary and leave the output root absent; do not precreate it.
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
- Treat recovery archive verification and checkout removal as separate outcomes.
  A successful command can return `worktree_removed: false`; inspect its
  `cleanup_error`, preserve the archive path, and verify the exact checkout is
  absent before counting a removal. A stale-plan refusal requires fresh review,
  never a forced retry.
- For recovery-plan adapters, derive comparison rules from the pinned producer's
  authorization contract. Require the digest, preserved content, repository and
  checkout identity, policy, and reviewed archive destination to remain exact.
  Allow only explicitly documented mutable evidence fields to advance; keep
  complete original and fresh snapshots. Exercise evidence advancement and
  refusal of changed protected fields in both preflight and outcome checks.
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
- For GC plans with age-based retention, bind a UTC-day floor anchor separately
  from the exact evaluation timestamp and use it for every age-derived plan
  field or cutoff included in the digest, including gate-cache candidate age
  text and journaled cutoffs. Preserve exact-time behavior for legacy journals
  and keep real current-time liveness and revalidation checks. If a plan can
  rewrite telemetry history, exclude that plan's own metric even when output
  measurement is opted in.
- For runtime rollout and dogfooding, resolve the executable actually selected
  by the agent's inherited PATH and compare its version with the repository
  runtime pointer. Pin the verified executable for later commands and pass the
  same absolute executable plus sibling identity to delegated agents; selecting
  a repository runtime does not rewrite their PATH. Separately verify the actual
  running process image path/build and inspect only a fixed set of known roots
  (CARGO_INSTALL_ROOT/bin, CARGO_HOME/bin, HOME/.cargo/bin, HOME/.local/bin)
  plus the PATH-selected bin directory. Do not recursively scan roots or run
  arbitrary binaries found in alternate roots. A selector change affects future
  opens; a process already running keeps its loaded build. An explicit candidate
  smoke proves that candidate runs; separately verify PATH and running-process
  selection before calling rollout complete.
- After a broker start call returns truncated or ambiguous output, inspect that
  invocation before retrying. Wait on its original tool handle when available,
  then query structured broker status by task or short name. An empty filtered
  result or truncated response is not proof that no session was created; retry
  only after a complete status confirms there is no registered session.
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
- When testing configurable duration or deadline selection, put selection behind
  a pure seam and assert defaults, provider classes and explicit overrides
  without sleeping. Test real timeout cleanup separately with a short test-only
  budget; never reduce a production budget just to speed up a test.
- For repeated CI build-script runs, inspect the checkout action's local Git
  config before attributing recompilation to profiles or cache keys. Persisted
  credentials can add `includeIf.*.path` entries that a fail-closed source
  identity probe treats as uncertain. If no authenticated Git operation follows
  checkout, set `persist-credentials: false` and ratchet both every checkout
  declaration and the resulting local config.
- For routine Rust validation, reuse a matching warm target and run one Cargo
  process at a time with `--locked -j1`. Prefer a selected package/binary or
  focused test filter. On a resource-constrained target, use `debug=0` and
  `incremental=0` from the outset; when setting Cargo profile environment
  variables, use booleans such as `CARGO_PROFILE_DEV_INCREMENTAL=false` and
  `CARGO_PROFILE_TEST_INCREMENTAL=false` (`CARGO_INCREMENTAL=0` accepts a
  numeric value). Avoid changing profile settings on a warm target. Expand to
  verbose or workspace-wide runs only when explicitly needed or after a
  focused failure.
- Before a long or frozen Rust inventory, verify each selected
  `--package`/`--test` pair against the owning package's effective Cargo
  targets: the `tests/` defaults and any `[[test]]` or `autotests` settings
  in `Cargo.toml`, and bind the exact lib/bin/test binary identity. A missing
  target is a setup failure; stop before Cargo instead of treating it as a
  product failure or zero-test result.
- Treat release-signing failures as workflow failures, not automatically as
  transient. Inspect the exact failed step and the tag/release asset state
  before retrying. For a Rekor conflict, confirm the pinned signer's released
  conflict handling and security status; exercise signer, bundle conversion,
  and verification commands with fail-closed fakes while preserving identity,
  issuer, and transparency-log checks. Keep intermediate signature files out
  of the published asset directory.
- Before running `shasum -a 256 -c SHA256SUMS`, download every file named
  by that manifest and verify the complete set. A partial set is not a complete
  release check; checksum consistency does not authenticate the manifest or
  signer, so verify the signed manifest and signer identity separately.
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
- Before merging a qualified PR, inspect `gh pr checks --required` and the
  repository's current merge requirements. Distinguish queued optional
  cache-recording jobs from required checks; retain their pending status without
  imposing an extra merge gate. Investigate any failed substantive check even
  when it is optional, and never bypass a required check. After retargeting a
  stacked PR, verify required jobs actually ran for the current head and base;
  a skipped job from an edited-event filter is not passing validation. Inspect
  workflow triggers and trigger an authorized real run without changing reviewed
  source or inventing a pass.
- For partial delivery, avoid issue-closing keywords immediately before an
  issue reference, even in a negative sentence. After publishing the exact
  body, inspect `closingIssuesReferences`; unfinished issues must not be linked
  for automatic closure. Verify their state again after merging.
- Before publishing a PR body, run the repository's contract checker against
  that exact body and base. Use its literal checkbox syntax (for example,
  `- [x] **none**`), not prose such as `Contract: none`, and verify the checker
  accepts the prepared body.
- Before changing CLI advice commands, trace the public resolver, aliases and
  flag allowlist through to the handler. A handler alone may not describe the
  accepted public spelling. Exercise the emitted command through the real CLI
  entrypoint and run the deprecated-spelling caller ratchet before publication.
- Before changing a default CLI display, find existing consumer assertions
  as well as new feature tests. Include safety fences, session attribution,
  waiter/holder relationships, pending operations and help snapshots in the
  focused selection. Test bounded output with explicit omission counts; a
  smaller display must still surface the coordination state callers need.
- For human-facing previews containing repository-derived text, escape terminal
  control characters, including ESC, in content, paths and metadata. Preserve
  newline or tab only when it is intentional display structure, not in data
  fields. Apply byte caps after escaping and label raw-source hashes separately
  from the transformed preview.
- For CLI JSON stream tests, parse complete stdout as one JSON value and
  check child output on the intended stream separately. Include both stdout
  and stderr in subprocess failure assertions: a structured refusal can be
  written to stdout while stderr is empty. A report may retain
  the original command arguments, so searching its serialized text for a child
  marker cannot establish output isolation. Assert command metadata separately.
- For CLI subprocess-count regressions, trace the complete invoked path,
  including contract preflight, human-only rendering, and the operation itself.
  Derive expectations from the exact argv and fixture preconditions; use JSON
  mode when human rendering is outside the behavior under test, and retain an
  intended guard-removal failure plus a restored positive to prove the count
  detects the removed optimization.
- For a production guard that must prevent side effects, test the real entrypoint
  with a fixture command that writes a unique sentinel. Assert the typed refusal
  and sentinel absence, then remove only the call-site guard in a mutation run
  and verify that the sentinel assertion fails. A helper-only test cannot prove
  that the production path invokes the guard.
- For each deliberate guard-removal check, run the same named regression on
  unmodified source, with the deliberate break, and after byte-for-byte
  restoration. Require positive, intended runtime failure, positive; a compile
  failure or a test that also fails without the break does not validate the
  guard. Classify a negative control by the exact selected test, exit status,
  and expected failure message; a custom Rust panic need not contain the
  literal word `assertion`. A passing adjacent suite cannot replace either
  positive run.
  Freeze the intended review snapshot before running mutations, or build it
  from the intended commit and selected patch. Exclude an in-flight deliberate
  mutation from the review snapshot so it is not mistaken for proposed code.
- In Git filesystem-monitor experiments, stop and verify the monitor for each
  exact owned checkout before removing that checkout, including temporary linked
  worktrees inside timing loops. A shared repository setting can affect several
  checkouts; final cleanup of only the primary directory is insufficient. Keep
  interrupted command receipts separate from completed benchmark results.
- Before reclaiming a completed worktree, inspect ignored paths for qualification
  records; a clean Git status does not cover them. Preserve irreplaceable
  reports outside the worktree and build target, verify copied file sizes and
  hashes, and check symlink targets before cleanup. Keep regenerable build
  output separate from the evidence archive. For cache reclamation, test ordinary
  files, reserved-cache name collisions, and non-container roots separately; an
  incomplete liveness inventory must retain cache candidates. On read-only stores,
  use a non-mutating liveness snapshot and never turn ownership or observation
  errors into an empty protection set. Include a real CLI case with both an active
  session and a stored-active session whose derived status needs a transition;
  verify the active candidate stays protected and the read-only command leaves the
  transition row unchanged.
- When making a routine command observation-only, audit callers and tests that
  used it to trigger maintenance or telemetry. Exercise cleanup assertions through
  an explicit refresh or maintenance entrypoint, and separately verify polling
  leaves state unchanged. Preserve the original reclamation and protection checks.
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
