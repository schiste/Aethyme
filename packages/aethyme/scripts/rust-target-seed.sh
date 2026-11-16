#!/usr/bin/env bash
# Seed a fresh worktree's Cargo target directory from an earlier build.
#
#     packages/aethyme/scripts/rust-target-seed.sh restore
#         # no target directory yet: clone the seed for this lockfile into place
#     packages/aethyme/scripts/rust-target-seed.sh publish
#         # no seed yet for this lockfile: clone the target directory into one
#
# Why: every broker session gets its own worktree, and a first build there
# compiles every third-party crate again. Measured on 2026-11-16: 70m41s for a
# first `--package aethyme-broker --lib` build on a host under memory pressure,
# against 1m54s in a second directory whose target was cloned from the first.
# Cargo keys third-party artifacts on the registry source, not the workspace
# path, so that run recompiled the 9 workspace crates and nothing else. The
# clone itself took 1.3s for 1.2 GB and uses no disk until files diverge.
#
# The seed is keyed on Cargo.lock, the compiler and the architecture, so a
# worktree never receives artifacts built for other dependencies. It lives
# beside the repository's linked worktrees so it shares their volume and a
# copy-on-write clone is possible; a main checkout has no such sibling and is
# left alone. Each worktree keeps its own target directory: the seed is a
# snapshot to start from, never a shared build directory.
#
# AETHYME_RUST_TARGET_SEED_DIR moves the seed store; `off` disables both
# verbs. Both verbs are best-effort: any failure prints a note and exits 0,
# because a missing seed only costs the build it would have saved.
set -uo pipefail

verb="${1:-}"
case "$verb" in
    restore | publish) ;;
    *)
        echo "usage: $0 restore|publish" >&2
        exit 2
        ;;
esac

note() { echo "rust target seed: $*" >&2; }

rust="$(cd "$(dirname "${BASH_SOURCE[0]}")/../rust" && pwd -P)" || exit 0
store="${AETHYME_RUST_TARGET_SEED_DIR:-}"
[ "$store" = off ] && exit 0
if [ -z "$store" ]; then
    top="$(git -C "$rust" rev-parse --show-toplevel 2>/dev/null)" || exit 0
    git_dir="$(cd "$top" && cd "$(git rev-parse --git-dir)" && pwd -P)" || exit 0
    common_dir="$(cd "$top" && cd "$(git rev-parse --git-common-dir)" && pwd -P)" || exit 0
    # Only a linked worktree has siblings to share with.
    [ "$git_dir" = "$common_dir" ] && exit 0
    store="$(dirname "$top")/.rust-target-seed"
fi

hash_stdin() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256; else sha256sum; fi
}
key="$( { cat "$rust/Cargo.lock" && rustc -Vv && uname -sm; } 2>/dev/null | hash_stdin | cut -c1-16)"
[ "${#key}" -eq 16 ] || { note "skipped, no key (Cargo.lock or rustc unavailable)"; exit 0; }
seed="$store/$key"

clone_dir() {
    # $1 source directory, $2 destination that must not exist.
    if [ "$(uname -s)" = Darwin ]; then
        cp -cR "$1" "$2" 2>/dev/null || { rm -rf "$2"; cp -R "$1" "$2"; }
    else
        cp -R --reflink=auto "$1" "$2"
    fi
}

started=$(date +%s)
case "$verb" in
    restore)
        [ -e "$rust/target" ] && exit 0
        [ -f "$seed/complete" ] || { note "none for $key yet"; exit 0; }
        tmp="$rust/target.seed-$$"
        if clone_dir "$seed/target" "$tmp" && mv "$tmp" "$rust/target"; then
            note "restored $key in $(( $(date +%s) - started ))s"
        else
            rm -rf "$tmp"
            note "restore of $key failed; building from scratch"
        fi
        ;;
    publish)
        [ -d "$rust/target/debug/deps" ] || exit 0
        [ -e "$seed" ] && exit 0
        tmp="$store/.incoming-$key-$$"
        mkdir -p "$store" || { note "cannot create $store"; exit 0; }
        if mkdir "$tmp" && clone_dir "$rust/target" "$tmp/target"; then
            # Incremental state and test reports belong to this worktree's
            # own crates and path; they would only take space in the seed.
            rm -rf "$tmp/target/debug/incremental" "$tmp/target/nextest" "$tmp/target/cargo-timings"
            : >"$tmp/complete"
            # Another session may have published the same key meanwhile.
            if mv "$tmp" "$seed" 2>/dev/null; then
                note "published $key in $(( $(date +%s) - started ))s"
                # Keep the two newest seeds; older lockfiles are not coming back.
                # Seed names are 16 hex digits; mtime order needs ls.
                # shellcheck disable=SC2010
                ls -1t "$store" 2>/dev/null | grep -E '^[0-9a-f]{16}$' | tail -n +3 | while read -r old; do
                    [ -f "$store/$old/complete" ] && rm -rf "${store:?}/$old" && note "removed old seed $old"
                done
            else
                rm -rf "$tmp"
            fi
        else
            rm -rf "$tmp"
            note "publish of $key failed"
        fi
        ;;
esac
exit 0
