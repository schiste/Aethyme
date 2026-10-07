#!/usr/bin/env bash
# Run the Rust workspace tests the way CI's "Rust workspace tests" job does.
#
# This script IS that job's test step: oss-ci.yml and macos-nightly.yml call
# it with --full, and aethyme-testkit's ci_validation test fails if a
# workflow runs nextest any other way. One command, one profile
# (`--profile ci` in packages/aethyme/rust/.config/nextest.toml), so a local
# run and CI cannot drift apart (#598).
#
# Usage, from anywhere in the checkout:
#
#     packages/aethyme/scripts/test-like-ci.sh            # tests affected by your change
#     packages/aethyme/scripts/test-like-ci.sh --full     # the whole workspace, as CI
#     packages/aethyme/scripts/test-like-ci.sh --base origin/main -- -E 'test(lease)'
#
# --changed (the default) selects the crates touched since the merge base with
# --base (default origin/main), plus every crate that depends on them; a change
# outside the crates (workflows, Cargo.lock, toolchain, nextest config) runs
# the whole workspace. Arguments after `--` go to `cargo nextest run`.
#
# The suite runs in parallel and retries a failing test once, as CI does; a
# test that passes on retry is reported FLAKY. It never runs serially: a
# serial workspace run took two hours on 2026-10-07 against eight minutes in
# CI, so `--test-threads=1`, `-j 1` and `--no-capture` are refused. Re-run one
# failing test by name instead.
set -euo pipefail

mode=changed
base=origin/main
extra=()
while [ $# -gt 0 ]; do
    case "$1" in
        --full) mode=full; shift ;;
        --changed) mode=changed; shift ;;
        --base) base="${2:?--base needs a ref}"; shift 2 ;;
        -h|--help) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --) shift; extra=("$@"); break ;;
        *) echo "error: unknown option $1 (arguments for nextest go after --)" >&2; exit 2 ;;
    esac
done

for arg in "${extra[@]+"${extra[@]}"}"; do
    case "$arg" in
        --test-threads=1|--nocapture|--no-capture|-j1)
            echo "error: '$arg' runs the suite serially; re-run the one failing test by name instead" >&2
            exit 2 ;;
    esac
done
prev=""
for arg in "${extra[@]+"${extra[@]}"}"; do
    if { [ "$prev" = "--test-threads" ] || [ "$prev" = "-j" ]; } && [ "$arg" = 1 ]; then
        echo "error: '$prev 1' runs the suite serially; re-run the one failing test by name instead" >&2
        exit 2
    fi
    prev="$arg"
done

root="$(git rev-parse --show-toplevel)"
rust="$root/packages/aethyme/rust"
command -v cargo-nextest >/dev/null 2>&1 || {
    echo "error: cargo-nextest is not installed; run: cargo install --locked cargo-nextest" >&2
    exit 2
}

# CI's environment. The test helpers use the binaries built below instead of
# building them once per nextest process.
export AETHYME_TESTKIT_PREBUILT_BINS=1
export CARGO_NET_RETRY="${CARGO_NET_RETRY:-10}"

# A CI runner has no agent process above the tests and no git identity. A
# workstation usually has both, which hid real failures (#574, #576): a test
# that commits without setting an author, or output that names the live agent.
# On a workstation, reproduce the runner: no agent ancestor, no inherited
# broker placement, no system or global git config, and git refuses to
# invent an identity. The user's global
# excludes file is kept so files ignored only there (editor and agent state)
# do not show up as untracked in fixture checks.
if [ "${CI:-}" != "true" ]; then
    export AETHYME_AGENT_PID=0
    # Where this host keeps broker state and worktrees is not where a runner
    # does. An inherited AETHYME_WORKTREE_ROOT put every test repository's
    # root in one shared container and made GC tests flaky (#598).
    unset AETHYME_WORKTREE_ROOT AETHYME_HOST_STATE_DIR XDG_STATE_HOME
    unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL EMAIL
    excludes="$(git config --global --path core.excludesFile 2>/dev/null || true)"
    isolated="$(mktemp "${TMPDIR:-/tmp}/aethyme-ci-gitconfig.XXXXXX")"
    trap 'rm -f "$isolated"' EXIT
    if [ -n "$excludes" ]; then
        git config --file "$isolated" core.excludesFile "$excludes"
    fi
    export GIT_CONFIG_GLOBAL="$isolated"
    export GIT_CONFIG_NOSYSTEM=1
    export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=user.useConfigOnly GIT_CONFIG_VALUE_0=true
fi

filter=()
if [ "$mode" = changed ]; then
    merge_base="$(git -C "$root" merge-base HEAD "$base" 2>/dev/null)" || {
        echo "error: no merge base between HEAD and $base; fetch it or pass --full" >&2
        exit 2
    }
    changed="$(
        {
            git -C "$root" diff --name-only "$merge_base"
            git -C "$root" ls-files --others --exclude-standard
        } | sort -u
    )"
    if [ -z "$changed" ]; then
        echo "No changes since $base; nothing to test. Use --full to run the workspace."
        exit 0
    fi
    expr=""
    while IFS= read -r path; do
        case "$path" in
            packages/aethyme/rust/crates/*/*)
                crate="${path#packages/aethyme/rust/crates/}"
                crate="${crate%%/*}"
                term="rdeps($crate)"
                ;;
            packages/aethyme/rust/*|rust-toolchain.toml|.github/workflows/*)
                mode=full
                break
                ;;
            *)
                # Docs, scripts and repository files are read by the testkit
                # and CLI suites (docs hygiene, spelling, deployed files).
                term="package(aethyme-testkit) | package(aethyme-cli)"
                ;;
        esac
        case " | $expr | " in
            *" | $term | "*) ;;
            *) expr="${expr:+$expr | }$term" ;;
        esac
    done <<<"$changed"
    if [ "$mode" = changed ]; then
        filter=(-E "$expr")
        echo "Testing what changed since $base: $expr"
    else
        echo "A change outside the crates touches every suite; running the whole workspace."
    fi
fi

cd "$rust"
started=$(date +%s)
cargo build --locked --workspace --bins
status=0
cargo nextest run --locked --workspace --profile ci "${filter[@]+"${filter[@]}"}" "${extra[@]+"${extra[@]}"}" || status=$?
elapsed=$(( $(date +%s) - started ))

junit="$rust/target/nextest/ci/junit.xml"
if [ -f "$junit" ]; then
    echo
    echo "Slowest tests:"
    grep -o '<testcase [^>]*' "$junit" \
        | sed -n 's/.*name="\([^"]*\)".*classname="\([^"]*\)".*time="\([0-9.]*\)".*/\3s  \2 \1/p' \
        | sort -rn | sed -n '1,10s/^/  /p'
fi
printf '\nWall time: %dm%02ds (%s)\n' $((elapsed / 60)) $((elapsed % 60)) "$mode"
exit "$status"
