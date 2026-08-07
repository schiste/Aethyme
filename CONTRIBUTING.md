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

That is the whole setup. `packages/aethyme` has no Python at all — `src/`
was deleted on 2026-08-01 (python-retirement Phase 6) and the dev pytest
harness followed on 2026-08-06 (Phase 7). No venv, no `pip install`, no
`pyproject.toml`.

`packages/aethyme-eval` is a separate package and stays Python by design;
it owns its own venv and its own gate.

## Core Checks

```bash
cd packages/aethyme/rust
cargo test --workspace
```

That includes the implementation-blind suites that drive the built
binaries (`aethyme-cli/tests/`) and the repo-hygiene suites over docs,
templates, and the PR template (`aethyme-testkit/tests/`). See
[`packages/aethyme/docs/guides/testing.md`](packages/aethyme/docs/guides/testing.md).

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
