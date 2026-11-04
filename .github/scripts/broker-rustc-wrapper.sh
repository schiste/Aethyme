#!/usr/bin/env bash
# Cargo workspace wrapper: adds BROKER_RUSTC_FLAGS to aethyme-broker's rustc
# only. Set through RUSTC_WORKSPACE_WRAPPER, so third-party crates and their
# restored cache are untouched.
rustc=$1
shift
for arg in "$@"; do
  if [[ $arg == aethyme_broker ]]; then
    # shellcheck disable=SC2086 # the flags are a space-separated list
    RUSTC_BOOTSTRAP=1 exec "$rustc" "$@" ${BROKER_RUSTC_FLAGS:-}
  fi
done
exec "$rustc" "$@"
