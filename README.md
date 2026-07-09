# Aethyme

Aethyme is repository-intelligence tooling for mapping source code into a
deterministic graph that agents and developers can query.

The initial open-source scope is **Aethyme Core**: local repository indexing,
graph traversal, search, task context generation, and evaluation tooling in
`packages/aethyme`.

## Status: what exists today vs what is planned

**Exists and works today**

- A deterministic **Rust graph engine** (`packages/aethyme/rust`): repo
  indexing into committed binary fragments plus a local redb store, graph
  navigation, impact frontiers, task-context packs, and a warm unix-socket
  daemon.
- A **Python CLI** (`aethyme`, click-based) that shells out to the engine:
  `repo`, `query`, `graph`, `task`, `facts`, `analyze`, `enhance`, `ai-ready`,
  `autofix`, `eval`.
- An **eval harness** with a playground protocol and a local eval dashboard
  (`packages/aethyme-eval-ui`).

**Planned, not implemented**

- A **local agent broker** for high-concurrency AI development: per-agent git
  worktrees, an agent session registry, file leases and overlapping-edit
  detection, an affected gate runner, merge simulation, and a promotion flow.
  This will be a **new local subsystem**; the graph engine remains a
  supporting repo-intelligence service and may later provide impact hints to
  the broker. See
  [`docs/aethyme-local-agent-broker.md`](docs/aethyme-local-agent-broker.md).

**Present but out of scope / frozen**

- `packages/aethyme-cloud` (SaaS scaffold) and the PostgreSQL/SCIP API lineage
  inside `packages/aethyme/src` are earlier cloud-oriented work. They are not
  part of the current local-first direction and are not required by any local
  workflow. No cloud execution, auth, or team sync is part of broker v0.

## Repository Layout

- `packages/aethyme`: core Python CLI/API code, Rust engine workspace, graph
  indexing, search, scorecard, and eval harnesses.
- `packages/aethyme/rust`: deterministic Rust engine and graph crates.
- `packages/aethyme-cloud`: SaaS-oriented application shell. Experimental for
  public OSS purposes.
- `packages/aethyme-eval-ui`: local evaluation dashboard. Experimental for
  public OSS purposes.
- `docs`: project-level planning and architecture notes.

## Quick Start

Prerequisites:

- Python 3.11+
- Rust toolchain
- Git

```bash
cd packages/aethyme
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'
cd rust
cargo build --quiet --bin aethyme-engine-cli
cd ..
aethyme repo inspect . --mode brief
rust/target/debug/aethyme-engine-cli explore --repo . \
  --request "Explain the main repository structure" --format answer-json
```

Note on `explore`: the `explore` command was removed from the Python CLI on
2026-05-08 and is served natively by the Rust binaries (`aethyme-engine-cli
explore`, or the `aethyme` Rust router binary). Because `pip install` also
puts a Python `aethyme` entrypoint on your PATH, invoke the Rust binary by
path as shown above unless you have arranged for the Rust `aethyme` binary to
take precedence.

For the longer setup guide, see
[`packages/aethyme/docs/getting-started/quickstart.md`](packages/aethyme/docs/getting-started/quickstart.md).

## Core Commands

```bash
aethyme repo warm /path/to/repo
aethyme repo inspect /path/to/repo --mode brief
aethyme task context --repo /path/to/repo --task "Update validate_token flow" --json-output
aethyme query symbol /path/to/repo main
rust/target/debug/aethyme-engine-cli explore --repo /path/to/repo \
  --request "Find the auth flow" --format answer-json
```

The Python CLI shells out to the Rust engine binary for deterministic repository
mapping and graph operations. See
[`packages/aethyme/README.md`](packages/aethyme/README.md) and
[`packages/aethyme/rust/README.md`](packages/aethyme/rust/README.md) for more
detail.

## Development

```bash
cd packages/aethyme
.venv/bin/pytest -q tests/local
cd rust
cargo test --workspace
```

Eval work has stricter rules: evaluations run against Playground repositories,
never against Aethyme itself. See
[`packages/aethyme/docs/guides/eval-protocol.md`](packages/aethyme/docs/guides/eval-protocol.md).

## Security And Support

- Security policy: [`SECURITY.md`](SECURITY.md)
- Support scope: [`SUPPORT.md`](SUPPORT.md)
- Contribution guide: [`CONTRIBUTING.md`](CONTRIBUTING.md)
- Governance: [`GOVERNANCE.md`](GOVERNANCE.md)
- Code of conduct: [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
