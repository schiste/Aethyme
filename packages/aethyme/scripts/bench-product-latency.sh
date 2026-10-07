#!/bin/zsh
# Run the checked-in Playground's non-gating product latency benchmarks.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
PRODUCT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
WORKSPACE_ROOT="$(cd "$PRODUCT_ROOT/../.." && pwd -P)"
MANIFEST="$PRODUCT_ROOT/rust/Cargo.toml"
TARGET_DIR="$(cargo metadata --manifest-path "$MANIFEST" --no-deps --format-version 1 | jq -r .target_directory)"

cargo build --manifest-path "$MANIFEST" --release \
  -p aethyme-cli --bin aethyme \
  -p aethyme-graph-indexer --bin aethyme-graph-index

export AETHYME_BENCH_BIN="$TARGET_DIR/release/aethyme"
export AETHYME_GRAPH_INDEX_BENCH_BIN="$TARGET_DIR/release/aethyme-graph-index"
[[ -x "$AETHYME_BENCH_BIN" ]] || { print -u2 "missing release binary: $AETHYME_BENCH_BIN"; exit 2; }
[[ -x "$AETHYME_GRAPH_INDEX_BENCH_BIN" ]] || { print -u2 "missing graph index binary: $AETHYME_GRAPH_INDEX_BENCH_BIN"; exit 2; }

cargo bench --manifest-path "$MANIFEST" -p aethyme-cli --bench product_latency -- "$@"
