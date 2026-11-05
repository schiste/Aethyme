# Allocator comparison — 2026-11-05

**Decision: keep the default allocator. Retain Rusty and mimalloc as optional
experimental build features; do not enable either globally from these results.**
The hosted comparison completed successfully on Linux x86_64 and macOS arm64:
108 workload cases, three repetitions per allocator, with matching checked
outputs. Numerical samples and provenance are retained in
[`allocator-comparison-2026-11-05.json`](allocator-comparison-2026-11-05.json).

Rusty is a plausible candidate for a dedicated indexing build: Django indexing
wall time fell 9.6% on Linux and 8.2% on macOS. That improvement did not extend
consistently to other work. macOS Django linking rose 8.5%, concurrent ripgrep
indexing rose 13.0%, and Linux short Explore commands added roughly 2–3 ms.
Incremental execution was essentially unchanged. macOS indexing CPU time rose
2.9% despite the lower wall time, so the observed speedup is not a universal
reduction in processing cost.

The most concrete tradeoff is Linux daemon retention: 10.8 MiB with the default
allocator, 18.8 MiB with Rusty, and 30.3 MiB with mimalloc after ten seconds idle.
macOS daemon retention was much closer (12.6, 13.0 and 12.8 MiB respectively).
Rusty's median request latency improved on both hosts, but macOS per-run p95
rose from 2.66 to 4.12 ms. These short bursts cannot establish production tail
behavior; that result is a reason to investigate rather than promise a benefit.

mimalloc was a little faster than Rusty on Linux Django indexing/linking and had
better macOS request latency, but it does not provide a consistent whole-product
win either. Its Linux idle daemon RSS was 2.8 times the default. Neither candidate
is a clear replacement for the platform allocator across this repository.

The optional implementation has a small integration footprint and preserves
normal builds. Its costs are additional low-level dependencies, another build
configuration to qualify, and modest binary growth: Rusty adds about 0.60 MiB
to the Linux router and 0.61 MiB on macOS; mimalloc adds about 0.13 and 0.11 MiB.
The experiment did not measure startup reliability, allocator failure paths, or
long-term fragmentation. If indexing throughput becomes a priority, extend that
workload's qualification before considering a targeted allocator default.

Percentages below are changes relative to the same host's default allocator;
negative means a lower measurement. Request p50/p95 rows are medians of the
three per-run percentiles, not percentiles pooled across all requests. Linux
command peak RSS is intentionally excluded because of the accounting defect
explained below.

## Linux results

Host: `Linux-7.0.0-1017-azure-x86_64-with-glibc2.39`. All entries are medians of three runs.

| Workload | Default | Rusty | mimalloc |
| --- | ---: | ---: | ---: |
| Django index | 3.765 s | 3.405 s (-9.6%) | 3.343 s (-11.2%) |
| Django link | 1.085 s | 0.930 s (-14.3%) | 0.882 s (-18.7%) |
| Django index CPU | 8.034 s | 7.553 s (-6.0%) | 7.384 s (-8.1%) |
| ripgrep index | 192.691 ms | 186.599 ms (-3.2%) | 176.181 ms (-8.6%) |
| Parcel index | 19.572 ms | 19.967 ms (+2.0%) | 23.084 ms (+17.9%) |
| Parcel first Explore | 12.651 ms | 15.990 ms (+26.4%) | 15.844 ms (+25.2%) |
| Parcel repeated Explore | 12.693 ms | 14.517 ms (+14.4%) | 15.343 ms (+20.9%) |
| Parcel incremental execution | 252.256 ms | 253.311 ms (+0.4%) | 248.189 ms (-1.6%) |
| Two concurrent ripgrep indexes | 381.487 ms | 369.245 ms (-3.2%) | 369.665 ms (-3.1%) |
| Daemon request p50 | 2.325 ms | 1.960 ms (-15.7%) | 2.046 ms (-12.0%) |
| Daemon request p95 | 2.458 ms | 2.084 ms (-15.2%) | 2.212 ms (-10.0%) |
| Daemon RSS after 10 s idle | 10.801 MiB | 18.836 MiB (+74.4%) | 30.293 MiB (+180.5%) |

Django index ranges: default 3.711–4.077 s, rusty 3.357–3.454 s, mimalloc 3.340–3.472 s.

