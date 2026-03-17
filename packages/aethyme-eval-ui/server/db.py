"""SQLite database for eval results."""

from __future__ import annotations

import json
import re
import sqlite3
from pathlib import Path
from typing import Any

DB_PATH = Path(__file__).resolve().parent / "evals.db"

SCHEMA = """
CREATE TABLE IF NOT EXISTS eval_results (
    id            TEXT PRIMARY KEY,
    run_dir       TEXT,
    date          TEXT,
    eval_type     TEXT,
    target        TEXT,
    model         TEXT,
    condition     TEXT,
    reasoning     TEXT,
    cto           TEXT,
    score         REAL,
    turns         INTEGER,
    tool_calls    INTEGER,
    total_tokens  INTEGER,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    cache_read    INTEGER,
    cache_create  INTEGER,
    cost          REAL,
    duration      TEXT,
    fixed         INTEGER,
    scenario      TEXT,
    raw_json       TEXT,
    output         TEXT,
    tool_breakdown TEXT,
    prompt         TEXT,
    run_id         TEXT
);

CREATE INDEX IF NOT EXISTS idx_eval_type ON eval_results(eval_type);
CREATE INDEX IF NOT EXISTS idx_target ON eval_results(target);
CREATE INDEX IF NOT EXISTS idx_model ON eval_results(model);
CREATE INDEX IF NOT EXISTS idx_condition ON eval_results(condition);
CREATE INDEX IF NOT EXISTS idx_date ON eval_results(date);
"""

ALL_CONDITIONS = ("control-cto-off", "control-cto-on", "control", "explore", "leverage")

CTO_MAP = {
    "control-cto-off": "off",
    "control-cto-on": "on",
    "explore": "on",
    "leverage": "on",
    "control": "unknown",
}


MIGRATIONS = [
    "ALTER TABLE eval_results ADD COLUMN output TEXT",
    "ALTER TABLE eval_results ADD COLUMN tool_breakdown TEXT",
    "ALTER TABLE eval_results ADD COLUMN prompt TEXT",
    "ALTER TABLE eval_results ADD COLUMN run_id TEXT",
]


def get_db() -> sqlite3.Connection:
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.executescript(SCHEMA)
    # Run migrations — each is idempotent (fails silently if column exists)
    for migration in MIGRATIONS:
        try:
            conn.execute(migration)
        except sqlite3.OperationalError:
            pass  # Column already exists
    return conn


