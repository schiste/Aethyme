#!/usr/bin/env python3
"""One bounded PR-monitor pass. Scheduling remains an explicit operator action.

The 90-second I/O budget includes a 35-second tick, ten-second listing and
at most 45 seconds of adapter work. Owned child stop/reap can add one second;
these are cooperative I/O budgets, not hard operating-system deadlines.
No live transport is opened for a verified empty outbox. Failure keeps pending
work for the next scheduled invocation, without retrying this pass.
"""
from __future__ import annotations

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import sys

spec = importlib.util.spec_from_file_location(
    "aethyme_delivery_adapter", Path(__file__).with_name("chau7-delivery-adapter.py"))
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)

PASS_SECONDS = 90
TICK_SECONDS = 35
LOG_CHARACTERS = 16 * 1024


class PassLog:
    """Bound both adapter streams together; never accumulate an unbounded log."""
    def __init__(self, stream, limit=LOG_CHARACTERS):
        self.stream = stream
        self.remaining = limit
        self.truncated = False

    def write(self, value):
        accepted = value[:self.remaining]
        self.stream.write(accepted)
        self.remaining -= len(accepted)
        self.truncated |= len(accepted) != len(value)
        return len(value)

    def flush(self):
        self.stream.flush()


def run_pass(repo, broker, worker, limit, budget=None):
    if not 1 <= limit <= 100:
        raise ValueError("monitor limit must be between 1 and 100")
    if not Path(repo).is_dir():
        raise ValueError("monitor repository directory is unavailable")
    budget = budget or adapter.Deadline(PASS_SECONDS)
    print("PR monitor: tick")
    try:
        tick = adapter.run_broker(
            [broker, "broker", "advanced", "watch", "pr", "tick", "--limit", str(limit)],
            repo, budget=budget, call_seconds=TICK_SECONDS)
        if tick.returncode:
            print("tick failed; continuing to delivery")
    except (OSError, RuntimeError, ValueError) as error:
        print(f"tick failed; continuing to delivery: {str(error)[:512]}")

    try:
        listing = adapter.run_broker(
            [broker, "broker", "advanced", "deliveries", "list", "--adapter", "chau7", "--json"],
            repo, budget=budget)
        if listing.returncode:
            raise ValueError("outbox listing command failed")
        pending = json.loads(listing.stdout)
        if not isinstance(pending, list):
            raise ValueError("outbox listing is not an array")
        if not pending:
            print("no delivery pending; adapter not started")
            return
    except (OSError, RuntimeError, ValueError) as error:
        print(f"delivery listing failed; running the adapter if budget remains: {str(error)[:512]}")

    budget.remaining()  # Refuse before opening a transport after expiry.
    delivery_budget = adapter.Deadline(adapter.PASS_SECONDS, budget.clock)
    delivery_budget.expires = min(delivery_budget.expires, budget.expires)
    adapter.main(["--worker", worker, "--repo", repo, "--broker", broker],
                 budget=delivery_budget)


def main():
    log = PassLog(sys.stdout)
    with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
        try:
            run_pass(os.environ["AETHYME_PR_MONITOR_REPO"],
                     os.environ.get("AETHYME_BROKER_BIN") or "aethyme",
                     os.environ.get("AETHYME_PR_MONITOR_WORKER") or "launchd-" + socket.gethostname().split(".")[0],
                     int(os.environ.get("AETHYME_PR_MONITOR_LIMIT") or "20"))
        except (OSError, RuntimeError, ValueError, KeyError) as error:
            print(f"monitor stopped: {str(error)[:512]}")
    if log.truncated:
        print("PR monitor log truncated; pending deliveries remain broker-owned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
