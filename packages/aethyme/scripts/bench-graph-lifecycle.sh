#!/bin/zsh
# Build disposable graph-enabled Playground clones and emit one reviewed JSON report.

set -euo pipefail

AETHYME_BIN="${AETHYME_BIN:-aethyme}"
PROGRAM="$0"
OUTPUT="-"
KEEP=false
typeset -a SOURCES

usage() {
    print "usage: $PROGRAM --source <label=git-url-or-local-path> [--source ...] [--aethyme <binary>] [--output <path>] [--keep]"
}

while (( $# > 0 )); do
    case "$1" in
        --source) SOURCES+=("$2"); shift 2 ;;
        --aethyme) AETHYME_BIN="$2"; shift 2 ;;
        --output) OUTPUT="$2"; shift 2 ;;
        --keep) KEEP=true; shift ;;
        --help|-h) usage; exit 0 ;;
        *) print -u2 "unknown option: $1"; usage >&2; exit 2 ;;
    esac
done

if (( ${#SOURCES[@]} == 0 )); then
    print -u2 "at least one --source label=location is required"
    exit 2
fi
for command_name in git jq "$AETHYME_BIN"; do
    command -v "$command_name" >/dev/null || {
        print -u2 "required command is unavailable: $command_name"
        exit 2
    }
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
PRODUCT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
WORKSPACE_ROOT="$(cd "$PRODUCT_ROOT/../.." && pwd -P)"
PRODUCT_GIT_COMMON="$(git -C "$WORKSPACE_ROOT" rev-parse --path-format=absolute --git-common-dir)"
RUN_ROOT="$(mktemp -d -t aethyme-graph-benchmark.XXXXXX)"
RESULTS="$RUN_ROOT/results.jsonl"
: > "$RESULTS"

cleanup() {
    if [[ "$KEEP" == true ]]; then
        print -u2 "kept benchmark Playgrounds at $RUN_ROOT"
    else
        command rm -rf -- "$RUN_ROOT"
    fi
}
trap cleanup EXIT INT TERM

file_bytes() {
    [[ -f "$1" ]] && wc -c < "$1" | tr -d ' ' || print 0
}

directory_bytes() {
    [[ -d "$1" ]] && du -sk "$1" | awk '{ print $1 * 1024 }' || print 0
}

for specification in "${SOURCES[@]}"; do
    if [[ "$specification" != *=* ]]; then
        print -u2 "invalid --source $specification; expected label=location"
        exit 2
    fi
    label="${specification%%=*}"
    source_location="${specification#*=}"
    if [[ -z "$label" || -z "$source_location" || "$label" == *[^A-Za-z0-9._-]* ]]; then
        print -u2 "invalid Playground label or source: $specification"
        exit 2
    fi

    if [[ -d "$source_location" ]]; then
        source_root="$(git -C "$source_location" rev-parse --show-toplevel 2>/dev/null || true)"
        if [[ -n "$source_root" ]]; then
            source_git_common="$(git -C "$source_root" rev-parse --path-format=absolute --git-common-dir)"
            if [[ "$source_git_common" == "$PRODUCT_GIT_COMMON" ]]; then
                print -u2 "refusing to benchmark Aethyme against itself: $source_location"
                exit 2
            fi
        fi
    elif [[ "${source_location:l}" == *"/aethyme.git" || "${source_location:l}" == *"/aethyme" ]]; then
        print -u2 "refusing an Aethyme source as a performance Playground"
        exit 2
    fi

    repo="$RUN_ROOT/$label"
    print -u2 "benchmark[$label]: cloning and enrolling Playground"
    git clone --quiet --no-local "$source_location" "$repo"
    git -C "$repo" config user.name "Aethyme Benchmark"
    git -C "$repo" config user.email "benchmark@example.invalid"
    source_head="$(git -C "$repo" rev-parse HEAD)"

    "$AETHYME_BIN" deploy --repo "$repo" --with-graph \
        --graph-repository "playground/$label" >/dev/null
    git -C "$repo" add -A
    git -C "$repo" commit -qm "chore(benchmark): enroll graph evidence"

    cold_plan="$RUN_ROOT/$label-cold-plan.json"
    print -u2 "benchmark[$label]: cold refresh"
    "$AETHYME_BIN" graph refresh plan --repo "$repo" --json > "$cold_plan"
    cold_digest="$(jq -r '.plan_sha256' "$cold_plan")"
    cold_refresh="$RUN_ROOT/$label-cold-refresh.json"
    "$AETHYME_BIN" graph refresh execute --repo "$repo" --confirm "$cold_digest" --json > "$cold_refresh"
    git -C "$repo" add -- .aethyme/graph
    git -C "$repo" commit -qm "chore(benchmark): commit graph snapshot"

    command rm -f -- "$repo/.aethyme/graph_store.redb" "$repo/.aethyme/graph_store.redb.indexing"
    print -u2 "benchmark[$label]: cold and no-op materialization"
    cold_materialization="$RUN_ROOT/$label-cold-materialization.json"
    "$AETHYME_BIN" graph materialize --repo "$repo" --json > "$cold_materialization"
    noop_materialization="$RUN_ROOT/$label-noop-materialization.json"
    "$AETHYME_BIN" graph materialize --repo "$repo" --json > "$noop_materialization"

    cold_explore="$RUN_ROOT/$label-cold-explore.json"
    print -u2 "benchmark[$label]: cold and warm Explore"
    warm_explore="$RUN_ROOT/$label-warm-explore.json"
    "$AETHYME_BIN" explore --repo "$repo" --request "locate the primary application entrypoint" \
        --format answer-json --show-observability --depth 0 > "$cold_explore"
    "$AETHYME_BIN" explore --repo "$repo" --request "locate the primary application entrypoint" \
        --format answer-json --show-observability --depth 0 > "$warm_explore"

    probe="$repo/aethyme-graph-performance-probe.md"
    print -u2 "benchmark[$label]: one-file refresh"
    print "# Aethyme graph performance probe" > "$probe"
    git -C "$repo" add aethyme-graph-performance-probe.md
    git -C "$repo" commit -qm "test(benchmark): add one-file graph probe"
    one_file_plan="$RUN_ROOT/$label-one-file-plan.json"
    "$AETHYME_BIN" graph refresh plan --repo "$repo" --json > "$one_file_plan"
    one_file_digest="$(jq -r '.plan_sha256' "$one_file_plan")"
    one_file_refresh="$RUN_ROOT/$label-one-file-refresh.json"
    "$AETHYME_BIN" graph refresh execute --repo "$repo" --confirm "$one_file_digest" --json > "$one_file_refresh"

    graph_bytes="$(directory_bytes "$repo/.aethyme/graph")"
    redb_bytes="$(file_bytes "$repo/.aethyme/graph_store.redb")"
    jq -n \
        --arg label "$label" \
        --arg source_head "$source_head" \
        --argjson cold_plan "$(jq '.performance' "$cold_plan")" \
        --argjson cold_refresh "$(jq '.performance' "$cold_refresh")" \
        --argjson one_file_refresh "$(jq '.performance' "$one_file_refresh")" \
        --argjson cold_materialization "$(jq '.performance' "$cold_materialization")" \
        --argjson noop_materialization "$(jq '.performance' "$noop_materialization")" \
        --argjson cold_explore "$(jq '.observability.performance' "$cold_explore")" \
        --argjson warm_explore "$(jq '.observability.performance' "$warm_explore")" \
        --argjson graph_bytes "$graph_bytes" \
        --argjson redb_bytes "$redb_bytes" \
        '{label: $label, source_head: $source_head,
          cold_refresh_plan: $cold_plan, cold_refresh: $cold_refresh,
          one_file_refresh: $one_file_refresh,
          cold_materialization: $cold_materialization,
          noop_materialization: $noop_materialization,
          cold_explore: $cold_explore, warm_explore: $warm_explore,
          disk: {committed_graph_bytes: $graph_bytes, redb_bytes: $redb_bytes}}' >> "$RESULTS"
done

report="$RUN_ROOT/report.json"
jq -s \
    --arg aethyme_version "$("$AETHYME_BIN" --version | head -1)" \
    --arg platform "$(uname -sm)" \
    '{schema_version: 1, methodology: {
        isolation: "disposable Playground clone per source",
        cold_explore: "first Explore process after cold materialization; OS caches are not forcibly purged",
        warm_explore: "immediate identical second Explore process",
        one_file_refresh: "one committed Markdown probe added after the cold snapshot",
        aethyme_version: $aethyme_version, platform: $platform
      }, repositories: .}' "$RESULTS" > "$report"

if [[ "$OUTPUT" == "-" ]]; then
    command cat "$report"
else
    cp "$report" "$OUTPUT"
    print "wrote graph performance report to $OUTPUT"
fi
