# Testing Guide

Last Updated: 2026-10-07

The suite is Rust. There is no database, no services, and no Python —
`src/` was deleted on 2026-08-01 (python-retirement Phase 6) and the dev
pytest harness followed on 2026-08-06 (Phase 7). `cargo test` is the
whole test story.

## Running It

Run the suite the way CI does:

```bash
packages/aethyme/scripts/test-like-ci.sh          # what your change affects
packages/aethyme/scripts/test-like-ci.sh --full   # the whole workspace
```

The script is CI's own test step, and `ci_validation` keeps the two from
drifting. It:

- runs `cargo nextest run --profile ci`: parallel, one retry, and FLAKY reported;
- reproduces a runner: no agent process above the tests, no inherited broker
  placement, and no git identity;
- prints the ten slowest tests and the wall time.

No venv, no `pip install`, no `pyproject.toml`.

**Never run the suite serially.** `--test-threads=1`, `-j 1` and
`--no-capture` are refused. If one test fails, re-run that test by name.

## Where The Time Goes

Measured on 2026-10-07 on a 10-core workstation at load average 67, with
other agents building:

| Run | Wall time |
|---|---|
| `test-like-ci.sh --full`: 3,722 tests, nextest, parallel | 5m21s (test phase) |
| The same suite serially, `cargo test --test-threads=1` across every target | about 2 hours |
| CI, the "Rust workspace tests" job | about 8 minutes |

What makes a serial run slow is not sleeping. The slowest tests are
process-heavy and do real work: enrollment and upgrade flows that run many
`aethyme` and `git` subprocesses. The top five took 53 s, 26 s, 26 s, 22 s
and 15 s. Parallel scheduling across all test binaries overlaps them; a
serial run adds every one up.

Real-time waits are small and deliberate. The five longest tests that wait
on a clock:

| Test | Time |
|---|---|
| `gates_cli::independent_repositories_share_gate_host_resources_and_release_them` (lease renewal) | 7.5 s |
| `merge_e2e::a_first_broker_timeout_defers_the_submission_and_a_repeat_rejects_it` | 6.6 s |
| `operations_e2e::bounded_remote_write_timeout_is_journaled_unknown_and_blocks_retry` | 5.9 s |
| `operations_e2e::a_read_does_not_wait_behind_a_held_lock_or_a_blocked_repository` | 5.2 s |
| `gates_e2e::slow_gate_emits_heartbeat_progress` | 3.2 s |

Together they come to about 28 s, overlapped in a parallel run. Their
budgets were raised on purpose: `bounded_remote_write_timeout_…` uses 5 s
rather than 1 s because preparing the operation alone could exceed 1 s
under load. Shortening them would buy little and bring flakiness back, so
they stay.

### Keeping tests hermetic

Two flaky tests sent an agent into a two-hour serial run on 2026-10-07. Both
read state the test did not own:

- **`closed_worktree_gc`** inherited the operator's `AETHYME_WORKTREE_ROOT`.
  Every test repository's worktree root then landed in one shared container,
  and GC counted the other tests' roots as orphans, so the plan digest moved
  between `plan` and `apply`. The fixture now pins its own worktree root with
  `Broker::with_worktree_root`.
- **`gates_e2e`'s label test** read the gate pidfile from inside the gate
  command. The broker writes that file just after it spawns the command, so
  the read raced the write.

A test that needs a host-level location should own it: pass a temporary
`AETHYME_HOST_STATE_DIR` (canonicalized on macOS) or a `with_worktree_root`,
and never read the operator's environment.

## Test Tiers

### Unit tests, per crate

In-module `#[cfg(test)]` tests over the crate's own types. They own the
detail: detectors and fixers in `aethyme-quality`, hygiene rules in
`aethyme-enhance`, graph views in `aethyme-engine`, lifecycle in
`aethyme-broker`.

### Implementation-blind CLI suites — `aethyme-cli/tests/`

These drive the built `aethyme` binary as a subprocess and assert on
stdout, exit codes, and the files it writes. They import no product
crate. That is why the pytest versions survived every phase of the
python-retirement while the code underneath them was replaced — they test
the contract, not the implementation — and it is why they were ported
rather than rewritten.

| Suite | Subject |
|---|---|
| `enhance_cli.rs` | `enhance deploy/verify`, generated onboarding and act artifacts, experience telemetry, the deployed SessionStart hook end to end |
| `ai_ready_cli.rs` | `ai-ready` report shape, formats, exit codes |
| `autofix_cli.rs` | `autofix` dry-run/apply, risk buckets, the approval gate, protected paths |
| `explore_summary_cli.rs` | the `explore-summary` projection, byte for byte |
| `local_workflow.rs` | `repo inspect`, `task pack/anchors/scope/next/expand`, `graph node/children/overview`, `query deps/impact` over a freshly indexed repo |
| `skills_cli.rs` | `repo deploy-skills` / `compile-skills` and the ranked command/entrypoint collection |
| `skill_templates.rs` | the skill card, references, and the progressive-disclosure ladder |
| `playground_hygiene.rs` | deployed root guidance and the two playground shell scripts that grep it |
| `commit_hygiene_cli.rs` | `repo commit-message-template` / `lint-commit-message` |
| `intents_cli.rs` | the explore intent catalogue |

