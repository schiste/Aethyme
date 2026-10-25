# Scheduled PR monitor

The shell entrypoint keeps the existing environment contract and delegates to
`aethyme-pr-monitor.py`, which uses the sibling delivery adapter's bounded
subprocess runner. Python 3 and all three files must be present together.
The LaunchAgent template remains an explicit operator-installed scheduler;
this change does not install a service or send a live notification.

One pass has a shared 90-second I/O budget: up to 35 seconds for the broker
watch tick, ten seconds for listing deliveries, and at most 45 seconds for the
adapter, reduced to the remaining pass time. Native tick provider work already
has its own smaller budget. Stopping/reaping an owned direct child can add one
second per failed subprocess; filesystem and operating-system operations are
not guaranteed to meet a hard wall-clock deadline. This monitor does not prove
cleanup of escaped descendants.

A failed tick still allows previously pending delivery work to be considered.
Only a successfully parsed empty JSON array skips the adapter; a refused,
malformed or failed listing falls back to the adapter if time remains. Expiry
prevents a new transport connection. Each pass keeps the adapter's existing
ten-delivery maximum. The monitor limit defaults to 20 and accepts 1 through
100. Broker command output remains bounded at four MiB per child and is not
copied into the scheduled log. Combined adapter/monitor output is capped at
16,384 characters per pass with a visible truncation marker. Existing log files
are not rotated by this script.

The default worker remains `launchd-<short-hostname>`. The other supported
variables are `AETHYME_PR_MONITOR_REPO` (required), `AETHYME_BROKER_BIN`,
`AETHYME_PR_MONITOR_WORKER` and `AETHYME_PR_MONITOR_LIMIT`. Quiet passes and
caught transport/provider failures exit successfully for the next scheduled
pass. The broker retains delivery state; an interrupted send can remain
ambiguous, and this does not provide exactly-once notification delivery.

Run the offline controls with `python3 test_pr_monitor.py` and
`python3 test_chau7_delivery_adapter.py`. They use synthetic subprocesses and
mock transports, and require no live Chau7, GitHub, broker, model or credentials.
The Rust testkit wrapper runs both suites in CI.

## Optional bounded last-pass health

`AETHYME_PR_MONITOR_RECORD_HEALTH=1` enables one `health.json` beside the
monitor script. It is off by default and intended for a separately installed
private helper directory, not the shared source checkout. The directory must
be user-owned with no group/other permissions. Existing health files must be
private, user-owned regular files with one link and at most 4 KiB; symlinks,
hard links, directories and oversized files are refused. Publication uses an
exclusive temporary file, fsync and atomic replacement through a directory
descriptor. Repeated passes replace one mode-0600 file rather than appending.
Temporary files are removed after publication failure; a process crash can
leave a small temporary file, which is not automatically removed without an
ownership proof. This option does not rotate existing manual scheduler logs.

The JSON contract is `aethyme.pr-monitor-health/v1` with these fields:

| Field | Meaning |
| --- | --- |
| `schema` | Versioned record identity above |
| `finished_unix_ms` | Wall-clock time when the pass exited; consumers must check staleness and clock skew |
| `outcome` | `completed` means the pass returned; `stopped` means it did not. Neither proves successful provider polling |
| `tick` | `completed`, `failed`, or `unknown` for the tick command |
| `outbox` | Verified `empty`, `pending`, or `unknown` |
| `delivery` | `not_started` or `attempted`; never a claim of send or acknowledgement |
| `failure_kind` | Exception class only, or null; no exception messages, provider output or transcripts |

A zero adapter exit can mean Chau7 was unavailable, so health never promotes it
to delivered. A failed tick remains visible even if the outbox is empty. Health
write failure is reported without replacing the monitor result. Records do not
authorize retries, process cleanup, lease release, provider freshness, or
exactly-once delivery. Service generation binding, freshness interpretation and
scheduler lifecycle status remain future installer responsibilities for #887.
