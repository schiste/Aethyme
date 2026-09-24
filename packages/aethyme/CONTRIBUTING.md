# Contributing To Aethyme

## Development Standard

Work against the current core flow only:

1. run `explore` against a repository (no graph required)
2. run graph and task-context queries on a graph-enrolled repository
3. run scorecard and broker workflows
4. keep tests and docs aligned with that flow

Do not add speculative status docs, sprint reports, or checked-in fake fixture repositories.

## Local Setup

```bash
cd packages/aethyme
cargo build --release --manifest-path rust/Cargo.toml
```

No database, services, **or Python** are required — including for the
tests. `src/` was deleted on 2026-08-01 (python-retirement Phase 6) and
the dev pytest harness followed on 2026-08-06 (Phase 7), so this package
is 100% Rust: no venv, no `pip install`, no `pyproject.toml`.

## Run The Core Checks

```bash
cd rust && cargo test --workspace && cd ..
aethyme explore --repo "$PWD" --request "Where are broker leases claimed?" --format answer-json
```

`cargo test --workspace` is the whole test story, including the
implementation-blind suites that drive the built binaries. See
[`docs/guides/testing.md`](docs/guides/testing.md).

## Documentation Rule

If behavior changes, update one of these instead of adding a new status file:

- `README.md`
- `docs/getting-started/quickstart.md`
- `docs/reference/cli.md`
- `docs/guides/testing.md`
