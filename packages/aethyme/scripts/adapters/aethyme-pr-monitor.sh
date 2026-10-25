#!/bin/bash
# One bounded scheduled pass; environment and quiet-pass semantics are preserved.
# Python owns each broker child directly and shares the budget with the adapter.
set -uo pipefail
: "${AETHYME_PR_MONITOR_REPO:?set AETHYME_PR_MONITOR_REPO to the repository path}"
exec python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/aethyme-pr-monitor.py"