## macOS results

Host: `macOS-26.7.2-arm64-arm-64bit`. All entries are medians of three runs.

| Workload | Default | Rusty | mimalloc |
| --- | ---: | ---: | ---: |
| Django index | 9.554 s | 8.774 s (-8.2%) | 8.982 s (-6.0%) |
| Django link | 2.400 s | 2.603 s (+8.5%) | 2.501 s (+4.2%) |
| Django index CPU | 13.088 s | 13.463 s (+2.9%) | 13.479 s (+3.0%) |
| ripgrep index | 438.997 ms | 455.036 ms (+3.7%) | 446.972 ms (+1.8%) |
| Parcel index | 91.366 ms | 85.119 ms (-6.8%) | 90.814 ms (-0.6%) |
| Parcel first Explore | 45.325 ms | 39.712 ms (-12.4%) | 38.538 ms (-15.0%) |
| Parcel repeated Explore | 36.664 ms | 37.196 ms (+1.5%) | 35.888 ms (-2.1%) |
| Parcel incremental execution | 826.975 ms | 826.860 ms (-0.0%) | 850.195 ms (+2.8%) |
| Two concurrent ripgrep indexes | 765.439 ms | 864.640 ms (+13.0%) | 892.365 ms (+16.6%) |
| Daemon request p50 | 2.185 ms | 1.905 ms (-12.8%) | 1.749 ms (-20.0%) |
| Daemon request p95 | 2.661 ms | 4.115 ms (+54.6%) | 2.137 ms (-19.7%) |
| Daemon RSS after 10 s idle | 12.594 MiB | 12.984 MiB (+3.1%) | 12.750 MiB (+1.2%) |

Django indexing peak RSS: default 274.7 MiB, rusty 286.1 MiB, mimalloc 288.5 MiB.

Django index ranges: default 9.543–10.561 s, rusty 8.571–9.111 s, mimalloc 8.837–9.043 s.

## Scope and method

The implementation adds mutually exclusive `allocator-rusty` and
`allocator-mimalloc` Cargo features to the router, engine daemon and standalone
graph tools. They select `rusty_alloc-api = 2.4.0` or `mimalloc = 0.1.52` at
executable boundaries. Defaults remain unchanged; library consumers do not
install a global allocator. Engine `dhat-heap` profiling cannot be combined
with either allocator feature.

