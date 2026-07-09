# Aethyme

Aethyme is repository-intelligence tooling for mapping source code into a
deterministic graph that agents and developers can query.

The initial open-source scope is **Aethyme Core**: local repository indexing,
graph traversal, search, task context generation, and evaluation tooling in
`packages/aethyme`.

## Status

Aethyme is early-stage developer tooling. The core local workflow is the public
entry point. Cloud, hosted SaaS, and eval dashboard packages are present in this
monorepo but are not the initial public support surface.

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
aethyme explore --repo . --request "Explain the main repository structure" --format answer-json
```

For the longer setup guide, see
[`packages/aethyme/docs/getting-started/quickstart.md`](packages/aethyme/docs/getting-started/quickstart.md).

## Core Commands

```bash
aethyme explore --repo /path/to/repo --request "Find the auth flow" --format answer-json
aethyme repo warm /path/to/repo
aethyme task context --repo /path/to/repo --task "Update validate_token flow" --json-output
aethyme query symbol /path/to/repo main
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
