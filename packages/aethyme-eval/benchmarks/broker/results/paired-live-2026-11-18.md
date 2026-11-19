# Live paired broker comparison, 2026-11-18

Aethyme 0.8.129 (build `55692c0946976d8dab618278f035b4fc9fd0ae55`) took
23.9% longer at the median and 50.7% longer at p90 than direct Git for the
ten tasks in this run. Both arms completed every task and repaired one
textual conflict. Neither needed a human intervention.

## Method

[The live paired runner](../paired_live.py) ran four solo tasks and three
concurrent pairs. Two actual Codex workers ran at the same time in each
pair. Seed 19 randomized group order and which arm went first. Both arms
used Codex CLI 0.168.0, `gpt-6-sol` with low reasoning, the same disposable
Playground, isolated worktrees, owned paths, exact published-head checks,
deterministic integration order, and one allowed repair. Final verification
ran the visible suite, all assigned hidden checks, and the joint hidden
check for each concurrent pair. The broker binary SHA-256 was
`f244c6e14661b3144318ef7c10fd8afa9b5429a6a00bb5d6e338c77035cf60a9`.

Task time covers worktree setup through integration, including order waits,
repairs, and broker session closure. Input tokens use Codex's native usage
records and were split into cached and uncached before combining attempts.
The runner captured command stdout bytes separately. The study used local
bare remotes, disabled update checks, and installed no Aethyme hooks. It
measures this explicit coordination workflow. Automatic hook delivery,
network latency, and duplicate work outside the assigned pairs require
separate trials.

## Results

Each cell gives median / nearest-rank p90. The all-task comparison uses 10
matched tasks per arm. Solo and concurrent p90 are maxima at these sample
sizes. The concurrent tasks share three independent group runs per arm.

| Cases | Measure | Direct Git | Aethyme | Change |
| --- | --- | ---: | ---: | ---: |
| All | Time, seconds | 35.05 / 41.27 | 43.42 / 62.20 | +23.9% / +50.7% |
| All | Uncached input tokens | 12,111 / 23,000 | 14,612.5 / 25,616 | +20.7% / +11.4% |
| All | Cached input tokens | 141,760 / 178,816 | 204,736 / 336,000 | +44.4% / +87.9% |
| All | Output tokens | 1,146.5 / 1,493 | 1,359 / 2,128 | +18.5% / +42.5% |
| Solo | Time, seconds | 33.01 / 39.28 | 44.93 / 59.24 | +36.1% / +50.8% |
| Solo | Uncached input tokens | 11,454.5 / 23,000 | 14,211.5 / 30,700 | +24.1% / +33.5% |
| Solo | Cached input tokens | 134,400 / 142,848 | 202,624 / 254,080 | +50.8% / +77.9% |
| Solo | Output tokens | 1,122.5 / 1,493 | 1,447 / 2,128 | +28.9% / +42.5% |
| Concurrent | Time, seconds | 35.87 / 56.54 | 43.42 / 122.59 | +21.1% / +116.8% |
| Concurrent | Uncached input tokens | 12,572.5 / 23,798 | 14,940 / 25,616 | +18.8% / +7.6% |
| Concurrent | Cached input tokens | 159,040 / 245,248 | 216,960 / 544,128 | +36.4% / +121.9% |
| Concurrent | Output tokens | 1,146.5 / 1,849 | 1,359 / 3,029 | +18.5% / +63.8% |

The pairs' elapsed times from concurrent launch through both integrations
were 36.03 versus 60.66 seconds for the shared file, 56.48 versus 121.27 for
the textual overlap, and 41.17 versus 42.81 for the semantic overlap
(Direct Git first). Each arm detected and resolved the same textual conflict
during integration. The semantic and shared-file pairs passed their joint
checks without repair.

## Broker command attribution

Durations are milliseconds and the columns show median / p90. Command
telemetry measures elapsed time over the process phases. Aggregated medians
and percentiles cannot be added. The worker record does not contain equivalent
per-command timing for direct Git.

| Command | Calls | Elapsed | Broker and unattributed | Child work | Queue wait | Stdout bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `start` | 10 | 1,482 / 2,051 | 683 / 1,487 | 786 / 1,083 | 1 / 2 | 6,840 / 7,481 |
| `status` | 15 | 382 / 646 | 178 / 271 | 218 / 342 | 0 / 0 | 3,877 / 4,354 |
| `advance` | 11 | 1,292 / 1,463 | 356 / 470 | 873 / 1,074 | 32 / 56 | 387 / 387 |
| `leases` | 2 | 3,455 / 6,381 | 2,637 / 5,185 | 818 / 1,196 | 0 / 0 | 379 / 445 |
| Detached `gc` | 2 | 49,986 / 62,716 | 12,898 / 17,058 | 37,086 / 45,658 | 0 / 1 | 0 / 0 |

Telemetry recorded zero retries. All ten `finish` receipts confirmed
closure, but the command log contained no `finish` timings. Detached GC
overlaps foreground work and its duration cannot be charged as foreground
wait. About three seconds of typical broker call time per task explains only
part of the observed gap. The slowest textual task took 122.59 seconds with
Aethyme versus 56.54 with Git. Its two Aethyme attempts executed 19 shell
calls, seven invoking the broker, versus 11 shell calls with Git. The sample
does not isolate model time from tool use or establish the background
worker's impact. The two additional lease calls occurred in the shared-file
pair, including one 6.38-second call. This is a separate source of overhead.

The November 17 comparison used a prescribed peer instead of two workers
running concurrently. Differences in protocol and trial composition limit
comparisons across dates. This report also covers a small, fast repository
with a controlled integration coordinator in both arms. A hook-enabled
trial with more independent pairs and a wider range of task lengths would
measure how automatic delivery and avoided rework change the result.
