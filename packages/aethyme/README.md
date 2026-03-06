# Aethyme Core

Last Updated: 2026-03-06

Aethyme Core is the backend product in this repository.

It owns:

1. repository indexing
2. graph persistence and traversal
3. search, ego graph, and impact analysis
4. scorecard analysis
5. controlled autofix tooling from the CLI
6. deterministic navigation primitives for AI agents

## Canonical Model

`Platform > Org > Tenant > Repository > Graph`

Runtime isolation is tenant-scoped.

## Language Direction

Aethyme is moving toward:

- Rust for deterministic engine components
- Python for API, CLI, auth enforcement, scorecard orchestration, and SDKs

See [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md) and [`rust/README.md`](rust/README.md).

## Auth Boundary

- cloud owns login, registration, sessions, and user lifecycle
- core validates bearer credentials and API keys
- core enforces `org`, `tenant_id`, and `scopes`
- local development can mint a cloud-issued token for an existing user via `POST /api/auth/dev/token`

## Active Surface

### Core Logic
- `src/indexer`
- `src/indexing`
- `src/graph`
- `src/models`
- `src/scorecard`
- `src/autofixers`
- `rust`

### Delivery
- `src/api`
- `src/cli.py`
- `sdk/python`

### Verification
- `tests/api`
- `tests/queries`
- `tests/indexing`
- `tests/scorecard`
- `tests/autofixers`

## Current Standard

Only document and defend the verified path:

- trusted bearer token or API key required
- `POST /api/v1/index/repositories`
- `POST /api/v1/search/`
- `POST /api/v1/ego/`
- `POST /api/v1/impact/`
- `POST /api/v1/scorecard/scan`

API and CLI indexing both run through [`src/indexing/service.py`](src/indexing/service.py).

## Local-First Workflow

For the first product proof, Aethyme can run against one local repository without any SaaS layer.

Core commands:

- `aethyme repo ingest /path/to/repo`
- `aethyme repo inspect /path/to/repo --json-output`
- `aethyme repo clear-cache /path/to/repo`
- `aethyme query symbol /path/to/repo main`
- `aethyme query deps /path/to/repo src/main.py`
- `aethyme task pack --repo /path/to/repo --task "Explain this repo" --json-output`
- `aethyme task explain --repo /path/to/repo`
- `aethyme eval explain-repo --repo /path/to/repo --json-output`
- `aethyme eval explain-repo --repo /path/to/repo --baseline-cmd "<cmd>" --aethyme-cmd "<cmd>"`

This local path is the shortest route to proving:

1. repository mapping
2. discoverability
3. deterministic task-context packs
4. explain-repo evaluation artifacts

Runtime notes:

- the Python layer now executes a built Rust binary rather than `cargo run` for every call
- local repo artifacts are cached by snapshot key under `AETHYME_CACHE_DIR` or `/tmp/aethyme-cache`
- Git repositories use commit plus dirty-state metadata for cache keys instead of a full recursive fingerprint on every call
- `eval explain-repo` can execute real baseline and Aethyme runs when `--baseline-cmd` and `--aethyme-cmd` are provided
- without those commands, it still emits the comparison artifacts only
- the Aethyme-assisted prompt now uses a compact rendered context-pack view instead of injecting the full raw pack

## Start Here

- [`../../docs/project-plan.md`](../../docs/project-plan.md)
- [`docs/vision.md`](docs/vision.md)
- [`docs/agent-navigation-spec.md`](docs/agent-navigation-spec.md)
- [`docs/architecture/rust-transition.md`](docs/architecture/rust-transition.md)
- [`rust/README.md`](rust/README.md)
- [`docs/getting-started/quickstart.md`](docs/getting-started/quickstart.md)
- [`roadmap.md`](roadmap.md)
