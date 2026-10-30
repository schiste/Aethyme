#!/usr/bin/env bash
# Prefer a bounded local run; --full keeps the CI coverage and profile.
#
# Usage, from anywhere in the checkout:
#
#     packages/aethyme/scripts/test-like-ci.sh
#         # changed crates, with bounded local resource defaults
#     packages/aethyme/scripts/test-like-ci.sh --full
#         # full workspace, with the CI profile
#     packages/aethyme/scripts/test-like-ci.sh --package aethyme-broker --lib
#         # one crate's library unit tests, without a workspace-bin prebuild
#     packages/aethyme/scripts/test-like-ci.sh --base origin/main -- -E 'test(lease)'
#
# --changed (the default) selects affected crates and their reverse
# dependencies. Repository-wide inputs that affect every suite fall back to
# full workspace coverage while keeping the bounded local profile.
# Arguments after -- are passed to nextest; package and target selectors must
# appear before -- so the script can choose the right build path.
#
# Local defaults use one Cargo build job, two nextest test workers, no retries,
# fail-fast, and failure output at the end. Explicitly pass --full for the CI
# profile, including CI retries and full-suite behavior. The local job count
# can be overridden with CARGO_BUILD_JOBS; nextest options after -- can
# explicitly override worker/retry settings.
set -euo pipefail

coverage=changed
coverage_explicit=0
base=origin/main
base_explicit=0
package=""
library=0
extra=()
while [ $# -gt 0 ]; do
    case "$1" in
        --full|--changed)
            requested="${1#--}"
            if [ "$coverage_explicit" -eq 1 ] && [ "$coverage" != "$requested" ]; then
                echo "error: --full and --changed cannot be combined" >&2
                exit 2
            fi
            coverage="$requested"
            coverage_explicit=1
            shift ;;
        --base)
            base="${2:?--base needs a ref}"
            base_explicit=1
            shift 2 ;;
        --package)
            if [ -n "$package" ]; then
                echo "error: pass --package only once" >&2
                exit 2
            fi
            package="${2:?--package needs a name}"
            shift 2 ;;
        --lib)
            library=1
            shift ;;
        -h|--help)
            awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
            exit 0 ;;
        --)
            shift
            extra=("$@")
            break ;;
        *)
            echo "error: unknown option $1 (nextest options go after --)" >&2
            exit 2 ;;
    esac
done

if [ -n "$package" ] || [ "$library" -eq 1 ]; then
    if [ -z "$package" ] || [ "$library" -ne 1 ]; then
        echo "error: focused runs require both --package NAME and --lib" >&2
        exit 2
    fi
    if [ "$coverage_explicit" -eq 1 ] || [ "$base_explicit" -eq 1 ]; then
        echo "error: focused --package/--lib cannot be combined with --full, --changed, or --base" >&2
        exit 2
    fi
    coverage=focused
elif [ "$coverage" = full ] && [ "$base_explicit" -eq 1 ]; then
    echo "error: --base only applies to changed-path selection" >&2
    exit 2
fi

# Keep test selection explicit so a focused request cannot accidentally
# trigger the workspace-bin build path. Zero selected tests remain a failure.
for arg in "${extra[@]+"${extra[@]}"}"; do
    case "$arg" in
        -p|-p?*|--package|--package=*|--workspace|--all|--exclude|--exclude=*|--lib|--bin|--bin=*|--bins|--test|--test=*|--tests|--example|--example=*|--examples|--all-targets)
            echo "error: package/target selectors must appear before --" >&2
            exit 2 ;;
        --no-tests|--no-tests=*)
            echo "error: zero selected tests must remain a failure" >&2
            exit 2 ;;
    esac
done


# Rust and tempfile helpers compare paths literally. Normalize the effective
# inherited temp root so symlinked aliases (for example /var vs /private/var)
# agree with filesystem canonicalization on macOS.
effective_tmpdir="${TMPDIR:-/tmp}"
if [ ! -d "$effective_tmpdir" ]; then
    echo "error: TMPDIR must name an existing directory" >&2
    exit 2
