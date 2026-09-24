# Troubleshooting

Last Updated: 2026-09-24

## Explore Returns `degraded`

Explore answers without a graph, but then its `trust_policy` is
`verify_before_use` and `safe_to_use_as_answer` is `false`: the hints are
ranked source-search navigation, not caller or impact evidence. This is the
expected result for a repository that has not opted into the graph. Verify the
spans that `aethyme verify-targets` prints, or enroll the graph (below) when
you need caller and impact answers.

## Graph Commands Refuse: Graph Store Missing

### Symptoms

- `aethyme query symbol`, `task pack`, `task explain` or `graph callers` fail
  with `graph store at .../.aethyme/graph_store.redb is missing`
- `aethyme repo ingest` or `repo inspect` fail with `open fragment store`
- `aethyme graph materialize` refuses with `AuthorityDisabled`

### Checks

```bash
aethyme graph status --repo . --json
```

Graph support is a repository opt-in, and query commands are read-only: they
never build the store. Enroll with `aethyme deploy --repo . --with-graph`,
commit, then follow the [graph refresh guide](graph-refresh.md) to generate the
committed fragments and materialize the local store.

## Broker Refuses: Schema Version Is Newer

### Symptoms

- `broker db schema version N is newer than this binary supports (M); upgrade aethyme`
- agent plugin hooks fail or go quiet on the same machine

The broker database is machine-wide, and migrations are one-way. A newer
binary (possibly a gate's build, or another worktree's `cargo install`) has
migrated it. Upgrade every installed copy of the `aethyme` and
`aethyme-engine-cli` pair on the machine; see
[`UPGRADING.md`](../../../../UPGRADING.md) for the release that introduced
schema `N`.

## Aethyme Behaves In Ways Its Version Cannot Explain

### Symptoms

- a command fails against an engine capability its version should have
- behaviour changes between one worktree and another with no code difference
- `aethyme --version` looks entirely reasonable

### Checks

```bash
aethyme plugin status
```

Look at the `Engine pair:` line. `aethyme` and `aethyme-engine-cli` are one
product shipped as two binaries, and on a development machine there is one
`~/.cargo/bin` but many worktrees: whichever session ran `cargo install --path`
last wins, separately for each binary. Two sessions installing from different
branches on the same afternoon leave a router from one beside an engine from
the other.

They usually share a version number and differ only in the suffix `git
describe` adds, which is why reading `--version` alone does not reveal it:

```
aethyme               0.7.16 (v0.7.16-8-gf74bf96b)
aethyme-engine-cli    0.7.16 (v0.7.16-6-g080d3c8a)
```

The fix is to reinstall both from one tree, in one command, so a later failure
cannot leave the pair half-updated:

```bash
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli \
  && cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

Then confirm both report the same build, and check nothing else on `PATH`
shadows them — `which -a aethyme` shows the order, and a Homebrew copy in
`/opt/homebrew/bin` precedes `~/.cargo/bin` on a default macOS `PATH`.

The `SessionStart` hook reports a split pair to the agent as well, so an agent
session that starts on a skewed machine is told before it does any work. Set
`AETHYME_UPDATE_CHECK=off` to silence that notice.

## First Debug Commands

```bash
aethyme broker doctor
aethyme broker blockers
cargo test --manifest-path packages/aethyme/rust/Cargo.toml --workspace
```
