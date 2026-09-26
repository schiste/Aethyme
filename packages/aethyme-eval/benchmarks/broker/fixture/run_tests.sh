#!/bin/sh
# The whole test suite. Standard library only; finishes in well under a second.
cd "$(dirname "$0")" && exec python3 -m unittest discover -s tests -t . -q "$@"
