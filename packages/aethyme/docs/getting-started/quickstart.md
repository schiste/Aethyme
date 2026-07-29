# Aethyme Graph Engine Quick Start

Last Updated: 2026-07-29

This page covers the lower-level graph-engine path. For the current public
product quickstart, use the broker-first flow in
[`../../../../README.md`](../../../../README.md) and the product map in
[`../../../../docs/product-surface.md`](../../../../docs/product-surface.md):
install -> `aethyme init` -> `aethyme broker quick-test` -> adopt -> submit.

## 1. Install

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
```

## 2. Build The Rust Engine

```bash
cargo build --release --manifest-path rust/Cargo.toml
```

## 3. Run The Local-First Path

Aethyme is local-first: the CLI drives a built Rust engine binary directly.
No services, database, or credentials are required.

```bash
. .venv/bin/activate

aethyme repo ingest /absolute/path/to/repo
aethyme repo inspect /absolute/path/to/repo --json-output
aethyme repo clear-cache /absolute/path/to/repo
aethyme query symbol /absolute/path/to/repo main
aethyme task pack --repo /absolute/path/to/repo --task "Explain this repo" --json-output
aethyme task explain --repo /absolute/path/to/repo
```

This path proves:

1. deterministic repository mapping
2. deterministic discoverability
3. deterministic task-context packs

At this stage:

- the Rust engine is executed as a built binary
- local artifacts are cached by repository snapshot
- Git repositories use commit plus dirty-state metadata for cache keys

## 4. Run The Test Suite

```bash
. .venv/bin/activate
pytest tests/local tests/indexing tests/scorecard tests/autofixers tests/contracts tests/docs -q
```

See `tests/README.md` for the suite layout.
