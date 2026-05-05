# Engine latency: where 14s actually goes

Date: 2026-05-05
Trigger: After shipping the response trim (`671cb75`) and `aethyme` Rust
binary + Python daemon (`ff1ac47`), per-call cost was 4,200 tokens / ~14s
warm. Token side is now in good shape; latency demanded measurement
before the next architectural choice.

## Method

Added `--profile` to `aethyme-engine-cli task-localize` (`387b8d8`):
prints stage timings to stderr. Ran it three times against MediaWiki -
Aethyme (12,493 files, parse cache + graph store warm on disk).

## Numbers

```
run #1: map_build=119477  task_parse=0  anchors=412   scope=16937  next=17176  json_render=1   total=154006ms
run #2: map_build=95589   task_parse=0  anchors=710   scope=17634  next=16385  json_render=3   total=130325ms
run #3: map_build=108581  task_parse=0  anchors=413   scope=16532  next=15447  json_render=1   total=140977ms
```

## Two facts that explain everything

### 1. `RepositoryMap::build` dominates at 73-78%

Even with the Phase 3 redb parse cache warm on disk (2.08 GB, hash-keyed
to skip re-parsing files), the Rust engine reconstructs the in-memory
`RepositoryMap` on every invocation. 95-119 seconds of CPU per call,
spent rebuilding the same Rust struct from the same on-disk bytes.

This is the single highest-leverage perf finding: **on a 12.5K-file
PHP repo, every engine call rebuilds 100+ seconds of in-memory state
that should be persistent.**

### 2. The Python `aethyme explore` wrapper sets a 1-second budget

`src/cli.py:1898-1902` calls `task_localize` with
`timeout_seconds=graph_query_timeout_ms / 1000` (default 1.0s).
On MediaWiki the engine never finishes within that budget, so the
Python wrapper records a `degraded_reason` and falls back to cheap
substitutes:

```
degraded_reasons:
  - task-localize skipped: Rust engine timed out after 1.0s
  - symbol batch search skipped: Rust engine timed out after 1.0s
```

The 14s `aethyme explore` measurement is the **fallback path** —
filesystem-filename, source-text grep (`grep -rn`), source-callsite
expansion. The graph-based localization that is Aethyme's actual
leverage **never runs on MediaWiki at default settings**.

## What this means

This reframes everything we observed in the dead-code evals:

- The "Aethyme conditions" in the eval did not actually use Aethyme's
  graph navigation. The agent invoked `aethyme explore`; Python
  computed the answer from text-grep + filename matching; the agent
  got numbers that look comparable to brute-grep because it was
  effectively brute-grep wrapped in a JSON envelope.
- The "explore won 4.5x cheaper" headline from the first run was
  strategy variance precisely because *every* condition was running
  approximately the same algorithm.
- The cost-per-call we just optimized (4,200 tokens / 14s) is the
  cost of the fallback, not of Aethyme. Aethyme's graph-based answer
  on this repo is currently *unreachable* in default budget.

## Two intervention paths

### A. Engine daemon (eliminates map_build cost across calls)

Make `aethyme-engine-cli` server-capable: hold `RepositoryMap` in
memory, listen on a socket, serve `task-localize` / `task-anchors` /
`task-next` / `symbol-batch` / etc. as RPCs. First call pays the
~100s map_build; every subsequent call drops to the per-stage cost
(~0.5-17s for scope/next, ~0.5s for anchors).

Estimated end-to-end after this: `aethyme explore` lands at 1-3s
because the Python wrapper's 1s budget can now actually be met by the
engine, AND the fallback cost drops because we're no longer in
fallback mode.

This is the headline fix. Substantial work — needs a server mode for
the engine binary (current binary is one-shot subcommand dispatch),
in-memory map lifecycle, multi-client serialization, eviction
policy. Probably a day or two of focused Rust + integration work.

### B. Optimize `RepositoryMap::build` so it doesn't take 100s

The fact that rebuild is 100s on a 12.5K-file repo is itself
suspicious. Phase 3 redb made the parse cache fast; map_build still
walks the whole cache per call. There's likely an O(N²) pattern or
unnecessary recomputation. A profiling pass on `RepositoryMap::build`
(criterion bench + flamegraph) would tell us whether 100s is
fundamental or 10x-able.

If we can drop map_build to ~10s, daemon mode is less urgent —
calls land in 5-10s and many users won't need warm state. If
map_build is fundamentally 100s, daemon mode is the only path.

### C. Increase the Python timeout for users who want graph results

Quick knob, no code change to engine. `--params
'{"graph_query_timeout_ms":120000}'` would let task-localize run on
MediaWiki at 120-150s per call. Useful for offline analysis;
unusable for agent flows. Not really a fix — just a workaround.

## Recommendation

Do (B) first: profile `RepositoryMap::build` with a flamegraph.
30-60 minutes of work, possibly drops map_build to 10-20s. If yes,
that alone makes Aethyme viable on MediaWiki at default budgets.

If (B) reveals fundamental cost, do (A): engine daemon. Larger but
clear architecture path.

## What did NOT exist that we expected

For the record, before this session:

| Looking for | Found |
|---|---|
| `tracing::` instrumentation | None — `tracing` not in `Cargo.toml` |
| Criterion / bench harnesses | None — no `[[bench]]` entries, no `benches/` dir |
| Per-stage timers in engine | One-off `Instant` prints for indexing only |
| Engine daemon mode | None — single-shot binary today |
| RepositoryMap caching | None — rebuild per call by design |
| Answer-cache layer | None — every request is recomputed |

So every intervention in this space is net-new infrastructure. The
flamegraph + map_build profile is the smallest possible next step.
