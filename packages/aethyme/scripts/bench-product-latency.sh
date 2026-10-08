#!/usr/bin/env bash
# Run the checked-in Playground's non-gating product latency benchmarks.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
PRODUCT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
MANIFEST="$PRODUCT_ROOT/rust/Cargo.toml"
TARGET_DIR="$(cargo metadata --manifest-path "$MANIFEST" --no-deps --format-version 1 | jq -r .target_directory)"

if [[ "${AETHYME_BENCH_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --locked --manifest-path "$MANIFEST" --release \
    -p aethyme-cli --bin aethyme \
    -p aethyme-graph-indexer --bin aethyme-graph-index

  export AETHYME_BENCH_BIN="${AETHYME_BENCH_BIN:-$TARGET_DIR/release/aethyme}"
  export AETHYME_GRAPH_INDEX_BENCH_BIN="${AETHYME_GRAPH_INDEX_BENCH_BIN:-$TARGET_DIR/release/aethyme-graph-index}"
fi

: "${AETHYME_BENCH_BIN:?set AETHYME_BENCH_BIN to a release aethyme binary}"
: "${AETHYME_GRAPH_INDEX_BENCH_BIN:?set AETHYME_GRAPH_INDEX_BENCH_BIN to a release graph index binary}"
[[ -x "$AETHYME_BENCH_BIN" ]] || { printf 'missing release binary: %s\n' "$AETHYME_BENCH_BIN" >&2; exit 2; }
[[ -x "$AETHYME_GRAPH_INDEX_BENCH_BIN" ]] || { printf 'missing graph index binary: %s\n' "$AETHYME_GRAPH_INDEX_BENCH_BIN" >&2; exit 2; }

cargo bench --locked --manifest-path "$MANIFEST" -p aethyme-cli --bench product_latency -- "$@"
