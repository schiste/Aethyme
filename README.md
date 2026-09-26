# Aethyme

> Local-first coordination and repository intelligence for AI coding agents.

Aethyme is a Rust-native CLI for running concurrent agent work safely in one
repository. It gives each task an isolated worktree, tracks ownership and
leases, runs repository-owned gates against the merged tree, and promotes
verified work to a local integration branch. Its engine adds bounded,
observable repository navigation when an agent needs context.

There is no hosted control plane to operate. Repository policy and broker state
live in `.aethyme/`; the optional graph store is derived locally.

## The product in three surfaces

| Surface | What it provides | Start with |
| --- | --- | --- |
| **Coordinate** | Sessions, worktrees, leases, gates, merge simulation, integration, handoffs, and guarded Git/GitHub operations. | `aethyme broker start --task "..."` |
| **Explore** | Deterministic repository orientation, bounded evidence, task context, graph queries, and verification targets. | `aethyme explore --repo . --request "..."` |
| **Improve** | Readiness checks, repository-quality inspection, scorecards, and controlled autofixes. | `aethyme readiness` |

The broker is the public front door. Explore and the lower-level graph and task
commands are supporting repository intelligence for agents and operators.

## Why not plain worktrees?

`git worktree` plus pull requests, or Claude Code's native worktree isolation,
already give each agent its own checkout. That solves "two agents editing one
directory". It does not solve what happens when their work meets. The broker
adds that part:

| Concern | `git worktree` + PRs | Claude Code native worktrees | Aethyme broker |
| --- | --- | --- | --- |
| Isolated checkout per task | yes | yes | yes (`broker start`) |
| Gates run on the *merged* tree before integration | only if CI does it, after push | no | yes: `broker submit` simulates the merge onto `aethyme/integration` and runs the repository's affected gates on that result |
| Path ownership between concurrent agents | no | no | leases: a conflicting `leases claim` is refused, and `broker exec` fails a command that dirties paths outside its leases |
| A local integration branch that only verified work reaches | no | no | `aethyme/integration`; nothing is pushed until someone publishes |
| Serialized, journaled Git/GitHub writes | no | no | `broker git` / `broker gh` queue per repository and journal each write; an unknown remote outcome fails closed until `broker operations reconcile` |
| Recovery after conflicts or crashes | manual | manual | conflict notices with exact rebase steps (`.aethyme/broker-action-required.md`), `broker blockers`, `broker cleanup` with provenance checks |

You do **not** need the broker when:

- one agent works at a time, in which case a plain branch or worktree is simpler;
- there are no shared gates, so merged-tree verification has nothing to run;
- every change already goes through a PR whose CI you trust, and agents never
  touch the same files concurrently.

The broker costs a local SQLite database, one worktree per session, and a
submit step that runs gates. It pays off when several agents share one
repository and its gates, and when an unverified merge or a duplicated push
is expensive.

## Quick start

### 1. Install the Rust binaries

Supported release targets are Apple Silicon macOS, Intel macOS, x86-64 and
arm64 Linux (glibc), and x86-64 Linux (musl, static, for Alpine and other
non-glibc distributions). A release contains the paired `aethyme` router and
`aethyme-engine-cli` engine binary.

Windows is not supported natively; run the x86-64 Linux build under WSL2.
The cost of a native port is scoped in
[docs/architecture/windows-port.md](packages/aethyme/docs/architecture/windows-port.md).

With Homebrew:

```bash
brew install schiste/tap/aethyme
aethyme --version
aethyme-engine-cli --version
```

The tap formula (macOS and glibc Linux, Intel and ARM) is updated by the
release workflow for every stable release.

Without Homebrew, use the installer. It picks the archive for your platform,
including the musl build where `ldd` reports musl, and checks it against the
release manifest's SHA-256:

```bash
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh
aethyme --version
aethyme-engine-cli --version
```

When `cosign` (Cosign 3) is on `PATH`, the installer also verifies the
release manifest's keyless Sigstore signature against this repository's
release workflow before installing, and stops if it fails. Without cosign it
prints a one-line note and continues on checksums alone. `--no-verify-signature`
skips the signature check. For the strictest installation, download and review
`install.sh`, then run it with `--require-signature`: it fails when cosign is
missing and also checks the installer file itself against the signed manifest
(`--verify-signature` is the older spelling of the same flag). Installer users can review
updates explicitly with `aethyme update check`, `aethyme update plan`, and
`aethyme update execute --confirm <manifest-sha256>`.

Contributors can install from this checkout instead:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

### 2. Enroll a repository

Aethyme is installed once per machine and deployed separately into each
repository. For a reviewed, shared enrollment, plan against the exact remote
default branch and authorize the printed digest:

```bash
cd /path/to/your-repo
aethyme deploy plan --repo . --diff
aethyme deploy execute --repo . --confirm <plan-sha256>
aethyme deploy verify --repo .
```

The plan is read-only. Execution applies only the reviewed policy, broker
configuration, gates, generated agent guidance, and hooks; it publishes through
the broker and refuses if the reviewed repository state has changed.

For an intentionally offline/manual enrollment, use the non-publishing path:

```bash
aethyme deploy --repo .
aethyme deploy verify --repo .
```

To keep the full policy clone-local, first commit the inert bridge and then
activate local-only deployment:

```bash
aethyme deploy bridge --repo .
git add AGENTS.md CLAUDE.md
git commit -m "docs: add optional local Aethyme bridge"
aethyme deploy --local-only --repo .
aethyme deploy verify --local-only --repo .
```

