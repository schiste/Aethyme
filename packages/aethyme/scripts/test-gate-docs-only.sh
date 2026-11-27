#!/usr/bin/env bash
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
classifier="$root/packages/aethyme/scripts/gate-docs-only.sh"
: "${AETHYME_SESSION_SCRATCH_DIR:?session scratch is required}"
fixture="$(mktemp -d "$AETHYME_SESSION_SCRATCH_DIR/docs-gate-test.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT
cd "$fixture"
git init -q
git config user.name 'Gate Test'
git config user.email gate-test@example.invalid
mkdir -p packages/aethyme/docs packages/aethyme/rust
printf 'before\n' > packages/aethyme/docs/guide.md
printf 'before\n' > packages/aethyme/rust/main.rs
git add packages/aethyme/docs/guide.md packages/aethyme/rust/main.rs
git commit -qm base
base="$(git rev-parse HEAD)"
printf 'after\n' > packages/aethyme/docs/guide.md
git add packages/aethyme/docs/guide.md
git commit -qm docs
AETHYME_GATE_BASE_COMMIT="$base" "$classifier"
printf 'after\n' > packages/aethyme/rust/main.rs
git add packages/aethyme/rust/main.rs
git commit -qm mixed
if AETHYME_GATE_BASE_COMMIT="$base" "$classifier"; then
    echo 'mixed changes must not use the documentation path' >&2
    exit 1
fi
if AETHYME_GATE_BASE_COMMIT= "$classifier"; then
    echo 'missing base must not use the documentation path' >&2
    exit 1
fi
printf 'untracked\n' > untracked
if AETHYME_GATE_BASE_COMMIT="$base" "$classifier"; then
    echo 'dirty checkout must not use the documentation path' >&2
    exit 1
fi
