# Product latency baseline (2026-10-08)

Last Updated: 2026-10-08

Reference product commit: `999f5cb41d9843b49f1e489c2d44dcd609d2f1fc`
Fixture tree SHA: `4e0655106d7c432c403eb8cfa3cfda35079b1446`

The `product-performance` workflow reads the two lines above. It builds `aethyme` and
`aethyme-graph-index` from the reference commit, then benchmarks those binaries and the current
checkout's binaries against the same checked-in Playground
(`packages/aethyme-eval/benchmarks/performance/fixture/`). If the fixture's tree SHA no longer
matches, it refuses to compare. Refresh this report when the fixture changes, or when a reviewed
change is meant to move these numbers.

## Recorded run

These numbers come from `packages/aethyme/scripts/bench-product-latency.sh --save-baseline
product-reference` (Criterion 0.5, 10 samples, 10 s measurement, 2 s warm-up, release build).
The measured product crates are byte-identical to the reference commit; the run only added the
benchmark harness.

- **Machine:** Apple M4, 10 cores, 16 GiB.
- **OS and toolchain:** macOS 27.2, rustc 1.96.0.
- **Conditions:** a developer laptop under ordinary load (memory pressure was present), not a quiet host.

| Benchmark | Lower bound | Estimate | Upper bound |
| --- | --- | --- | --- |
| `explore_cold_process` (first Explore process after a fresh graph) | 112.52 ms | 128.05 ms | 149.39 ms |
| `explore_warm_store` (repeated Explore on the same warm store) | 57.45 ms | 67.43 ms | 80.51 ms |
| `verify_targets` (two targets from a saved Explore answer) | 7.08 ms | 7.41 ms | 7.89 ms |
| `graph_index` (fresh index of the Playground) | 165.36 ms | 176.48 ms | 186.58 ms |

## Gating policy

None of these benchmarks gates. Process launch, OS scheduling and host load move these timings
by tens of percent on the same machine, which is wider than most regressions worth catching.

- The nightly and manual workflow runs both revisions on the same runner and uploads Criterion's
  comparison as an artifact for review.
- Nothing in it fails a pull request.
- Do not replace this report from a single noisy sample: a threshold set from one run is one
  that everybody learns to ignore.
