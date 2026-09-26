# Pilot metrics export

Last Updated: 2026-09-26

[`export-metrics.sh`](export-metrics.sh) is opt-in and local. Each run reads
the broker's state for one repository and writes one small, redacted JSON
snapshot to a directory on your machine. At the end of the pilot you look
through that directory and send it to us. The script only reads broker
state; it never changes the repository or the broker.

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
| `--label <name>` | `repo` | Your name for this repository; letters, digits, `-`, `_` |

Once a day is enough; more often is fine. With cron, set `PATH` explicitly,
because cron's default does not include Homebrew:

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
submit.

## Sample

A snapshot of this repository's own broker, with the lists cut to one entry:

```json
{
  "schema": "aethyme-pilot-metrics.v1",
  "captured_at": "2026-09-26T05:18:12Z",
  "aethyme_version": "0.8.3",
  "label": "aethyme",
  "summary": {"live_sessions": 7, "active_sessions": 6, "idle_sessions": 0, "stale_sessions": 1,
              "dirty_sessions": 3, "overlap_count": 0, "promoted_conflict_count": 1,
              "integration_relation": "current_with_main", "integration_ahead_main_commits": 0},
  "sessions": [{"id": 621, "status": "stale", "derived_status": "stale", "origin": "spawned",
                "cleanup_state": "open", "created_at": 1790150038269, "last_activity_at": 1790150038269}],
  "leases": {"count": 35, "by_kind": {"explicit": 21, "implicit": 14}, "sessions": [621, 669, 670, 671, 672, 673, 674]},
  "queue": {"count": 41, "by_status": {"verified": 41},
            "entries": [{"id": 563, "session_id": 613, "status": "verified",
                         "created_at": 1790090409062, "updated_at": 1790108751604}]},
  "queue_history": {"externally_landed": 446, "rejected": 17, "superseded": 111},
  "promoted_conflicts": {"count": 1, "sessions": [621]},
  "advice": [{"id": "integration.upstream-main-ahead", "severity": "notice", "session_id": null}],
  "advisories": {"count": 7, "by_producer": {"gate_reliability_history": 6, "resource_history": 1},
                 "by_severity": {"warning": 7}},
  "storage": {"broker_owned_worktree_count": 0, "eligible_worktree_count": 0,
              "estimated_retained_bytes": 0, "estimated_reclaimable_bytes": 0, "severity": "notice"},
  "blockers": {"available": true, "count": 0, "by_kind": {}, "by_scope": {},
               "safe_to_clear_automatically": 0, "sessions": [], "unavailable_sources": []}
}
```

## Versions

On v0.8.4 and later the script reads blockers with
`aethyme broker unblock --json`; on v0.8.3 it falls back to
`aethyme broker blockers --json`. If `aethyme broker status --json` fails,
the script exits non-zero and writes nothing.

## Sharing

At the end of the pilot, read the files (they are plain JSON), delete any you
would rather not send, and send us the directory as a zip or tarball.
