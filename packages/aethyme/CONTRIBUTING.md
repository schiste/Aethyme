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
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
cargo build --release --manifest-path rust/Cargo.toml
```

No database or services are required; the CLI drives the built Rust
engine binary directly.

## Run The Core Checks

```bash
. .venv/bin/activate
pytest tests/local tests/scorecard -q
python -m src.cli repo ingest .
```

## Documentation Rule

If behavior changes, update one of these instead of adding a new status file:

- `README.md`
- `roadmap.md`
- `docs/getting-started/quickstart.md`
- `docs/reference/cli.md`
- `tests/README.md`