Graph support is opt-in and canonical. Enable it with
`aethyme deploy --repo . --with-graph`, commit the enrollment, then follow the
[graph refresh guide](packages/aethyme/docs/guides/graph-refresh.md). Ordinary
and local-only deployment remain graph-free.

Prove the local broker loop before starting real work:

```bash
aethyme broker quick-test
aethyme broker verify-loop
```

`quick-test` uses a disposable repository. `verify-loop` also reports which
integration tip was tested and detects movement during the check.

### 3. Coordinate a task

```bash
aethyme broker status
aethyme broker start --task "Describe the task"
# Change into the worktree printed by `broker start`.
aethyme broker leases claim path/to/area --session <id>
# Edit, test, and commit in that worktree.
aethyme broker submit --session <id>
aethyme broker finish --session <id>
```

Use `aethyme broker adopt --task "..."` for an existing dedicated worktree.
Run broad commands through `aethyme broker exec --session <id> -- ...`; use
`aethyme broker git` and `aethyme broker gh` for coordinated Git or GitHub
mutations. Only committed work can be submitted.

`broker submit` simulates the merge, selects the affected repository gates,
and promotes a verified result to the local `aethyme/integration` branch. It
does not publish a remote branch. Use the reviewed `broker ship plan` /
`broker ship execute` lane, or your normal review flow, when publication is
authorized.

## Explore a repository

`aethyme explore` is the bounded navigation path for agents. It runs through
the Rust engine, starts the paired local engine daemon when needed, and
returns candidate files, evidence, verification steps, confidence, and
observability. If the optional graph is unavailable, it returns a degraded
result that must be verified before it is treated as an answer.

For the full agent-oriented loop, keep one saved `answer-json` result and feed
that same file to both readers:

```bash
AETHYME_JSON="$(mktemp -t aethyme-explore.XXXXXX.json)"
aethyme explore \
  --repo "$PWD" \
  --request "Find the files responsible for this behavior" \
  --format answer-json \
  --show-observability \
  --depth 0 > "$AETHYME_JSON"
aethyme explore-summary --from "$AETHYME_JSON"
aethyme verify-targets \
  --repo "$PWD" \
  --from "$AETHYME_JSON" \
  --max-targets 2 \
  --max-lines 80
```

Lower-level commands are available for focused work:

```bash
aethyme graph callers /path/to/repo <target> --json-output
aethyme task pack --repo /path/to/repo --task "Explain this area" --json-output
aethyme intents --request "Find public functions with no outside callers" --format compact-json
```

## Architecture

The shipped product lives in the Rust workspace at
[`packages/aethyme/rust`](packages/aethyme/rust):

| Component | Responsibility |
| --- | --- |
| `aethyme` / `aethyme-cli` | The single native command entrypoint and router. |
| `aethyme-broker` | Sessions, external worktrees, leases, gates, merge queue, integration, events, reports, and publication coordination. |
| `aethyme-engine` | Repository mapping, graph storage and traversal, Explore, and deterministic task-context packs. |
| `aethyme-enhance` | Repository deployment, generated agent guidance, skills, hooks, and experience artifacts. |
| `aethyme-quality` | Readiness, repository-quality analysis, scorecards, and controlled autofix behavior. |
| `aethyme-engine-cli` | The paired local engine-daemon binary used by the router. |

The shipped product path is 100% Rust: it needs no Python runtime, virtual
environment, or pip installation. `packages/aethyme-eval` is a deliberately
separate Python acceptance harness and does not belong to the product runtime.
The old `python -m src.cli` path was removed and has no compatibility shim.

## Development

From the repository root:

```bash
cargo build --manifest-path packages/aethyme/rust/Cargo.toml --workspace
cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace
cargo fmt --manifest-path packages/aethyme/rust/Cargo.toml --all -- --check
cargo clippy --manifest-path packages/aethyme/rust/Cargo.toml --workspace --all-targets
```

The workspace test command above is the product test story. It includes unit tests,
implementation-blind CLI suites that drive the built binaries, and repository
hygiene tests for docs, templates, and contracts. See the
[testing guide](packages/aethyme/docs/guides/testing.md) for the suite layout.

When a binary update changes repository policy or embedded migrations, review
the repository separately with `aethyme upgrade plan --repo . --diff`; binary
updates and repository upgrades are intentionally independent.

## Documentation

- [Public product surface](docs/product-surface.md) — canonical user journeys and command tiers.
- [Repository deployment contract](packages/aethyme/docs/guides/repository-deployment.md) — reviewed enrollment, local-only mode, and recovery.
- [Broker workflows](packages/aethyme/docs/guides/broker-workflows.md) — preparation, leases, gate evidence, reuse, handoffs, and recovery.
- [CLI reference](packages/aethyme/docs/reference/cli.md) — command and contract details.
- [Graph refresh guide](packages/aethyme/docs/guides/graph-refresh.md) — opt-in committed graph artifacts and local materialization.
- [Contributing](CONTRIBUTING.md) — development setup and contribution expectations.
- [Changelog](CHANGELOG.md) — user-visible release history.

## Security, support, and license

- [Security policy](SECURITY.md)
- [Support](SUPPORT.md)
- [Code of conduct](CODE_OF_CONDUCT.md)
- [Governance](GOVERNANCE.md)

Aethyme is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE)
for attribution information.
