# Writable broker startup, 2026-11-18 and 2026-11-19

A recent artifact sweep can prove that another inline pass is premature
even under disk pressure. The candidate uses that record before sampling
disk headroom. With a recent sweep, the named inline phase dropped from
21–24 ms median to under 1 ms. The larger wall-clock differences below
include host and subprocess variability.

## Attribution

Twelve initial instrumented Playground trials recorded these recovery
durations in milliseconds. Recovery phases are nested.

| Phase | Warm start median / p90 | Advance median / p90 |
| --- | ---: | ---: |
| GC maintenance | 129 / 158 | 183 / 254 |
| Inline artifact sweep | 90 / 112 | 145 / 194 |
| Cache rotation recovery check | 36 / 45 | 39 / 49 |
| Detached worker admission | 0 / 1 | 1 / 2 |

Contract backfill, operation outcome replay, dead prepared-operation reap,
repeatable fetch recovery, integration reconciliation, retention policy,
and confirmed-journal checks each rounded to 0 ms at p90 on these healthy
fixtures. Fault recovery remains mandatory when there is work to recover.

## Paired comparison

[The profiler](../profile_open.py) interleaves both builds using seed 104 on
macOS arm64. Both binaries used the same local Rust dev profile with debug
symbols disabled. The instrumented control was built from `05820c178`.
The built router SHA-256 values were
`dad56f96a72ff1aec4f733a4fc2defb764ec5ce0d9eb0931c3ac0fb7da93f853`
and `a068b7657291ff4cd217b237c2fa9243bff0d6a60ef2859b6ddc4718e915002e`
(control first).

The broker Playground uses fresh isolated stores and local bare remotes.
They live under the broker process's temporary directory so detached
workers cannot outlive the trial. Recent-sweep trials use a real `gc sweep`
after closing a retained worktree. The profiler verifies that the sweep
removed the seeded artifact. For concurrent trials, advance runs in two
different owned worktrees with disjoint paths. The profiler verifies each
exact remote branch head, integrates both and confirms session closure.

| State | Operation | Calls per arm | Control median / p90 (ms) | Candidate median / p90 (ms) |
| --- | --- | ---: | ---: | ---: |
| Cold | Start | 12 | 1,024 / 2,492 | 1,067 / 1,780 |
| No recent sweep | Warm start | 12 | 644 / 847 | 644 / 785 |
| No recent sweep | Advance | 12 | 919 / 1,269 | 950 / 1,214 |
| No recent sweep | Concurrent advance | 12 | 1,021 / 2,997 | 1,084 / 1,351 |
| Recent completed sweep | Warm start | 12 | 404 / 650 | 363 / 433 |
| Recent completed sweep | Advance | 12 | 609 / 1,223 | 567 / 742 |
| Recent completed sweep | Concurrent advance | 12 | 1,003 / 1,460 | 708 / 1,139 |

The concurrent rows represent six independent pairs per arm. All 24 warm
candidate openings after a completed sweep took the shortcut in the
sequential trials. It also fired in all 12 simultaneous candidate advances.
Inline time fell from 24 / 35 ms to 0 / 0 ms on warm start, 21 / 37 ms
to 0 / 0 ms on advance, and 22 / 45 ms to 0 / 1 ms on concurrent advance.
The median paired wall differences were −18 ms for warm start, −71 ms
for advance and −340 ms for the slower call in each simultaneous pair.
These small trials isolate the skipped phase. They provide limited evidence
about tail latency or the share of real agent sessions that qualify. The
no-recent-sweep controls showed no repeatable wall-time improvement.

The optimization applies to writable open within the shortest configured
interval. Once that interval elapses, the broker still samples headroom
and chooses the original cadence. The bounded inline pass, confirmed
journal replay, interrupted operations, and detached worker admission keep
their existing execution order and authority.

Reproduce the comparison with matching local builds, using all four
combinations of `--recent-sweep` and `--concurrent`:

```sh
python3 packages/aethyme-eval/benchmarks/broker/profile_open.py \
  --scratch /path/to/isolated/scratch --output /path/to/result.json \
  --arm control=/path/to/control/aethyme --arm candidate=/path/to/candidate/aethyme
```
