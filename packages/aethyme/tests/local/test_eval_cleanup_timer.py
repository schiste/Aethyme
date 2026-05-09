"""Tests for the bug-fix benchmark auto-cleanup timer.

The cleanup timer was bumped from 5 → 15 min on 2026-05-09 after a
GRC eval run lost its clone dir mid-debug. The `cancel_cleanup_timer`
helper was added so the launch-phase orchestration can disarm the
timer once agents are dispatched (the run is committed, clones must
survive until completion).

These tests exercise both behaviors. We construct timers directly via
`schedule_cleanup` so we can use a tiny delay; the production default
is 15 min and would make tests slow.
"""

from __future__ import annotations

import threading
import time
from pathlib import Path

import pytest

from src.eval.bug_fix import (
    _CLEANUP_DELAY_SECONDS,
    cancel_cleanup_timer,
    cleanup_benchmark,
    schedule_cleanup,
)


def test_default_cleanup_delay_is_at_least_15_minutes():
    """Regression: a 5-minute default lost a clone dir during debugging.

    The bump to 15 min is a deliberate choice; if someone lowers it,
    failing this test should make them think twice.
    """
    assert _CLEANUP_DELAY_SECONDS >= 15 * 60, (
        "cleanup delay should be ≥15 min so eval runs that take >5 min "
        "(common on MediaWiki) don't lose their clones mid-flight"
    )


def test_schedule_cleanup_returns_running_timer(tmp_path: Path):
    """schedule_cleanup arms a daemon Timer that callers can cancel."""
    target = tmp_path / "to-clean"
    target.mkdir()
    timer = schedule_cleanup(target, delay=60.0)
    try:
        assert isinstance(timer, threading.Timer)
        assert timer.is_alive(), "timer should be running"
        assert timer.daemon, "timer must not block process exit"
    finally:
        timer.cancel()


def test_cancel_cleanup_timer_returns_true_for_armed_timer(tmp_path: Path):
    """The launch-phase orchestration calls this once agents are dispatched."""
    target = tmp_path / "kept"
    target.mkdir()
    timer = schedule_cleanup(target, delay=60.0)
    result_dict = {"cleanup_timer": timer, "dest_dir": str(target)}

    # First cancel succeeds.
    assert cancel_cleanup_timer(result_dict) is True
    # Idempotent: a second cancel is safe (timer object is already cancelled).
    assert cancel_cleanup_timer(result_dict) is True


def test_cancel_cleanup_timer_returns_false_when_not_armed():
    """No-op if the prepare result didn't include a timer (auto_cleanup=False)."""
    result_dict: dict = {"dest_dir": "/tmp/whatever"}
    assert cancel_cleanup_timer(result_dict) is False


def test_cancel_cleanup_timer_returns_false_for_non_timer_value():
    """Defensive: if 'cleanup_timer' is somehow a non-Timer (e.g. a stub
    in tests, or a serialized placeholder), do not crash."""
    result_dict = {"cleanup_timer": "not a timer"}
    assert cancel_cleanup_timer(result_dict) is False


def test_cancel_prevents_cleanup_from_running(tmp_path: Path):
    """End-to-end: a cancelled timer should NOT delete the directory.

    Uses a tiny delay so the test completes in ~1s if cancel fails.
    """
    target = tmp_path / "kept"
    target.mkdir()
    (target / "marker.txt").write_text("survives")

    timer = schedule_cleanup(target, delay=0.2)
    cancelled = cancel_cleanup_timer({"cleanup_timer": timer})
    assert cancelled is True
    # Wait past when it would have fired.
    time.sleep(0.5)
    assert target.exists(), "cancelled timer must not delete dir"
    assert (target / "marker.txt").read_text() == "survives"


def test_uncancelled_timer_does_clean(tmp_path: Path):
    """Sanity: without cancel, the timer DOES delete the dir.

    Confirms the cleanup machinery still works — the bump-to-15-min isn't
    accidentally a "never fires" change.
    """
    target = tmp_path / "scheduled-cleanup"
    target.mkdir()
    (target / "marker.txt").write_text("doomed")

    timer = schedule_cleanup(target, delay=0.1)
    try:
        # Wait past fire time.
        time.sleep(0.5)
        assert not target.exists(), "uncancelled timer should have removed dir"
    finally:
        timer.cancel()


def test_cleanup_benchmark_is_idempotent(tmp_path: Path):
    """cleanup_benchmark should not raise when the dir is already gone."""
    target = tmp_path / "missing"  # Never created.
    cleanup_benchmark(target)  # First call: no-op (doesn't exist)
    cleanup_benchmark(target)  # Second call: still no-op
