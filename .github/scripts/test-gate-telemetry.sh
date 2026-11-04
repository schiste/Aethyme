#!/usr/bin/env bash
# Fixture regression for gate-telemetry.py: gate results, covered and refused
# gates, cargo-test phases and suite detail, and missing inputs reported as not
# recorded rather than as zeros.
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
gate cargo-test started (cost 3, tree e718ed60ee21)
gate cargo-test running... 30s
gate cargo-test fail in 900s (tree e718ed60ee21) at load 1m 2.1/4 cpus
gate fast-guards: covered by pull-request run 41846527551 on this tree
gate script-contract could not start: storage is below the gate reserve
LOG
printf 'product-binaries\t60\t0\ntest-build\t180\t0\ntest-run\t600\t1\ndoctests\t40\t0\nexamples\t20\t0\n' \
  > "$work/phases.tsv"
cat > "$work/junit.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1" errors="0" time="600.0">
  <testsuite name="aethyme-broker" tests="2">
    <testcase name="fast" classname="aethyme-broker" time="0.5"/>
    <testcase name="slow" classname="aethyme-broker" time="90.0"/>
  </testsuite>
  <testsuite name="aethyme-cli::it" tests="1">
    <testcase name="broken" classname="aethyme-cli::it" time="2.0">
      <failure message="assertion failed"/>
    </testcase>
  </testsuite>
</testsuites>
XML

python3 "$telemetry" --gate-log "$work/gates.log" --phases "$work/phases.tsv" \
  --junit "$work/junit.xml" --markdown "$work/summary.md" --json "$work/summary.json"

python3 - "$work/summary.json" <<'PY' || fail 'telemetry JSON does not match the fixture'
import json, sys
s = json.load(open(sys.argv[1]))
assert s["schema"] == "aethyme.gate-telemetry/v1", s["schema"]
gates = {g["gate"]: g for g in s["gates"]}
assert (gates["gate-policy"]["result"], gates["gate-policy"]["seconds"]) == ("pass", 120), gates
assert (gates["cargo-test"]["result"], gates["cargo-test"]["seconds"]) == ("fail", 900), gates
assert (gates["fast-guards"]["result"], gates["fast-guards"]["seconds"]) == ("covered", None), gates
assert (gates["script-contract"]["result"], gates["script-contract"]["seconds"]) == ("refused", None), gates
assert [p["phase"] for p in s["phases"]] == ["product-binaries", "test-build", "test-run", "doctests", "examples"]
assert s["phases"][2] == {"phase": "test-run", "seconds": 600, "status": 1}, s["phases"]
suite = s["suite"]
assert (suite["tests"], suite["passed"], suite["wall_seconds"]) == (3, 2, 600.0), suite
assert suite["test_seconds"] == 92.5 and suite["average_parallelism"] == 0.15, suite
assert [t["name"] for t in suite["failed"]] == ["broken"], suite["failed"]
assert suite["slowest"][0]["name"] == "slow", suite["slowest"]
assert suite["binaries"][0] == {"binary": "aethyme-broker", "tests": 2, "test_seconds": 90.5}, suite["binaries"]
PY
grep -Fq '| cargo-test | fail | 15m00.0s | 88.2% |' "$work/summary.md" || fail 'gate row is missing'
grep -Fq '| test-run | 10m00.0s | 1 |' "$work/summary.md" || fail 'phase row is missing'
grep -Fq '`aethyme-cli::it broken`' "$work/summary.md" || fail 'failed test is not listed'

# A tree a pull request already tested runs no cargo-test gate: not zeros, and
# not a failure of the job.
python3 "$telemetry" --gate-log "$work/gates.log" --phases "$work/missing.tsv" \
  --junit "$work/missing.xml" --markdown "$work/none.md" --json "$work/none.json"
grep -Fq 'Not recorded: the cargo-test gate did not run here.' "$work/none.md" || fail 'missing phases are not reported'
grep -Fq 'No JUnit report was recorded.' "$work/none.md" || fail 'a missing report is not reported'
python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); assert s["phases"] is None and s["suite"] is None' \
  "$work/none.json" || fail 'missing inputs must be null, not zero'

printf 'PASS: gate telemetry summarizes gates, phases, the suite and missing inputs\n'
