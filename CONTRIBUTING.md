# Contributing To Aethyme

Thank you for considering a contribution.

The initial open-source contribution scope is Aethyme Core:

- `packages/aethyme`
- `packages/aethyme/rust`
- root documentation and CI that support Aethyme Core

`packages/aethyme-cloud` is not the initial
community support surface unless maintainers explicitly mark them as such.

## Development Setup

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
cd rust
cargo build --quiet --bin aethyme-engine-cli
```

## Core Checks

```bash
cd packages/aethyme
.venv/bin/pytest -q tests/local
cd rust
cargo test --workspace
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
