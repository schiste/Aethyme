# Contributing To Aethyme

Thank you for considering a contribution.

The initial open-source contribution scope is Aethyme Core:

- `packages/aethyme`
- `packages/aethyme/rust`
- root documentation and CI that support Aethyme Core

Historical packages removed from the tree are not the initial
community support surface unless maintainers explicitly mark them as such.

## Development Setup

```bash
cd packages/aethyme/rust
cargo build --quiet --bin aethyme --bin aethyme-engine-cli
```

`packages/aethyme` has no Python product code — `src/` was deleted on
2026-08-01 (python-retirement Phase 6) and there is nothing to
`pip install`. A dev-only pytest harness still drives the built binary
in `tests/local`; it needs the tools, never an editable install:

```bash
cd packages/aethyme
python3 -m venv .venv
.venv/bin/python -m pip install pytest pytest-asyncio ruff
```

That harness retires when `tests/local` ports to Rust.

## Core Checks

```bash
cd packages/aethyme/rust
cargo test --workspace
cd .. && .venv/bin/python -m pytest -q tests/local   # dev harness
```

## Pull Request Expectations

- Keep changes scoped and explain the behavior change.
- Include the exact commands you ran.
- Update docs when commands, contracts, or public behavior change.
- Do not commit generated local artifacts, runtime databases, secrets, or private
  repository data.

## Eval Rules

All evaluations run against Playground repositories, never against Aethyme
itself.

Never modify tools, engine, pipeline, or skills to improve eval scores. Evals
are diagnostics, not targets. If an eval reveals a weakness, fix the generic
system rather than adding task-specific accommodations.
