# Internal dogfooding metrics export

Last Updated: 2026-10-08

[`export-metrics.sh`](export-metrics.sh) is an opt-in local diagnostic. Each
run reads broker state for one repository and writes a redacted JSON snapshot
to a directory on your machine. The snapshot still contains stable session ids
and timestamps, so keep it local and out of internal reports. The script only
reads broker state; it never changes the repository or the broker.

## Run it

Needs `aethyme` and `jq` (1.6 or later) on `PATH`.

```bash
./export-metrics.sh --repo /path/to/your-repo --label app
# -> ~/aethyme-pilot-metrics/app-20260926T051812Z.json
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--repo <path>` | current directory | Repository to snapshot |
| `--out <dir>` | `$AETHYME_PILOT_OUT`, else `~/aethyme-pilot-metrics` | Where snapshots go |
| `--label <name>` | `repo` | Local label for this repository; letters, digits, `-`, `_` |

Use this for local troubleshooting only. Do not include its raw output
directory in internal reports. If you schedule local diagnostics, set `PATH`
explicitly because cron's default may not include Homebrew:

```cron
0 18 * * 1-5 PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin $HOME/aethyme-pilot/export-metrics.sh --repo $HOME/src/app --label app >>$HOME/aethyme-pilot-metrics/cron.log 2>&1
```

## What it keeps

The script works from an allowlist: a field is copied only if it is named
below. Everything else in the broker output is dropped.

| Section | Kept |
| --- | --- |
| `summary` | Session counts (live, active, idle, stale, dirty), overlap and promoted-conflict counts, integration relation and commits ahead |
| `sessions` | Per session: numeric id, status, origin, cleanup state, created and last-activity timestamps |
| `leases` | Count, count by kind, ids of sessions holding leases |
| `queue` | Count and count by status; per entry: id, session id, status, timestamps |
| `queue_history` | Count of finished queue entries by outcome |
| `promoted_conflicts` | Count and session ids |
| `advice` | The advice kind (for example `session.dirty-worktree`), its severity and session id |
| `advisories` | Count, by producer and by severity |
| `storage` | Broker worktree counts and retained and reclaimable bytes |
| `blockers` | Count, by kind and scope, how many are safe to clear automatically, session ids; or `available: false` when the list could not be read |

It never keeps file or worktree paths, branch names, task text, commit SHAs,
agent names or emails, advisory evidence, blocker causes or clear commands,
or any source. As a second check, the script refuses to write a snapshot in
which any string contains a `/`, and names the offending field (not its
value) so you can tell us.

Timestamps are Unix milliseconds, as the broker records them. Together with
the queue entries they give the time from a session's start to its first
submit. That detail is useful for local analysis but is deliberately excluded
from the internal aggregate report.

## Aggregate export for internal review

Copy `scripts/pilot-report.jq` and `scripts/pilot-compare.jq` from the Aethyme
checkout to a team-controlled local folder. From an internal repository,
capture numeric counters at the start and end of the observation window. The
filters are strict allowlists: they discard gate and command names, repository
identity, paths, task text, timestamps, and all unrecognized fields. Advisory
gate suggestions are not configured gate executions and are not included in
the gate counters. These counters describe Aethyme activity; they do not
provide a pre-install baseline.

```sh
set -eu
PILOT_DIR="$HOME/aethyme-pilot"
mkdir -p "$PILOT_DIR"
cp /path/to/Aethyme/scripts/pilot-report.jq "$PILOT_DIR/"
cp /path/to/Aethyme/scripts/pilot-compare.jq "$PILOT_DIR/"
cd /path/to/internal-repo
capture() {
    raw=$(aethyme broker advanced metrics --json)
    printf '%s\n' "$raw" | jq -e -f "$PILOT_DIR/pilot-report.jq" > "$PILOT_DIR/$1.json"
    unset raw
}
capture pilot-start
# At the end of the two-week follow-up:
capture pilot-followup
jq -e -s -f "$PILOT_DIR/pilot-compare.jq" "$PILOT_DIR/pilot-start.json" "$PILOT_DIR/pilot-followup.json" > "$PILOT_DIR/pilot-delta.json"
```

Review the numeric files with the team before using `pilot-delta.json` in an
internal report. Record the baseline with the [baseline form](baseline-form.md).
If a counter decreases, the comparison refuses: a reset or prune invalidated
that window, so establish a new baseline. These cumulative counters do not
measure task elapsed time, operator effort, prevented incidents, or causation;
collect those with the baseline form and team feedback.

## Numeric result example

Illustrative numeric-only output from `scripts/pilot-report.jq`:

```json
{
  "schema_version": 1,
  "counters": {
    "gate_runs": 2,
    "gate_execution_ms": 300,
    "gate_cache_hits": 1,
    "estimated_gate_time_saved_ms": 100,
    "conflicts_caught_pre_gate": 1,
    "overlap_warnings": 2,
    "command_calls": 3,
    "command_execution_ms": 400,
    "command_output_bytes": 800,
    "command_output_sampled_calls": 2
  }
}
```

## Versions

On v0.8.4 and later the script reads blockers with
`aethyme broker unblock --json`; on v0.8.3 it falls back to the deprecated
blocker-list command. If `aethyme broker status --json` fails,
the script exits non-zero and writes nothing.

## Internal reporting

Do not include the directory produced by `export-metrics.sh` in a report. Use
only reviewed aggregate data in internal findings; do not publish it as
evidence of independent adoption or product-market fit.
