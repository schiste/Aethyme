# @aethyme/cli

Installable CLI to generate an Aethyme repository graph.

## Usage

```bash
pnpm dlx @aethyme/cli graph --repo . --output .aethyme/graph.json
```

```bash
node bin/aethyme.js deck
```

```bash
node bin/aethyme.js deck --print
```

## Requirements

- Docker + Docker Compose v2
- Python 3.11+
- SCIP binaries on PATH (`scip-python`, `scip-typescript`)

For interactive deck mode, run inside a TTY terminal.

## Notes

- The CLI uses Aethyme core (the Python indexer in `packages/aethyme`).
- If the core is not found, set `AETHYME_CORE_PATH` to its location.
- Deck commands are experimental and are not part of the initial public support scope.
