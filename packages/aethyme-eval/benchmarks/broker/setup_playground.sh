#!/bin/sh
# Generate the shoplib playground as a standalone Git repository at <dest>.
# Refuses any destination inside a Git work tree (never Aethyme itself).
set -eu
[ $# -eq 1 ] || { echo "usage: $0 <dest>" >&2; exit 2; }
exec python3 "$(cd "$(dirname "$0")" && pwd)/harness.py" playground "$1"