def import_eval_runs(eval_runs_dir: Path) -> int:
    """Scan eval-runs/ and import any runs not already in the DB."""
    if not eval_runs_dir.exists():
        return 0

    conn = get_db()
    existing = {row[0] for row in conn.execute("SELECT id FROM eval_results").fetchall()}
    imported = 0

    for entry in sorted(eval_runs_dir.iterdir()):
        if not entry.is_dir():
            continue
        if not re.match(r"\d{8}-\d{6}", entry.name):
            continue

        complete = entry / "complete-result.json"
        if not complete.exists():
            continue

        try:
            data = json.loads(complete.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            continue

        metadata = {}
        metadata_file = entry / "metadata.json"
        if metadata_file.exists():
            try:
                metadata = json.loads(metadata_file.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, OSError):
                pass

        eval_type = data.get("eval_type", metadata.get("eval_type", "unknown"))
        target = _extract_target(data, metadata)
        model_raw = data.get("model", metadata.get("model", {}))
        model_name = model_raw if isinstance(model_raw, str) else (model_raw.get("name", "unknown") if isinstance(model_raw, dict) else "unknown")
        reasoning = "default"
        if isinstance(model_raw, dict):
            reasoning = model_raw.get("reasoning", "default")
        scenario = data.get("scenario")

        timestamp = metadata.get("timestamp", "")
        date_str = timestamp[:19] if timestamp else entry.name[:15]

        for cond in ALL_CONDITIONS:
            side = data.get(cond)
            if not isinstance(side, dict):
                continue

            result_id = f"{entry.name}-{cond}"
            if result_id in existing:
                continue

            assessment = side.get("assessment") or {}
            run = side.get("run") or {}
            if not isinstance(assessment, dict):
                assessment = {}
            if not isinstance(run, dict):
                run = {}

            ws = assessment.get("weighted_score", 0)
            input_tokens = run.get("input_tokens") or 0
            output_tokens = run.get("output_tokens") or 0
            cache_read = run.get("cache_read_tokens", 0) or 0
            cache_create = run.get("cache_create_tokens", 0) or 0
            total = input_tokens + output_tokens + cache_read + cache_create
            cost = run.get("cost_usd", 0) or 0
            duration = run.get("duration_seconds")
            dur_str = f"{duration:.0f}s" if duration and duration > 0 else "-"
            turns = run.get("num_turns", 0) or 0
            tool_calls_data = run.get("tool_calls")
            tool_count = len(tool_calls_data) if isinstance(tool_calls_data, list) else 0
            test_pass = assessment.get("test_pass")
            fixed = 1 if (test_pass if test_pass is not None else (ws > 0)) else 0
            cto = CTO_MAP.get(cond, "unknown")

            conn.execute(
                """INSERT OR REPLACE INTO eval_results
                   (id, run_dir, date, eval_type, target, model, condition, reasoning,
                    cto, score, turns, tool_calls, total_tokens, input_tokens,
                    output_tokens, cache_read, cache_create, cost, duration, fixed,
                    scenario, raw_json)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                (
                    result_id, entry.name, date_str,
                    eval_type.split("-reasoning")[0] if "-reasoning" in eval_type else eval_type,
                    target, model_name, cond,
                    reasoning if reasoning != "default" else "high",
                    cto, round(ws, 2) if ws else 0,
                    turns, tool_count, total, input_tokens, output_tokens,
                    cache_read, cache_create, round(cost, 4), dur_str, fixed,
                    scenario, json.dumps(side),
                ),
            )
            imported += 1

    conn.commit()
    conn.close()
    return imported


def insert_result(result: dict[str, Any]) -> None:
    """Insert a single eval result row."""
    conn = get_db()
    conn.execute(
        """INSERT OR REPLACE INTO eval_results
           (id, run_dir, date, eval_type, target, model, condition, reasoning,
            cto, score, turns, tool_calls, total_tokens, input_tokens,
            output_tokens, cache_read, cache_create, cost, duration, fixed,
            scenario, raw_json, output, tool_breakdown, prompt, run_id)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
        (
            result["id"], result.get("runDir"), result["date"],
            result["evalType"], result["target"], result["model"],
            result["condition"], result.get("reasoning", "high"),
            result.get("cto", "unknown"), result.get("score", 0),
            result.get("turns", 0), result.get("toolCalls", 0),
            result.get("totalTokens", 0), result.get("inputTokens", 0),
            result.get("outputTokens", 0), result.get("cacheRead", 0),
            result.get("cacheCreate", 0), result.get("cost", 0),
            result.get("duration", "-"), result.get("fixed", 0),
            result.get("scenario"), result.get("rawJson"),
            result.get("output"), result.get("toolBreakdown"),
            result.get("prompt"), result.get("runId"),
        ),
    )
    conn.commit()
    conn.close()


def query_results(
    eval_type: str | None = None,
    target: str | None = None,
    model: str | None = None,
    condition: str | None = None,
) -> list[dict[str, Any]]:
    """Query eval results with optional filters."""
    conn = get_db()
    where = []
    params: list[Any] = []

    if eval_type:
        where.append("eval_type = ?")
        params.append(eval_type)
    if target:
        where.append("target = ?")
        params.append(target)
    if model:
        where.append("model = ?")
        params.append(model)
    if condition:
        where.append("condition = ?")
        params.append(condition)

    clause = f" WHERE {' AND '.join(where)}" if where else ""
    rows = conn.execute(
        f"SELECT * FROM eval_results{clause} ORDER BY date DESC", params
    ).fetchall()
    conn.close()

    return [_row_to_dict(row) for row in rows]


def _row_to_dict(row: sqlite3.Row) -> dict[str, Any]:
    return {
        "id": row["id"],
        "date": row["date"],
        "evalType": row["eval_type"],
        "target": row["target"],
        "model": row["model"],
        "condition": row["condition"],
        "reasoning": row["reasoning"],
        "cto": row["cto"],
        "score": row["score"],
        "turns": row["turns"],
        "toolCalls": row["tool_calls"],
        "totalTokens": row["total_tokens"],
        "inputTokens": row["input_tokens"],
        "outputTokens": row["output_tokens"],
        "cacheRead": row["cache_read"],
        "cacheCreate": row["cache_create"],
        "cost": row["cost"],
        "duration": row["duration"],
        "fixed": bool(row["fixed"]),
        "scenario": row["scenario"],
        "output": row["output"],
        "toolBreakdown": row["tool_breakdown"],
        "prompt": row["prompt"],
        "runId": row["run_id"] if "run_id" in row.keys() else None,
    }


def _extract_target(data: dict, metadata: dict) -> str:
    repo_path = metadata.get("repo_path", "")
    dir_name = data.get("report", {}).get("repo_path", repo_path) if isinstance(data.get("report"), dict) else repo_path
    lower = str(dir_name).lower()
    if "mediawiki" in lower:
        return "mediawiki"
    if "grc" in lower or "playground" in lower or "mockup" in lower:
        return "grc"
    return Path(dir_name).name if dir_name else "unknown"
