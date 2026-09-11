# Troubleshooting

Last Updated: 2026-09-11

## API Will Not Start

### Symptoms
- startup exits early
- `/health/ready` is unavailable

### Checks

```bash
cd packages/aethyme
bash scripts/start-api.sh
```

Verify:

- `DATABASE_URL` points to PostgreSQL
- migrations have been applied
- PostgreSQL is reachable

## Indexing Fails

### Checks

```bash
cd packages/aethyme
aethyme repo ingest .
```

If fallback works and SCIP mode fails, the issue is in the language indexer toolchain rather than the shared indexing contract.

## Search Returns No Results

Check that the repository was indexed successfully:

```bash
curl -s http://localhost:8001/api/v1/index/freshness \
  -H "Authorization: Bearer $TOKEN"
```

## Scorecard Fails

Verify that the repository path still exists and that the repository was indexed under the expected tenant.

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

## First Debug Command

```bash
cd packages/aethyme
make test-full
```
