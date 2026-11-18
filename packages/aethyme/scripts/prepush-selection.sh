#!/usr/bin/env bash
# Print the nextest filterset the pre-push tier runs for the current change.
#
# Usage, from anywhere in the checkout:
#
#     packages/aethyme/scripts/prepush-selection.sh [--base REF] [--data FILE]
#
# Three tiers share the work (`.aethyme/gates.toml`, `aethyme-gates.yml`):
# pre-push runs this selection locally, a push to main runs the changed crates
# and their reverse dependencies, and a release runs the whole workspace.
#
# The selection is
#   1. every test in `always_binaries` and `always_modules`: the repository
#      lint suites and the modules with the most failing CI runs per second of
#      test time (about 4% of suite time), then
#   2. the test modules the change touches, cheapest first, until the summed
#      test time reaches `budget_seconds` (15% of the suite). A module with no
#      recorded time is new, so it costs nothing here and always runs.
#   3. minus `slow_tests`, each over `slow_seconds` (5 s) in CI: 128 tests
#      holding 23% of suite time, which run at push instead. Two of them took
#      over a minute each under local load and set this tier's wall time.
#
# Measured on 187 test-failing pull-request runs from 2026-10-20 to 2026-11-08
# (the always-run modules were chosen from earlier runs only): this selection
# caught 89% of them at a mean of 10% of suite time. A fixed list alone caught
# 44%; the changed modules alone cost 26% on average and up to the whole suite.
#
# Cargo's workspace targets identify the current test binaries. A changed
# source module selects the library (or the main binary for a bin-only
# package), an explicit binary/test root selects that target, and shared
# integration modules select the package's declared it suite if present.
# Crate roots and shared test support are handled by the push and release
# tiers and the always-run tests.
#
# Modules left out by the budget are named on stderr. The filterset goes to
# stdout and is never empty.
set -euo pipefail

gate_base="${AETHYME_GATE_BASE_COMMIT:-}"
base="${gate_base:-origin/main}"
root="$(git rev-parse --show-toplevel)"
data="$root/packages/aethyme/rust/.config/prepush-tests.json"
while [ $# -gt 0 ]; do
    case "$1" in
        --base) base="${2:?--base needs a ref}"; shift 2 ;;
        --data) data="${2:?--data needs a file}"; shift 2 ;;
        -h|--help)
            awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
            exit 0 ;;
        *) echo "error: unknown option $1" >&2; exit 2 ;;
    esac
done
command -v jq >/dev/null 2>&1 || { echo "error: jq is required" >&2; exit 2; }
[ -f "$data" ] || { echo "error: no selection data at $data" >&2; exit 2; }

changed=""
if [ -n "$gate_base" ] && [ "$base" = "$gate_base" ]; then
    # Submit already chose this exact comparison commit. It is also valid
    # when the primary-checkout refresh advanced the candidate's parent.
    merge_base="$(git -C "$root" rev-parse --verify "$base^{commit}")"
else
    merge_base="$(git -C "$root" merge-base HEAD "$base")" || merge_base=""
fi
if [ -n "$merge_base" ]; then
    changed="$(
        {
            git -C "$root" diff --no-renames --name-only "$merge_base" -- &&
            git -C "$root" ls-files --others --exclude-standard
        } | sort -u
    )"
else
    echo "warning: no merge base between HEAD and $base; running only the always-run tests" >&2
fi

