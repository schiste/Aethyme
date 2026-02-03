# @aethyme/cli

Installable CLI to generate an Aethyme repository graph.

## Usage

```bash
pnpm dlx @aethyme/cli graph --repo . --output .aethyme/graph.json
```

## Requirements

- Docker + Docker Compose v2
- Python 3.11+
- SCIP binaries on PATH (`scip-python`, `scip-typescript`)

## Notes

- The CLI uses Aethyme core (the Python indexer in `packages/aethyme`).
- If the core is not found, set `AETHYME_CORE_PATH` to its location.