### Repo-hygiene suites — `aethyme-testkit/tests/`

Checks that belong to the repository rather than to any product crate:
`docs_hygiene.rs` (links, required docs, last-updated stamps, JSON
fences), `pr_template.rs` (the four contract labels and the cardinal-rule
self-check), `grammar_provenance.rs` (tree-sitter manifest shape,
licenses, and `grammar.wasm` checksums).

`grammar_provenance.rs` also carries the release gate, which is
`#[ignore]`d because it is expected to fail until every grammar records a
pinned upstream ref:

```bash
cargo test -p aethyme-testkit --test grammar_provenance -- --ignored
```

### Product path (no Python at all)

The exit criterion of the retirement is that a `cargo install` user never
needs an interpreter. `.github/workflows/oss-ci.yml` proves it in the
`product-path-no-python` job: it installs the binaries, builds a PATH
containing nothing else, asserts no `python`/`python3` is reachable, and
then runs the full product surface — enhance deploy/verify, ai-ready,
autofix, the deployed SessionStart hook, indexing, and the explore chain.

## What The Suite Proves

- repository indexing and graph navigation
- Explore, its readers (`explore-summary`, `verify-targets`), and the
  trust/observability contract
- `enhance deploy`/`verify` deployed artifact bytes
- scorecard (`ai-ready`) and `autofix` behavior, via the router
- broker lifecycle: sessions, leases, gates, merge queue, hooks

## Test Support

Everything shared lives in the `aethyme-testkit` crate
([`../../rust/crates/aethyme-testkit`](../../rust/crates/aethyme-testkit)),
a `publish = false` workspace member consumed only as a dev-dependency,
so it can never enter `cargo install`:

- `bins` — builds and resolves `aethyme`, `aethyme-engine-cli`, and
  `aethyme-graph-index`. A failed build **asserts**; it never skips.
  Environment-dependent skips are a known gate blind spot — a suite that
  quietly skips its subject looks exactly like one that passes. (The
  pytest harness had an `AETHYME_REQUIRE_LOCAL_ENGINE` opt-in for this;
  strict is now the only mode, so the flag and its second CI lane are
  gone.)
- `invoke` — runs the router with merged stdout+stderr, optional cwd and
  stdin.
- `repos` — programmatic fixture repositories, built on demand and never
  checked in (CONTRIBUTING's fixture rule).
- `paths` — the three checkout roots, resolved from `CARGO_MANIFEST_DIR`
  rather than cwd.

### The live broker database is not the suite's to touch

A test binary's working directory is its crate directory, which sits inside
a real checkout. Broker state is per-repository and resolves through
`main_root()` — the git *common* directory's parent — so from a worktree it
lands in the main checkout. A CLI spawned by a test therefore has a path to
the developer's own `.aethyme/broker.db` that nothing in the test wrote.

Two rules keep that path harmless (#163):

- **A metric never migrates.** The post-command metric hook opens the
  database only if one already exists at exactly this binary's schema
  version, and otherwise writes nothing. Before this, `cargo test
  --workspace` on a branch that added a migration moved the shared database
  ahead of every installed `aethyme` on the machine — silently, and long
  before the branch merged. Recovery was manual.
- **`AETHYME_BROKER_DB` pins the file.** Set it to an absolute path and
  every opener uses that database instead of `<repo>/.aethyme/broker.db`.
  It is used verbatim, so a relative value resolves against the process
  working directory. Unset in production; a harness that wants a database
  it owns, rather than one it merely does not corrupt, sets it.

## Static Analysis

```bash
cd packages/aethyme/rust
cargo clippy --workspace --all-targets
```

`ruff` left with the Python it linted (2026-08-06). `pyright` and
`vulture` were dropped on 2026-08-01: both were configured to analyze
`src/`. Vulture earned its place in 2026-05 by catching a 2,500-line
unreachable subgraph that ruff cleared; the equivalent question on the
Rust side is answered by `cargo clippy` and by dead-code warnings
surfacing at build time.

## The Other Package

`packages/aethyme-eval` is Python and stays that way by operator
decision: an arm's-length acceptance check should not share the measured
system's toolchain. It owns its own tests, its own venv, and its own
gate. Nothing here applies to it.

## Documentation Rule

If a command, contract, or flow changes, update the docs in this directory and keep the docs tests green.
