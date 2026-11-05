# Allocator comparison — 2026-11-05

Status: experiment in progress. No default allocator change is proposed.

## Scope

Compare the Rust default allocator with `rusty_alloc-api = 2.4.0` and
`mimalloc = 0.1.52` using release builds of the router, engine daemon and
standalone graph tools. The two optional Cargo features are
`allocator-rusty` and `allocator-mimalloc`; they are mutually exclusive.
Engine heap profiling (`dhat-heap`) cannot be combined with either one.
Library consumers do not install an allocator through these features.

All inputs are disposable source-only Playgrounds: the existing Parcel
fixture, Django at `b3997c604720ff061e9d3588c86a9bf7ca07fa32`, and ripgrep at
`91f5d22ef48bde459d90a8c7dbeab416e97d0f2b`. No evaluation indexes Aethyme itself.

## Execution

The existing non-gating `product-performance.yml` workflow accepts
`allocator_comparison=true` and `allocator_repetitions=3` or `6`. It runs
independent macOS and Linux jobs with Rust 1.96.0 and one Cargo build worker.
The ordinary scheduled performance job is unchanged when this input is false.
Each job builds all three allocators before measuring, rotates their order
between repetitions, fixes Rayon at four threads, isolates user and Git state,
and validates output equality. Source archive hashes are pinned in the workflow.

Measurements cover separate index/link commands on all three Playgrounds;
materialization, first/repeated Explore and one-file incremental refresh on
Parcel; 100 repeated symbol-batch requests and ten seconds of idle retention
on ripgrep; and two concurrent ripgrep indexing processes. A repeated-query
burst is not a production-length daemon soak. Filesystem caches are not flushed.

The local Apple M4 build attempt hit its 40-minute timeout under heavy shared
host load. That attempt and same-binary harness smoke checks are excluded from
allocator performance results. Hosted-runner results will be recorded below.

## Harness interface and evidence schema

`build_allocator_matrix.py --repo PATH --output NEW_DIR --target-dir PATH`
requires a broker resource lease. `--isolated-ci` explicitly permits a
GitHub-hosted Actions runner. The helper writes `build.json` with the source
commit, compiler, platform, admission mode, and an `arms` mapping. Each arm has
its exact build argv, exit status, duration and binary path/SHA-256/size mapping.
Failed or timed-out builds are retained and stop the matrix. Cache-service
wrappers are disabled so compile children remain in the supervised group.

`allocator_matrix.py --build BUILD_JSON --source LABEL=PATH --output NEW_DIR`
accepts `--stage index|product|daemon|concurrent`, `--repetitions N`, and
`--requests N`. Inputs must be source-only repositories; binary hashes must
match the supplied manifest. Output directories must not already exist.

`report.json` records the platform, build and runner hashes, source hashes,
thread count, stage, repetition count and successful `samples`. Only a finished
matrix has `complete: true`; missing completion means unavailable results.
Each command has argv, cwd, wall/user/system seconds, peak RSS in bytes, exit
status, timeout flag, load average and retained stdout/stderr paths.
`commands.jsonl` preserves receipts even for rejected command failures.
Daemon samples add individual request latencies and RSS before/after the burst
and after idle intervals. Concurrent samples retain both child receipts and
group wall time; summed process high-water marks are not simultaneous total RSS.

Index and link checks compare graph filenames/bytes and non-timing summaries.
Explore checks compare path-normalized answer/navigation/evidence fields.
Incremental refresh checks compare graph counts; daemon checks compare complete
response hashes. A mismatch stops the run; failed samples are never timings.
CI artifacts retain raw evidence for 14 days; durable copies and conclusions
are recorded with the completed report.

## Validation and results

Pending hosted comparison. Measurement controls exercise successful children,
nonzero exits, timeout cleanup, semantic mismatches, graph-content changes,
environment isolation, and local/CI build admission.
