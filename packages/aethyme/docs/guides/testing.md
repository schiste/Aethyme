# Testing Guide

Last Updated: 2026-08-06

The suite is Rust. There is no database, no services, and no Python —
`src/` was deleted on 2026-08-01 (python-retirement Phase 6) and the dev
pytest harness followed on 2026-08-06 (Phase 7). `cargo test` is the
whole test story.

## Running It

```bash
cd packages/aethyme/rust
cargo test --workspace
```

That is the entire setup. No venv, no `pip install`, no `pyproject.toml`.

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
