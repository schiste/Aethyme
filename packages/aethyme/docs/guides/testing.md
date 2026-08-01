# Testing Guide

Last Updated: 2026-08-01

The suite is Rust plus a temporary, dev-only pytest harness. There is no
database, no services, and no Python on the product path — `src/` was
deleted on 2026-08-01 (python-retirement Phase 6).

## Test Tiers

### Rust workspace — the real suite

```bash
cd packages/aethyme/rust
cargo test --workspace
```

Unit tests per crate plus integration tests that drive the built
binaries. Everything that ships is tested here.

### Local harness (dev-only, temporary)

`tests/local` is implementation-blind: it drives the built `aethyme`
binary as a subprocess rather than importing anything. That is why it
survived every phase of the python-retirement while the code underneath
it was replaced — it tests the contract, not the implementation. It
ports to Rust integration tests in a follow-up session and takes
`pyproject.toml` and the last Python in the package with it.

```bash
cd packages/aethyme
python3 -m venv .venv
.venv/bin/python -m pip install pytest pytest-asyncio ruff
.venv/bin/python -m pytest -q tests/local
```

Never `pip install -e .`: there is no package to install.

Engine-backed tests skip by default when the Rust engine cannot be built
in the current environment. Set `AETHYME_REQUIRE_LOCAL_ENGINE=1` to make
them fail instead of skip — a silent skip reports green while verifying
nothing, which is a known gate blind spot. CI runs both lanes in
`.github/workflows/aethyme-local-tests.yml`.

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

- generated repository builders: [`../../tests/support/repo_builders.py`](../../tests/support/repo_builders.py)
  (the seeded-DB and token helpers were removed 2026-07-13 with the
  Gen-0 PostgreSQL lineage)
- engine build-if-stale helper: [`../../tests/support/engine_binary.py`](../../tests/support/engine_binary.py)
  (formerly `src/indexing/engine.py`; dies with the harness)

## Static Analysis

```bash
# Rust
cd packages/aethyme/rust && cargo clippy --workspace --all-targets

# Style + import hygiene for the remaining test harness
cd packages/aethyme && .venv/bin/python -m ruff check .
```

`pyright` and `vulture` were dropped on 2026-08-01: both were configured
to analyze `src/`, which no longer exists. Vulture earned its place in
2026-05 by catching a 2,500-line unreachable subgraph that ruff cleared;
the equivalent question on the Rust side is answered by `cargo clippy`
and by dead-code warnings surfacing at build time.

## Documentation Rule

If a command, contract, or flow changes, update the docs in this directory and keep the docs tests green.
