# Upgrading to Aethyme v0.8.3

Last Updated: 2026-09-24

v0.8.3 makes Explore useful without a graph, which is phase 3 of the recovery
plan. No operator action is needed.

## What is new

### Explore searches source when there is no graph

Most repositories have no materialized graph store. In those repositories,
Explore used to match path names in the first 128 files. It now:

- searches the contents of tracked and untracked, non-ignored files within a
  2 s budget
- ranks files with BM25F over file-name, directory, definition-name, code and
  comment fields
- returns up to 8 hits, each with the definitions that best cover the request

A symbol index is built on demand and cached under the host cache
(`symbol-index/v1/`). It is invalidated by file mtime and size.

`observability.readiness` is `ready` when the search completed, or `partial`
with a reason when the budget ran out. `observability.source_fallback`
describes the search.

### Graph-free results are navigation, not answers

`safe_to_use_as_answer` stays false for graph-free results, and `answer[]` is
empty. Verify the ranked spans before acting on them. A content-evidence rule
is evaluated and reported in `observability.source_fallback.answer_safety`, but
it will only be allowed to mark answers safe once its precision is at least
95%.

### How much it helps

Measured on a private held-out set of 40 questions, scoring a hit only when its
span overlaps the answer lines:

- Explore's mean reciprocal rank is 0.39.
- BM25-ranked ripgrep scores 0.10, and a stronger BM25+ variant scores 0.28.
- In an agent benchmark, Explore answered in 4.75 turns on average against
  7.54 for ripgrep.

### TypeScript and JavaScript exports are indexed

`export`-wrapped declarations were previously dropped by the indexer. Graphs and
symbol indexes now include them. Repositories that commit graph fragments gain
these symbols when they regenerate.

## Compatibility

**No schema migration.** The broker database stays at schema 42.

- Install the CLI and engine pair together.
- The answer-json shape is unchanged. Consumers that read
  `observability.source_fallback` should expect its new fields (see
  `docs/architecture/cross-process-consumers.md`).

## Before upgrading

- Check for in-flight submissions: `aethyme broker status --json`.

## Install or update

```
cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli
cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine
```

## Migrate and verify

```
aethyme --version                  # 0.8.3; build_commit matches the release tag
aethyme plugin status              # engine pair: matched
aethyme explore --repo . --request "where is the config loaded" --format answer-json
```

## Known issues

- On very large repositories (tens of thousands of files), the 2 s budget can
  end the search early. Results then depend on how warm the symbol-index cache
  is. `readiness` reports `partial` when this happens.
- The trust test escape (`AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS`) also skips
  the terminal check when set outside tests.
- Orphaned gate containers (#287) are not yet listed as blockers.
- In a repository with customized CLAUDE.md or AGENTS.md, no command refreshes
  the generated guidance.

## Rollback

Unrestricted. Reinstall v0.8.2; it reads the same schema-42 database. The
`symbol-index/v1/` cache is ignored by 0.8.2 and can be deleted.
