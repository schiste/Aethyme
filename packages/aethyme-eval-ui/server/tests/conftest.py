"""Pytest fixtures — isolate DB per-test so we don't corrupt the real evals.db."""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

import pytest


@pytest.fixture
def isolated_db(tmp_path, monkeypatch):
    """Point the db module at a fresh sqlite file in tmp_path and return the module."""
    # Ensure the server/ directory is importable as a top-level package regardless
    # of where pytest is invoked from.
    server_dir = Path(__file__).resolve().parent.parent
    if str(server_dir) not in sys.path:
        sys.path.insert(0, str(server_dir))

    if "db" in sys.modules:
        del sys.modules["db"]
    db = importlib.import_module("db")

    db_path = tmp_path / "test-evals.db"
    monkeypatch.setattr(db, "DB_PATH", db_path)
    return db