manifest="$root/packages/aethyme/rust/Cargo.toml"
out="$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$manifest" |
jq -r --arg root "$root/" --arg changed "$changed" --slurpfile data "$data" '
    def tests($p):
      [$p.targets[]
       | select(.test and ((."required-features" // []) | length == 0))
       | .kind[0] as $kind
       | .src_path as $src | .name as $name
       | if (["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"] | index($kind)) != null then
           {b: $p.name, kind: "lib", src: $src}
         elif $kind == "bin" then
           {b: ($p.name + "::bin/" + $name), kind: $kind, src: $src}
         elif $kind == "test" then
           {b: ($p.name + "::" + $name), kind: $kind, src: $src, name: $name}
         else empty end];
    def source_module: split("/")[0] | rtrimstr(".rs");
    def touched($p; $path):
      $p.dir as $dir | ($path | ltrimstr($dir)) as $rel
      | $p.tests as $tests
      | [$tests[] | select(.src == $path)] as $exact
      | if ($exact | length) > 0 then
          $exact[] | select(.src != ($dir + "src/lib.rs") and .src != ($dir + "src/main.rs"))
          | {b, m: "*"}
        elif $rel | startswith("src/bin/") then
          ($dir + "src/bin/" + ($rel | ltrimstr("src/bin/") | split("/")[0]) + "/main.rs") as $entry
          | $tests[] | select(.kind == "bin" and .src == $entry) | {b, m: "*"}
        elif $rel | startswith("src/") then
          ($rel | ltrimstr("src/") | source_module) as $m
          | ([$tests[] | select(.kind == "lib")] | first)
            // ([$tests[] | select(.kind == "bin" and .src == ($dir + "src/main.rs"))] | first)
          | select(. != null) | {b, m: $m}
        elif $rel | startswith("tests/") then
          ($rel | ltrimstr("tests/") | source_module) as $m
          | select((["it", "main", "mod", "common", "support"] | index($m)) == null)
          | $tests[] | select(.kind == "test" and .name == "it") | {b, m: $m}
        else empty end;
    def cost($b; $m):
      if $m == "*" then [.modules | to_entries[] | select(.key | startswith($b + " ")) | .value] | add // 0
      else .modules[$b + " " + $m] // 0 end;
    def term($b; $m):
      if $m == "*" then "binary_id(\($b))" else "(binary_id(\($b)) & test(/^\($m)(::|$)/))" end;
    $data[0] as $d
    | .workspace_members as $members
    | [.packages[] | select(.id as $id | $members | index($id))
       | {dir: (.manifest_path | rtrimstr("Cargo.toml")), tests: tests(.)}] as $packages
    | ([$packages[].tests[].b] | unique) as $binaries
    | def exists: . as $b | $binaries | index($b);
    ([$d.always_binaries[] | {b: ., m: "*"}] + [$d.always_modules[] | {b: .binary, m: .module}]) as $historical
    | ($historical | map(select(.b | exists))) as $always
    | (($d.slow_tests // []) | map(select(.binary | exists))) as $slow
    | ([$changed | split("\n")[] | select(endswith(".rs")) | ($root + .) as $path
         | $packages[] | select(.dir as $dir | $path | startswith($dir))
         | touched(.; $path)]
       | unique
       | map(select(. as $t | $always | any(.b == $t.b and (.m == "*" or .m == $t.m)) | not))
       | map(. as $t | . + {c: ($d | cost($t.b; $t.m))})
       | sort_by(.c, .b, .m)) as $candidates
    | ([$always[] | . as $t | $d | cost($t.b; $t.m)] | add // 0) as $base_cost
    | reduce $candidates[] as $c ({used: $base_cost, take: [], skip: []};
        if .used + $c.c <= $d.budget_seconds then .used += $c.c | .take += [$c] else .skip += [$c] end)
    | ([$historical[].b, ($d.slow_tests // [])[].binary] | unique[] | select(exists | not)
       | "stale\t\(.)"),
      (.skip[] | "skip\t\(.b) \(.m) (\(.c * 100 | round / 100) s)"),
      "used\t\(.used | round) s of a \($d.budget_seconds) s budget, \(.take | length) changed module(s)",
      "sel\t" + "(" + ([($always + .take)[] | term(.b; .m)] | if length > 0 then join(" | ") else "all()" end) + ")"
        + (if $slow == [] then ""
           else " & !(" + ([$slow[] | "(binary_id(\(.binary)) & test(=\(.test)))"] | join(" | ")) + ")" end)
')"
while IFS=$'\t' read -r kind text; do
    case "$kind" in
        (stale) echo "pre-push: historical binary no longer built: $text" >&2 ;;
        (skip) echo "pre-push: over budget, left to the push tier: $text" >&2 ;;
        (used) echo "pre-push: selected $text" >&2 ;;
        (sel) printf '%s\n' "$text" ;;
    esac
done <<<"$out"
