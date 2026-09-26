# Aethyme Quick Start

Last Updated: 2026-09-26

This page takes a fresh machine to a first Explore answer, a first broker
round trip, and (optionally) the graph-backed commands. Every command below was
run against `aethyme 0.8.3`. The canonical product overview is the
[top-level README](../../../../README.md); the product map is
[`docs/product-surface.md`](../../../../docs/product-surface.md).

## 1. Install

Releases ship the `aethyme` router and its required `aethyme-engine-cli`
sibling as one unit, for Apple Silicon macOS, Intel macOS, x86-64 and arm64
Linux (glibc), and x86-64 Linux (musl, static, for Alpine).

```bash
brew install schiste/tap/aethyme
# or, without Homebrew, the installer (detects glibc vs musl on Linux):
curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh

aethyme --version
aethyme-engine-cli --version
```

Both commands must print the same version. No interpreter, virtualenv, pip
step, or background updater is involved.

The installer always checks the archive against the release manifest's
SHA-256. When `cosign` is on `PATH` it also verifies the manifest's Sigstore
signature (issued to this repository's release workflow at the release tag)
and stops if that fails; without cosign it prints a note and continues.
`--no-verify-signature` skips the signature check, and `--require-signature`
makes it mandatory and also checks a downloaded, reviewed `install.sh`
against the signed manifest. The Homebrew formula is published to
`schiste/tap` by the release workflow for every stable release. Installer users review updates
explicitly with `aethyme update check`, `aethyme update plan`, and
`aethyme update execute --confirm <manifest-sha256>`; Homebrew users run
`brew upgrade aethyme`.

To build from this checkout instead, install both crates. `--locked` is
required: without it Cargo re-resolves dependencies and the build can fail.

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

## 2. Explore A Repository

Explore needs no enrollment and no graph. Without a graph it returns ranked
source-search hints marked `degraded` and `verify_before_use`: navigation, not
an authoritative answer.

```bash
cd /path/to/your-repo
AETHYME_JSON="$(mktemp -t aethyme-explore.XXXXXX.json)"
aethyme explore --repo "$PWD" --request "Where is the request handler defined?" \
    --format answer-json --show-observability --depth 0 > "$AETHYME_JSON"
aethyme explore-summary --from "$AETHYME_JSON"
aethyme verify-targets --repo "$PWD" --from "$AETHYME_JSON" --max-targets 2 --max-lines 80
```

Read `safe_to_use_as_answer` and `trust_policy` in the summary, then check the
bounded source spans that `verify-targets` prints.

## 3. Set Up The Broker

`aethyme init` certifies the repository (read-only), scaffolds the broker
configuration and database, and drafts `.aethyme/gates.toml` when none exists.
It is idempotent.

```bash
aethyme init
aethyme broker quick-test
```

`quick-test` runs a full adopt, commit, and submit round trip in a disposable
repository and removes it afterwards. For shared, reviewed enrollment of the
agent guidance (`AGENTS.md`, `CLAUDE.md`, skills, hooks), follow the
[repository deployment guide](../guides/repository-deployment.md) instead of
running `aethyme deploy` blind.

## 4. Coordinate A Task

```bash
aethyme broker status
aethyme broker start --task "Describe the task"
# Change into the worktree printed by `broker start`, then edit and commit.
aethyme broker leases claim path/to/area --session <id>
aethyme broker submit --session <id>
aethyme broker finish --session <id>
```

`broker submit` simulates the merge onto the local `aethyme/integration`
branch, runs the affected gates on the merged tree, and promotes on success.
It never pushes. See the [broker workflows guide](../guides/broker-workflows.md)
for leases, gates, handoffs, and recovery.

## 5. Optional: Graph-Backed Commands

`aethyme repo inspect`, `aethyme query symbol`, `aethyme task pack`,
`aethyme task explain`, and `aethyme graph callers` read a graph store and
refuse when it is missing. The graph is a repository opt-in:

```bash
aethyme deploy --repo . --with-graph
git add -A && git commit -m "chore: enroll Aethyme graph"
aethyme graph refresh plan --repo . --diff
aethyme graph refresh execute --repo . --confirm <plan-sha256>
```

Add `--graph-repository owner/name` to the deploy when the repository has no
canonical origin. Commit the refreshed fragments; other clones then run
`aethyme graph materialize --repo .`. The full procedure, including the
version pin, is in the [graph refresh guide](../guides/graph-refresh.md).
With a graph in place:

```bash
aethyme query symbol "$PWD" main
aethyme task pack --repo "$PWD" --task "Explain this repo" --json-output
aethyme graph callers "$PWD" helper --json-output
```

## 6. Run The Test Suite (Contributors)

```bash
cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace
```

That is the whole test story: no venv and no `pip install`. See
[`../guides/testing.md`](../guides/testing.md) for the suite layout.
