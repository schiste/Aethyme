#!/usr/bin/env bash
# Fixture regression for rust-test-telemetry.py: counts, flaky and failed
# classification, shard balance, job phases, and the explicit "unavailable"
# report when the Actions API could not be read.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
telemetry="$script_dir/rust-test-telemetry.py"
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/rust-test-telemetry.XXXXXX")
trap 'rm -rf "$work"' EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

mkdir -p "$work/junit/shard-1" "$work/junit/shard-2"
cat > "$work/junit/shard-1/junit.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="0" errors="0" time="120.0">
  <testsuite name="aethyme-broker" tests="3">
    <testcase name="fast" classname="aethyme-broker" time="0.5"/>
    <testcase name="slow" classname="aethyme-broker" time="90.0"/>
    <testcase name="wobbly" classname="aethyme-broker" time="1.5">
      <flakyFailure message="first attempt failed"/>
    </testcase>
  </testsuite>
</testsuites>
XML
cat > "$work/junit/shard-2/junit.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" failures="1" errors="0" time="60.0">
  <testsuite name="aethyme-cli::it" tests="2">
    <testcase name="ok" classname="aethyme-cli::it" time="10.0"/>
    <testcase name="broken" classname="aethyme-cli::it" time="2.0">
      <failure message="assertion failed"/>
    </testcase>
  </testsuite>
</testsuites>
XML
printf '{"archive_bytes": 52428800, "test_binaries": 12}\n' > "$work/build.json"
cat > "$work/jobs.json" <<'JSON'
{"jobs": [
  {"name": "Classify PR changes", "created_at": "2026-11-04T07:00:00Z", "started_at": "2026-11-04T07:00:05Z", "completed_at": "2026-11-04T07:00:20Z", "steps": []},
  {"name": "Rust workspace tests (build)", "conclusion": "success",
   "created_at": "2026-11-04T07:00:20Z", "started_at": "2026-11-04T07:00:30Z", "completed_at": "2026-11-04T07:03:30Z",
   "steps": [
     {"name": "Build the test archive", "conclusion": "success", "started_at": "2026-11-04T07:00:40Z", "completed_at": "2026-11-04T07:03:10Z"},
     {"name": "Skipped step", "conclusion": "skipped", "started_at": "2026-11-04T07:03:10Z", "completed_at": "2026-11-04T07:03:10Z"}
   ]},
  {"name": "Rust workspace tests (shard 1/2)", "conclusion": "success",
   "created_at": "2026-11-04T07:03:30Z", "started_at": "2026-11-04T07:03:40Z", "completed_at": "2026-11-04T07:06:00Z", "steps": []}
]}
JSON

python3 "$telemetry" --junit-dir "$work/junit" --build-json "$work/build.json" \
  --jobs-json "$work/jobs.json" --markdown "$work/summary.md" --json "$work/summary.json"

python3 - "$work/summary.json" <<'PY' || fail 'telemetry JSON does not match the fixture'
import json, sys
s = json.load(open(sys.argv[1]))
assert s["schema"] == "aethyme.rust-test-telemetry/v1", s["schema"]
t = s["tests"]
assert (t["total"], t["passed"], t["flaky"], t["failed"]) == (5, 3, 1, 1), t
assert t["test_seconds"] == 104.0, t
assert [x["shard"] for x in s["shards"]] == ["shard-1", "shard-2"], s["shards"]
assert s["shard_balance"] == 2.0, s["shard_balance"]
assert s["slowest"][0]["name"] == "slow" and s["slowest"][0]["shard"] == "shard-1", s["slowest"][0]
assert [x["name"] for x in s["flaky"]] == ["wobbly"], s["flaky"]
assert [x["name"] for x in s["failed"]] == ["broken"], s["failed"]
jobs = {j["job"]: j for j in s["jobs"]}
assert set(jobs) == {"Rust workspace tests (build)", "Rust workspace tests (shard 1/2)"}, jobs
build = jobs["Rust workspace tests (build)"]
assert build["queued_seconds"] == 10.0 and build["seconds"] == 180.0, build
assert build["steps"] == [{"name": "Build the test archive", "seconds": 150.0}], build["steps"]
assert s["jobs_first_start_to_last_end_seconds"] == 330.0, s["jobs_first_start_to_last_end_seconds"]
PY
grep -Fq '| Rust workspace tests (build) | 10.0s | 3m00.0s | Build the test archive 2m30.0s |' "$work/summary.md" \
  || fail 'job table row is missing from the Markdown summary'
grep -Fq '`aethyme-broker wobbly` (shard-1)' "$work/summary.md" || fail 'flaky test is not listed'
grep -Fq '`aethyme-cli::it broken` (shard-2)' "$work/summary.md" || fail 'failed test is not listed'

# Without job timings the summary must say so instead of reporting zeros.
python3 "$telemetry" --junit-dir "$work/junit" --build-json "$work/build.json" \
  --jobs-json "$work/missing.json" --markdown "$work/no-jobs.md" --json "$work/no-jobs.json"
grep -Fq 'Job timings unavailable' "$work/no-jobs.md" || fail 'missing job timings are not reported'
python3 -c 'import json,sys; s=json.load(open(sys.argv[1])); assert s["jobs"] is None and s["jobs_first_start_to_last_end_seconds"] is None' \
  "$work/no-jobs.json" || fail 'missing job timings must be null, not zero'

# No shard reports is an error, not an empty summary.
mkdir -p "$work/empty"
if python3 "$telemetry" --junit-dir "$work/empty" --build-json "$work/build.json" \
  --markdown "$work/e.md" --json "$work/e.json" 2>/dev/null; then
  fail 'an empty report directory must fail'
fi

printf 'PASS: Rust test telemetry summarizes shards, jobs and missing inputs\n'