Results come from [Actions run 42073849485](https://github.com/schiste/Aethyme/actions/runs/42073849485),
using source commit `73542d67072d269ca53388c4f97b3b28472ae2bb`, Rust 1.96.0,
release builds, and one Cargo build worker. All five executable hashes differ
across the three arms. The accompanying JSON preserves build identities,
individual numerical samples, source hashes, outcome hashes and report hashes.
Raw command receipts remain in the Actions artifacts (14-day retention) and the
local task archive `allocator-1896` outside disposable worktrees/build caches.

Each host ran three repetitions per allocator with rotating order, four Rayon
threads, isolated user/Git state, and untuned allocator defaults. Inputs were
source-only disposable Playgrounds:

| Input | Pinned revision | Indexed files / graph nodes |
| --- | --- | ---: |
| Parcel fixture | Included at the measured Aethyme commit | 16 / 47 |
| ripgrep | `91f5d22ef48bde459d90a8c7dbeab416e97d0f2b` | 151 / 4,691 |
| Django | `b3997c604720ff061e9d3588c86a9bf7ca07fa32` | 4,317 / 147,604 |

Archive hashes are pinned in the workflow. File coverage is partial for
ripgrep and Django, with the same coverage and graph outputs across allocators.
No evaluation indexes Aethyme itself.

Measurements cover separate indexing/linking on all inputs; materialization,
first/repeated Explore and one-file incremental refresh on Parcel; 100 repeated
symbol-batch requests followed by ten seconds idle on ripgrep; and two
concurrent ripgrep indexing processes. Medians compare allocators within the
same host. Three repetitions do not establish statistical confidence or
production tail latency. Filesystem caches were not flushed. A short repeated
query burst is not a long-running daemon or fragmentation soak.

## Memory measurement correction

The measured commit used direct Python `Popen`/`wait4` accounting. Linux child
peak RSS can retain the Python launcher's pre-exec high-water mark: many small
commands therefore showed an artificial floor around 84–86 MiB. **Linux command
RSS and concurrent summed peaks are excluded from the comparative conclusions.**
Even though Django indexing exceeded that floor, this report does not depend on
its Linux peak-RSS values. The JSON retains those observations under
`raw_wait4_peak_rss_bytes`, clearly identified as diagnostic values.

Daemon RSS was sampled directly from the live daemon PID with `ps`; it is
independent of that defect and is retained on both platforms. The macOS
large-parent/small-child control did not reproduce the Linux contamination.

The maintained runner now measures application RSS through a small native
`/usr/bin/time` supervisor and retains the supervisor's `wait4` peak separately.
With a touched 128 MiB parent allocation, the Linux negative control reported
159,092,736 bytes for an 8 MiB allocating child; the corrected runner reported
15,728,640 bytes. Ten controls pass on local macOS and Linux arm64, including
this isolation check, units, success, failure and timeout handling. These are
measurement controls, not additional allocator-performance samples.

**The correction does not retroactively change the measured run.** Future runs
include the native supervisor's startup in wall/CPU time, so compare variants
within one protocol rather than mixing old and new harness measurements.

## Reproduction and evidence interface

The existing non-gating `product-performance.yml` workflow accepts
`allocator_comparison=true` and `allocator_repetitions=3` or `6`. It runs
independent macOS and Linux jobs. Ordinary scheduled performance behavior is
unchanged when allocator comparison is false.

`build_allocator_matrix.py --repo PATH --output NEW_DIR --target-dir PATH`
requires a broker resource lease. `--isolated-ci` explicitly permits a
GitHub-hosted Actions runner. It writes `build.json` with the source commit,
compiler, platform, admission mode, exact build argv, status, duration and a
binary path/SHA-256/size mapping for each arm. Failed or timed-out builds stop
the matrix and retain their receipts.

`allocator_matrix.py --build BUILD_JSON --source LABEL=PATH --output NEW_DIR`
accepts `--stage index|product|daemon|concurrent`, `--repetitions N`, and
`--requests N`. Inputs must be source-only repositories; executable hashes
must match the manifest. Output directories must not already exist.

`report.json` records platform, build/runner/source hashes, thread count, stage,
repetition count and successful `samples`. Only a finished matrix has
`complete: true`. Receipts include original `argv`, `measurement_argv`, cwd,
wall/user/system seconds, `peak_rss_bytes`, `supervisor_peak_rss_bytes`, exit
status, timeout, load and stdout/stderr paths. `rss_measurement` is
`native-time-child`; `resource_error` is null on successful measurement, or a
string with `peak_rss_bytes: null` when accounting failed. Such failures stop
the run; `commands.jsonl` still retains their receipts. The original measured
commit lacks the newly added measurement/supervisor/error fields.

Daemon samples add individual request latencies and live RSS before/after the
burst and after idle intervals. Concurrent samples retain both child receipts
and group wall time; summed per-process high-water marks are not simultaneous
total RSS. Index/link checks compare graph filenames/bytes and non-timing
summaries. Explore checks compare path-normalized answer/navigation/evidence
fields; incremental refresh checks graph counts; daemon checks response hashes.
Mismatches stop the run and never become valid timing samples.

## Validation and remaining limits

Both hosted platforms built the three allocator variants and completed all
performance stages with matching checked outputs. All six deliberate conflicting-
feature builds refused with the expected diagnostics. The measured implementation
passed the ordinary Rust CI suite, formatting/clippy, examples/doctests,
Windows default-build smoke check, no-Python product path and contract check.
The final harness correction passes ten controls on macOS and Linux; workflow
`actionlint` and `git diff --check` pass.

The first local full release build timed out after 40 minutes on the shared
Apple M4 host. Same-binary smoke runs and that failed build are excluded from
allocator comparisons. Local broker submission was deferred by shared-host
load/build time limits; it is not reported as local gate verification.

Remaining qualification before any default switch: longer multi-repository
runs, cold-process/cache-controlled replication, realistic concurrent daemon
traffic, long idle/fragmentation retention, application-only Linux peak RSS
with the corrected runner, and supported Windows allocator builds. Matching
outputs in these workloads does not establish general allocator correctness,
OOM behavior, or production reliability.
