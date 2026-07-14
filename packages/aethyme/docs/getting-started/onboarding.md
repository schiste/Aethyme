# Onboarding

Last Updated: 2026-03-06

## What Aethyme Core Is

Aethyme Core is a backend for:

1. indexing repositories into a graph
2. querying that graph
3. running scorecard analysis
4. applying controlled autofixes from the CLI

## What To Read First

1. [`../README.md`](../README.md)
2. [`quickstart.md`](quickstart.md)
3. [`../reference/cli.md`](../reference/cli.md)
4. [`../guides/testing.md`](../guides/testing.md)

## Working Rules

- treat `Platform > Org > Tenant > Repository > Graph` as canonical
- keep API and CLI on shared services
- do not add customer identity flows to core
- do not add new broad platform claims to docs

## First Local Checks

```bash
cd packages/aethyme
. .venv/bin/activate
python -m src.cli stats
make test-unit
```
