#!/usr/bin/env bash
# Fixture regression for gate-telemetry.py: gate results, covered, sharded and
# refused gates, and a missing log reported as not recorded rather than as zeros.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
telemetry="$script_dir/gate-telemetry.py"
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/gate-telemetry.XXXXXX")
trap 'rm -rf "$work"' EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

cat > "$work/gates.log" <<'LOG'
gate gate-policy started (cost 1, tree e718ed60ee21)
gate gate-policy pass in 120s (tree e718ed60ee21) at load 1m 1.1/4 cpus
gate workflow-contract started (cost 1, tree e718ed60ee21)
gate workflow-contract running... 30s
gate workflow-contract fail in 900s (tree e718ed60ee21) at load 1m 2.1/4 cpus
gate fast-guards: covered by pull-request run 41846527551 on this tree
gate cargo-test: runs in this workflow's shard jobs
gate script-contract could not start: storage is below the gate reserve
LOG

python3 "$telemetry" --gate-log "$work/gates.log" --markdown "$work/summary.md" --json "$work/summary.json"

python3 - "$work/summary.json" <<'PY' || fail 'telemetry JSON does not match the fixture'
import json, sys
s = json.load(open(sys.argv[1]))
assert s["schema"] == "aethyme.gate-telemetry/v2", s["schema"]
gates = {g["gate"]: g for g in s["gates"]}
assert (gates["gate-policy"]["result"], gates["gate-policy"]["seconds"]) == ("pass", 120), gates
assert (gates["workflow-contract"]["result"], gates["workflow-contract"]["seconds"]) == ("fail", 900), gates
assert (gates["fast-guards"]["result"], gates["fast-guards"]["seconds"]) == ("covered", None), gates
assert (gates["cargo-test"]["result"], gates["cargo-test"]["seconds"]) == ("shard jobs", None), gates
assert (gates["script-contract"]["result"], gates["script-contract"]["seconds"]) == ("refused", None), gates
PY
grep -Fq '| workflow-contract | fail | 15m00.0s | 88.2% |' "$work/summary.md" || fail 'gate row is missing'
grep -Fq '| cargo-test | shard jobs | - | - |' "$work/summary.md" || fail 'sharded gate row is missing'

# A log that was never written is not a run of zero gates, and does not fail the job.
python3 "$telemetry" --gate-log "$work/missing.log" --markdown "$work/none.md" --json "$work/none.json"
grep -Fq 'Gate results were not recorded.' "$work/none.md" || fail 'a missing log is not reported'
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["gates"] is None' \
  "$work/none.json" || fail 'a missing log must be null, not zero'

printf 'PASS: gate telemetry summarizes gates and a missing log\n'
