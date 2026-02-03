# Aethyme (Aethyme SaaS)

Aethyme is the dedicated Aethyme SaaS repository. It contains:

- `packages/aethyme`: Aethyme core indexer + API
- `packages/aethyme-cloud`: Aethyme Cloud SaaS (API + web)

## Quick Start

### Aethyme Core

```bash
cd packages/aethyme
python -m venv .venv
source .venv/bin/activate
pip install -r requirements-dev.txt
bash scripts/start-api.sh
```

### Aethyme Cloud (SaaS)

```bash
cd packages/aethyme-cloud
pnpm install
pnpm --filter @aethyme-cloud/web dev
```

## Documentation

- `docs/aethyme-production-plan.md`
- `docs/aethyme-deployment-plan.md`
- `packages/aethyme/README.md`
- `packages/aethyme-cloud/README.md`

## Notes

This repo is a clean import from the Aeptus monorepo. Any Aeptus-specific
integration has been removed so Aethyme can be deployed independently.
