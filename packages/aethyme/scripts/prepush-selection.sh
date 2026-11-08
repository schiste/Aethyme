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
#
# Measured on 187 test-failing pull-request runs from 2026-10-20 to 2026-11-08
# (the always-run modules were chosen from earlier runs only): this selection
# caught 89% of them at a mean of 10% of suite time. A fixed list alone caught
# 44%; the changed modules alone cost 26% on average and up to the whole suite.
#
# A touched path maps to a test module as the test names do:
#   crates/C/src/M.rs, crates/C/src/M/...   -> library tests of C under M::
#   crates/C/src/bin/B.rs                   -> the whole binary C::bin/B
#   crates/C/tests/F.rs                     -> the whole test binary C::F
#   crates/C/tests/it/M.rs, tests/it/M/...  -> tests of C::it under M::
# Crate roots, shared test support, manifests and non-Rust files select no
# module of their own; the always-run suites read the repository files, and
# the push and release tiers run everything this narrows away.
#
# Modules left out by the budget are named on stderr. The filterset goes to
# stdout and is never empty.
set -euo pipefail

base=origin/main
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
if merge_base="$(git -C "$root" merge-base HEAD "$base" 2>/dev/null)"; then
    changed="$(
        {
            git -C "$root" diff --name-only "$merge_base"
            git -C "$root" ls-files --others --exclude-standard
        } | sort -u
    )"
else
    echo "warning: no merge base between HEAD and $base; running only the always-run tests" >&2
fi

# One "binary<TAB>module" line per touched module; module "*" is a whole
# binary. A suite file under tests/ is a module of the crate's `it` binary
# unless it is a test target of its own; the jq program below decides which.
map_path() {
    local rel crate rest m
    case "$1" in
        (packages/aethyme/rust/crates/*/*.rs) ;;
        (*) return 0 ;;
    esac
    rel="${1#packages/aethyme/rust/crates/}"
    crate="${rel%%/*}"
    rest="${rel#*/}"
    case "$rest" in
        (src/bin/*) m="${rest#src/bin/}"; m="${m%%/*}"; printf '%s::bin/%s\t*\n' "$crate" "${m%.rs}" ;;
        (src/lib.rs|src/main.rs) ;;
        (src/*) m="${rest#src/}"; m="${m%%/*}"; printf '%s\t%s\n' "$crate" "${m%.rs}" ;;
        (tests/*)
            m="${rest#tests/}"; m="${m%%/*}"; m="${m%.rs}"
            case "$m" in
                (it|main|mod|common|support) ;;
                (*) printf '%s::?\t%s\n' "$crate" "$m" ;;
            esac ;;
    esac
}
touched=""
while IFS= read -r path; do
    [ -n "$path" ] && touched+="$(map_path "$path")"$'\n'
done <<<"$changed"

out="$(jq -r --arg touched "$touched" '
    def cost($b; $m):
      if $m == "*" then [.modules | to_entries[] | select(.key | startswith($b + " ")) | .value] | add // 0
      else .modules[$b + " " + $m] // 0 end;
    def term($b; $m):
      if $m == "*" then "binary_id(\($b))" else "(binary_id(\($b)) & test(/^\($m)(::|$)/))" end;
    . as $d
    | ([$d.always_binaries[] | {b: ., m: "*"}] + [$d.always_modules[] | {b: .binary, m: .module}]) as $always
    | ([$touched | split("\n")[] | select(length > 0) | split("\t") | {b: .[0], m: .[1]}
         | if (.b | endswith("::?")) then
             (.b | rtrimstr("::?")) as $c | .m as $f
             | if ($d.modules | keys | any(startswith($c + "::" + $f + " ")))
               then {b: ($c + "::" + $f), m: "*"} else {b: ($c + "::it"), m: $f} end
           else . end]
       | unique
       | map(select(. as $t | $always | any(.b == $t.b and (.m == "*" or .m == $t.m)) | not))
       | map(. as $t | . + {c: ($d | cost($t.b; $t.m))})
       | sort_by(.c, .b, .m)) as $candidates
    | ([$always[] | . as $t | $d | cost($t.b; $t.m)] | add // 0) as $base_cost
    | reduce $candidates[] as $c ({used: $base_cost, take: [], skip: []};
        if .used + $c.c <= $d.budget_seconds then .used += $c.c | .take += [$c] else .skip += [$c] end)
    | (.skip[] | "skip\t\(.b) \(.m) (\(.c * 100 | round / 100) s)"),
      "used\t\(.used | round) s of a \($d.budget_seconds) s budget, \(.take | length) changed module(s)",
      "sel\t" + ([($always + .take)[] | term(.b; .m)] | join(" | "))
' "$data")"
while IFS=$'\t' read -r kind text; do
    case "$kind" in
        (skip) echo "pre-push: over budget, left to the push tier: $text" >&2 ;;
        (used) echo "pre-push: selected $text" >&2 ;;
        (sel) printf '%s\n' "$text" ;;
    esac
done <<<"$out"
