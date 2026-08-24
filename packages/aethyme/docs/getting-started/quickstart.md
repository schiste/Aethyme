# Aethyme Graph Engine Quick Start

Last Updated: 2026-08-23

This page covers the lower-level graph-engine path. For the current public
product quickstart, use the broker-first flow in
[`../../../../README.md`](../../../../README.md) and the product map in
[`../../../../docs/product-surface.md`](../../../../docs/product-surface.md):
install -> `aethyme init` -> `aethyme broker quick-test` -> start -> submit.

## 1. Install

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
aethyme --version
aethyme-engine-cli --version
```

This installs the Rust router and its required engine-daemon sibling from one
checksum-verified archive. No interpreter, virtualenv, pip step, or background
updater is involved. Re-run the command for the newest stable release; see the
[v0.2.2 upgrade guide](../guides/upgrading-to-v0.2.2.md) for pinned versions,
signature verification, source installation, and rollback.

## 2. Or Build From The Checkout

```bash
cd packages/aethyme
cargo build --release --manifest-path rust/Cargo.toml
```

## 3. Run The Local-First Path

Aethyme is local-first. No services, database, credentials, **or Python**
are required.

```bash
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
cd rust && cargo test --workspace
```

That is all of it — no venv, no `pip install`. The implementation-blind
suites that drive the built binaries and the repo-hygiene suites over
docs and templates are part of the workspace since python-retirement
Phase 7 (2026-08-06).

See [`../guides/testing.md`](../guides/testing.md) for the suite layout.
