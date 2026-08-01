# Contributing To Aethyme

## Development Standard

Work against the current core flow only:

1. ingest and inspect a repository (`aethyme repo ingest` / `inspect`)
2. run `explore`, graph, and task-context queries
3. run scorecard and broker workflows
4. keep tests and docs aligned with that flow

Do not add speculative status docs, sprint reports, or checked-in fake fixture repositories.

## Local Setup

```bash
cd packages/aethyme
cargo build --release --manifest-path rust/Cargo.toml
```

No database, services, **or Python** are required: `aethyme` is a single
Rust binary. To run the dev test harness as well:

```bash
python3 -m venv .venv
.venv/bin/python -m pip install pytest pytest-asyncio ruff
```

There is nothing to `pip install` from this repo: `src/` was deleted on
2026-08-01 (python-retirement Phase 6) and `packages/aethyme` ships no
Python. The venv exists only to run the dev test harness, which drives
the built `aethyme` binary as a subprocess; it retires when `tests/local`
ports to Rust.

## Run The Core Checks

```bash
cd rust && cargo test --workspace && cd ..
.venv/bin/python -m pytest -q tests/local   # dev harness
aethyme repo ingest .
```

## Documentation Rule

If behavior changes, update one of these instead of adding a new status file:

- `README.md`
- `roadmap.md`
- `docs/getting-started/quickstart.md`
- `docs/reference/cli.md`
- `tests/README.md`
