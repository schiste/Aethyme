# Graph performance evidence

Last Updated: 2026-09-04

Aethyme graph lifecycle commands expose content-free performance evidence in
their JSON output. The observations are diagnostic measurements, not plan
authority: elapsed time and process memory are deliberately excluded from the
digest that authorizes a graph refresh.

## Lifecycle observations

`graph status`, `graph refresh plan --json`, `graph refresh execute --json`,
and `graph materialize --json` expose a `performance` object with these phases:

- `repository_discovery`: resolve the repository, HEAD, and tree.
- `source_snapshot`: create the disposable exact-HEAD refresh checkout.
- `policy_loading`: load graph policy and the engine pin.
- `fragment_validation`: load and verify committed/generated graph artifacts.
- `source_indexing`: discover, read, parse, and build source graph records.
- `fragment_serialization`: encode fragments, shards, and the authority manifest.
- `graph_linking`: resolve cross-fragment symbols and caller edges.
- `fragment_application`: atomically apply the reviewed fragment write set.
- `redb_materialization`: decode verified fragments and publish the local store.

Each phase reports monotonic `elapsed_us`, `bytes_read`, and `bytes_written`.
The byte values cover data visible at that phase boundary; they are not claims
about kernel, Git compression, or filesystem cache traffic. `counts` reports
files, nodes, and edges when that command constructs the complete graph. A
no-op may leave node and edge counts null rather than decoding fragments only
to populate diagnostics. `peak_memory_bytes` is the process high-water RSS on
supported Unix hosts and must be compared across separate command processes.

Explore adds `observability.performance` when observability is requested. It
contains repository discovery, redb open, query, and total microseconds, the
redb file size, and process peak RSS. It does not expose source contents,
queries beyond the already returned request contract, or absolute paths.

## Reproducible Playground matrix

Build an optimized binary, then run the harness against two or more unrelated
repositories. The harness clones every source into its own temporary
Playground and refuses the Aethyme repository itself.

```bash
cargo build --release -p aethyme-cli

packages/aethyme/scripts/bench-graph-lifecycle.sh \
  --aethyme packages/aethyme/rust/target/release/aethyme \
  --source typescript-small=/path/to/typescript-repository \
  --source python-medium=/path/to/python-repository \
  --output /tmp/aethyme-graph-performance.json
```

For each source the harness records:

1. cold refresh plan and confirmed execution;
2. cold materialization after removing only the disposable local redb;
3. an already-current no-op materialization;
4. the first and immediate second identical Explore process;
5. refresh after one committed Markdown probe is added; and
6. committed fragment and redb disk footprints.

The first Explore is “cold” only at the application-process level. The harness
does not purge operating-system caches, which would require privileged,
host-disruptive behavior. Run at least three trials on an otherwise quiet host
before setting regression thresholds. Preserve the source SHAs, Aethyme build
identity, platform, and methodology object from every report.

## Interpretation rules

- Do not compare evaluation quality or optimize scorer output with this tool.
- Do not target Aethyme itself; use representative disposable Playgrounds.
- Compare the same source SHAs and release-mode build across revisions.
- Use phase deltas to choose work. A total alone cannot distinguish Git object
  loading, parsing, graph linking, atomic application, or redb costs.
- Treat a one-file refresh near cold-refresh cost as missing incrementality,
  not as evidence that the parser itself is slow.
- Treat no-op materialization time and bytes as hot-path overhead: it should
  perform enough validation to stay safe, but should not rebuild redb.

The initial measured baseline and resulting priorities are recorded in
[Phase 2 graph performance baseline](../reports/graph-performance-phase2.md).