fi
if ! canonical_tmpdir="$(CDPATH= cd -P -- "$effective_tmpdir" 2>/dev/null && pwd -P)"; then
    echo "error: TMPDIR could not be resolved to a canonical directory" >&2
    exit 2
fi
export TMPDIR="$canonical_tmpdir"

root="$(git rev-parse --show-toplevel)"
rust="$root/packages/aethyme/rust"
command -v cargo-nextest >/dev/null 2>&1 || {
    echo "error: cargo-nextest is not installed; run: cargo install --locked cargo-nextest" >&2
    exit 2
}

export CARGO_NET_RETRY="${CARGO_NET_RETRY:-10}"

# A CI runner has no agent process above the tests and no git identity. A
# workstation usually has both, which hid real failures (#574, #576): a test
# that commits without setting an author, or output that names the live agent.
# On a workstation, reproduce the runner: no agent ancestor, no inherited
# broker placement, no system or global git config, and git refuses to
# invent an identity. The user's global excludes file is kept so files
# ignored only there (editor and agent state) do not show up as untracked in
# fixture checks.
if [ "${CI:-}" != "true" ]; then
    export AETHYME_AGENT_PID=0
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
if [ "$coverage" = changed ]; then
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
            packages/aethyme/rust/*|Cargo.toml|Cargo.lock|.cargo/*|rust-toolchain.toml|.github/workflows/*)
                coverage=workspace
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
    if [ "$coverage" = changed ]; then
        filter=(-E "$expr")
        echo "Testing what changed since $base: $expr"
    else
        echo "A repository-wide input changed; running workspace coverage with the bounded local profile."
    fi
fi

cd "$rust"
started=$(date +%s)
selection=()
test_command=()
if [ "$coverage" = full ]; then
    export AETHYME_TESTKIT_PREBUILT_BINS=1
    cargo build --locked --workspace --bins
    selection=(--workspace)
    test_command=(cargo nextest run --locked "${selection[@]}" --profile ci --no-tests fail)
else
    local_build_jobs="${CARGO_BUILD_JOBS:-1}"
    if ! [[ "$local_build_jobs" =~ ^[1-9][0-9]*$ ]]; then
        echo "error: CARGO_BUILD_JOBS must be a positive integer" >&2
        exit 2
    fi
    export CARGO_BUILD_JOBS="$local_build_jobs"
    if [ "$coverage" = focused ]; then
        unset AETHYME_TESTKIT_PREBUILT_BINS
        selection=(-p "$package" --lib)
    else
        # The affected-crate and workspace-fallback suites may invoke the CLI.
        # Warm these binaries once, with the same bounded Cargo job count.
        export AETHYME_TESTKIT_PREBUILT_BINS=1
        cargo build --locked --workspace --bins --jobs "$local_build_jobs"
        selection=(--workspace)
    fi
    test_command=(cargo nextest run --locked "${selection[@]}" --profile local --build-jobs "$local_build_jobs" --no-tests fail)
fi

status=0
test_command+=("${filter[@]+"${filter[@]}"}" "${extra[@]+"${extra[@]}"}")
"${test_command[@]}" || status=$?
elapsed=$(( $(date +%s) - started ))

if [ "$coverage" = full ] && [ "$status" -eq 0 ]; then
    junit="$rust/target/nextest/ci/junit.xml"
    if [ -f "$junit" ]; then
        echo
        echo "Slowest tests:"
        grep -o '<testcase [^>]*' "$junit" \
            | sed -n 's/.*name="\([^"]*\)".*classname="\([^"]*\)".*time="\([0-9.]*\)".*/\3s  \2 \1/p' \
            | sort -rn | sed -n '1,10s/^/  /p'
    fi
fi
printf '\nWall time: %dm%02ds (%s coverage)\n' $((elapsed / 60)) $((elapsed % 60)) "$coverage"
exit "$status"
