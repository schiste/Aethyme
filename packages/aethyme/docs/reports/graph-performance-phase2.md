# Phase 2 graph performance baseline

Last Updated: 2026-09-04

This is the first release-mode baseline produced by the
[graph performance harness](../guides/graph-performance.md). It is performance
evidence, not an evaluation score and not yet a CI threshold.

## Method

- Host: Apple Silicon macOS (`Darwin arm64`).
- Aethyme: `0.7.5`, after Phase 1 and the Phase 2 observability commits.
- Isolation: a fresh disposable Playground clone per source.
- Samples: `Google-tabcleaner` (small TypeScript), `wiki-assistant` (small
  Python), and `Mockup` (large mixed application).
- Exact source commits: `1251585e3ca682913a08c48ea4f57451f9ff8012`,
  `639a5fbbb7cee733dcd01e6e9bccffff47e900d5`, and
  `5e4be5658cab893ec82c5f6fa880e4d565cd3c1f`, respectively.
- One-file refresh: add and commit one Markdown probe after the cold graph
  snapshot.
- Cold/warm Explore: separate processes with an identical request; operating
  system caches were not purged.
- Samples shown below are one run each. They establish scale and attribution;
  repeat runs are required before enforcing regression bands.

## End-to-end results

Times are seconds. Confirmed refresh includes its required state revalidation
and application, reflecting the real user command.

| Playground | Tracked files | Graph files | Nodes | Edges | Cold plan | Cold refresh | One-file refresh | Cold materialize | No-op materialize | Cold Explore | Warm Explore |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| TypeScript small | 53 | 43 | 58 | 78 | 0.385 | 0.950 | 0.995 | 1.025 | 0.984 | 0.027 | 0.023 |
| Python small | 78 | 39 | 793 | 1,182 | 0.468 | 1.335 | 1.546 | 1.454 | 1.383 | 0.050 | 0.046 |
| Mixed large | 9,708 | 9,363 | 172,581 | 227,468 | 79.094 | 266.659 | 227.461 | 393.459 | 290.817 | 9.739 | 9.159 |

## Large-repository attribution

The large cold confirmed refresh reported:

| Phase | Seconds |
| --- | ---: |
| Source snapshot | 8.090 |
| Source indexing | 1.145 |
| Fragment serialization | 64.506 |
| Graph linking | 15.342 |
| Fragment validation | 0.351 |
| Fragment application | 157.330 |
| redb materialization | 12.292 |

The 227.461-second one-file confirmed refresh spent 127.019 seconds in
fragment validation, 53.238 seconds serializing the full graph, 12.207 seconds
linking it, and 19.706 seconds rebuilding redb. Applying the actual two-file
write set took only 0.074 seconds. The parser/indexer itself took 1.692 seconds.

Cold materialization spent 370.260 of 393.459 seconds validating 364.1 MB at
observable boundaries before a 21.804-second redb build. The already-current
no-op still spent 290.663 of 290.817 seconds validating the same committed
state and read 364.1 MB, while correctly writing zero bytes and performing no
redb work.

## Footprint and memory

| Playground | Committed graph | Local redb | Refresh peak RSS | Cold materialize peak RSS | No-op peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| TypeScript small | 0.23 MiB | 1.01 MiB | 11.94 MiB | 7.73 MiB | 5.86 MiB |
| Python small | 0.77 MiB | 4.01 MiB | 20.81 MiB | 13.48 MiB | 7.50 MiB |
| Mixed large | 154.80 MiB | 514.00 MiB | 761.56 MiB | 387.53 MiB | 299.58 MiB |

## Findings and next priorities

1. **Validation fan-out is the first hot-path defect.** Materialization loads
   committed graph objects repeatedly and currently uses one Git read per
   artifact. Batch Git object reads or manifest/object-ID validation should
   make no-op materialization proportional to manifest/tree metadata, while
   retaining exact HEAD and fragment-integrity guarantees.
2. **One-file refresh is not incremental.** It clones, fully indexes,
   serializes, links, validates, and rematerializes the repository. Reuse
   unchanged committed fragments and rebuild only the affected linker frontier;
   never use an unrestricted merge-base or active-worktree source bytes.
3. **Atomic application is expensive only for cold full writes.** The large
   cold refresh spent 157 seconds applying thousands of files, while the
   one-file write set applied in 74 ms. Preserve exact planned writes; reduce
   the number of proposed changes rather than weakening transactional safety.
4. **Committed graph and redb footprint need budgets.** The large sample adds
   about 155 MiB to Git-visible artifacts and a 514 MiB local store. Before
   graph enrollment becomes default, define repository size guidance,
   compression/packing experiments, and disk-budget warnings.
5. **Explore is usable on small graphs but not yet an instant large-repo hot
   path.** The large warm query remained about 9.2 seconds and used roughly
   227 MiB peak RSS in its process. Profile query stages and retained
   allocations separately before changing ranking or semantic behavior.

No finding justifies modifying evaluation logic, scorer-facing output, or the
graph’s correctness contract. Graph generation should remain opt-in while the
large-repository validation, incremental refresh, footprint, and Explore
latency issues are addressed.
