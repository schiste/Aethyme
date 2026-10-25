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
import stat
import time
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


def run_pass(repo, broker, worker, limit, budget=None, report=None):
    report = report if report is not None else {}
    report.update(tick="unknown", outbox="unknown", delivery="not_started")
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
        report["tick"] = "failed" if tick.returncode else "completed"
        if tick.returncode:
            print("tick failed; continuing to delivery")
    except (OSError, RuntimeError, ValueError) as error:
        report["tick"] = "failed"
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
        report["outbox"] = "pending" if pending else "empty"
        if not pending:
            print("no delivery pending; adapter not started")
            return
    except (OSError, RuntimeError, ValueError) as error:
        print(f"delivery listing failed; running the adapter if budget remains: {str(error)[:512]}")

    budget.remaining()  # Refuse before opening a transport after expiry.
    delivery_budget = adapter.Deadline(adapter.PASS_SECONDS, budget.clock)
    delivery_budget.expires = min(delivery_budget.expires, budget.expires)
    report["delivery"] = "attempted"  # A zero exit also means unavailable; never claim sent.
    adapter.main(["--worker", worker, "--repo", repo, "--broker", broker],
                 budget=delivery_budget)


HEALTH_BYTES = 4096
HEALTH_FIELDS = {"schema", "finished_unix_ms", "outcome", "tick", "outbox", "delivery", "failure_kind"}


def write_health(report):
    """Replace one small private sibling record; never append or follow links.

    The opt-in is intended for an installed private helper directory. A trusted
    directory descriptor anchors checks and publication across path renames.
    This reports pass progress, not provider freshness or delivery completion.
    """
    import fcntl  # Health is Unix-only; keep the default-off import path unchanged.

    record = {key: value for key, value in report.items() if key in HEALTH_FIELDS}
    payload = (json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n").encode()
    if len(payload) > HEALTH_BYTES:
        raise ValueError("monitor health exceeds the record budget")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    directory = os.open(Path(__file__).absolute().parent, flags)
    def private_file(metadata, maximum):
        return (stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.geteuid() and
                not metadata.st_mode & 0o077 and metadata.st_nlink == 1 and
                metadata.st_size <= maximum)

    try:
        metadata = os.fstat(directory)
        if metadata.st_uid != os.geteuid() or metadata.st_mode & 0o077:
            raise ValueError("monitor health requires a private user-owned helper directory")
        lock_fd = os.open(".health.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW |
                          os.O_CLOEXEC, 0o600, dir_fd=directory)
        with os.fdopen(lock_fd, "r+b") as lock:
            if not private_file(os.fstat(lock.fileno()), 0):
                raise ValueError("monitor health lock is not a private empty regular file")
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                old = os.stat("health.json", dir_fd=directory, follow_symlinks=False)
            except FileNotFoundError:
                old = None
            if old is not None and not private_file(old, HEALTH_BYTES):
                raise ValueError("existing monitor health is not a bounded private regular file")
            # This fixed scratch name belongs to the health protocol. The writer
            # lock serializes reuse after a crash; it is never unlinked or rotated.
            pending_fd = os.open(".health.pending", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW |
                                 os.O_CLOEXEC, 0o600, dir_fd=directory)
            with os.fdopen(pending_fd, "r+b") as output:
                if not private_file(os.fstat(output.fileno()), HEALTH_BYTES):
                    raise ValueError("monitor health scratch is not a bounded private regular file")
                os.ftruncate(output.fileno(), 0)
                output.write(payload)
                output.flush()
                os.fsync(output.fileno())
            os.replace(".health.pending", "health.json", src_dir_fd=directory, dst_dir_fd=directory)
            os.fsync(directory)
    finally:
        os.close(directory)


def main():
    log = PassLog(sys.stdout)
    health = os.environ.get("AETHYME_PR_MONITOR_RECORD_HEALTH") == "1"
    report = {"schema": "aethyme.pr-monitor-health/v1", "outcome": "stopped",
              "tick": "unknown", "outbox": "unknown", "delivery": "not_started",
              "failure_kind": None}
    with contextlib.redirect_stdout(log), contextlib.redirect_stderr(log):
        try:
            arguments = (os.environ["AETHYME_PR_MONITOR_REPO"],
                         os.environ.get("AETHYME_BROKER_BIN") or "aethyme",
                         os.environ.get("AETHYME_PR_MONITOR_WORKER") or "launchd-" + socket.gethostname().split(".")[0],
                         int(os.environ.get("AETHYME_PR_MONITOR_LIMIT") or "20"))
            if health:
                run_pass(*arguments, report=report)
            else:
                run_pass(*arguments)
            report["outcome"] = "completed"
        except (OSError, RuntimeError, ValueError, KeyError) as error:
            report["failure_kind"] = type(error).__name__
            print(f"monitor stopped: {str(error)[:512]}")
        finally:
            if health:
                report["finished_unix_ms"] = time.time_ns() // 1_000_000
                try:
                    write_health(report)
                except (OSError, ValueError) as error:
                    print(f"monitor health unavailable: {type(error).__name__}")
    if log.truncated:
        print("PR monitor log truncated; pending deliveries remain broker-owned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
