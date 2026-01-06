# Aethyme (RepoGraph SaaS)

Aethyme is the dedicated RepoGraph SaaS repository. It contains:

- `packages/repograph`: RepoGraph core indexer + API
- `packages/repograph-cloud`: RepoGraph Cloud SaaS (API + web)

## Quick Start

### RepoGraph Core

```bash
cd packages/repograph
python -m venv .venv
source .venv/bin/activate
pip install -r requirements-dev.txt
bash scripts/start-api.sh
```

### RepoGraph Cloud (SaaS)

```bash
cd packages/repograph-cloud
pnpm install
pnpm --filter @repograph-cloud/web dev
```

## Documentation

- `docs/repograph-production-plan.md`
- `docs/repograph-deployment-plan.md`
- `packages/repograph/README.md`
- `packages/repograph-cloud/README.md`

## Notes

This repo is a clean import from the Aeptus monorepo. Any Aeptus-specific
integration has been removed so RepoGraph can be deployed independently.
