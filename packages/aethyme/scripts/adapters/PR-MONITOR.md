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
