#!/bin/sh
# Create a fresh arm-b root and write <arm-root>/launch_plan.json.
# usage: $0 <arm-root> [--rep N]
set -eu
[ $# -ge 1 ] || { echo "usage: $0 <arm-root> [--rep N]" >&2; exit 2; }
exec python3 "$(cd "$(dirname "$0")" && pwd)/harness.py" setup-arm b "$@"
