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
    run_id         TEXT,
    quality_score REAL,
    recalculated_eval_score REAL,
    quality_delta_vs_control REAL,
    token_ratio_vs_control REAL,
    time_ratio_vs_control REAL,
    cost_ratio_vs_control REAL,
    score_per_1k_tokens REAL,
    score_per_minute REAL,
    top_tools TEXT
);

CREATE INDEX IF NOT EXISTS idx_eval_type ON eval_results(eval_type);
CREATE INDEX IF NOT EXISTS idx_target ON eval_results(target);
CREATE INDEX IF NOT EXISTS idx_model ON eval_results(model);
CREATE INDEX IF NOT EXISTS idx_condition ON eval_results(condition);
CREATE INDEX IF NOT EXISTS idx_date ON eval_results(date);
"""

ALL_CONDITIONS = ("control-cto-off", "control-cto-on", "control", "explore", "leverage", "task-conditioned")

CTO_MAP = {
    "control-cto-off": "off",
    "control-cto-on": "on",
    "explore": "on",
    "leverage": "on",
    "task-conditioned": "on",
    "control": "unknown",
}


MIGRATIONS = [
    "ALTER TABLE eval_results ADD COLUMN output TEXT",
    "ALTER TABLE eval_results ADD COLUMN tool_breakdown TEXT",
    "ALTER TABLE eval_results ADD COLUMN prompt TEXT",
    "ALTER TABLE eval_results ADD COLUMN run_id TEXT",
    "ALTER TABLE eval_results ADD COLUMN quality_score REAL",
    "ALTER TABLE eval_results ADD COLUMN recalculated_eval_score REAL",
    "ALTER TABLE eval_results ADD COLUMN quality_delta_vs_control REAL",
    "ALTER TABLE eval_results ADD COLUMN token_ratio_vs_control REAL",
    "ALTER TABLE eval_results ADD COLUMN time_ratio_vs_control REAL",
    "ALTER TABLE eval_results ADD COLUMN cost_ratio_vs_control REAL",
    "ALTER TABLE eval_results ADD COLUMN score_per_1k_tokens REAL",
    "ALTER TABLE eval_results ADD COLUMN score_per_minute REAL",
    "ALTER TABLE eval_results ADD COLUMN top_tools TEXT",
    # LLM-as-judge columns (P3: intra-rater reliability scoring)
    "ALTER TABLE eval_results ADD COLUMN judge_score REAL",
    "ALTER TABLE eval_results ADD COLUMN judge_stdev REAL",
    "ALTER TABLE eval_results ADD COLUMN judge_model TEXT",
    "ALTER TABLE eval_results ADD COLUMN judge_reliable INTEGER",
    "ALTER TABLE eval_results ADD COLUMN judge_samples TEXT",
    "ALTER TABLE eval_results ADD COLUMN judge_error TEXT",
    "ALTER TABLE eval_results ADD COLUMN judge_cost_usd REAL",
    # P1: multi-run support
    "ALTER TABLE eval_results ADD COLUMN batch_id TEXT",
    "ALTER TABLE eval_results ADD COLUMN run_index INTEGER",
    "ALTER TABLE eval_results ADD COLUMN runs_in_batch INTEGER",
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
    """Scan eval-runs/ and upsert all discovered runs into the DB."""
    if not eval_runs_dir.exists():
        return 0

    conn = get_db()
    imported = 0

    for entry in sorted(eval_runs_dir.iterdir()):
        if not entry.is_dir():
            continue
        if not re.match(r"\d{8}(?:T|-)\d{6}", entry.name):
            continue

        complete = entry / "complete-result.json"
        if not complete.exists():
            continue

        try:
            data = json.loads(complete.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            continue

        judge_by_condition = _load_judge_results(entry)

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

            assessment = side.get("assessment") or {}
            run = side.get("run") or {}
            summary = side.get("summary_metrics") or {}
            if not isinstance(assessment, dict):
                assessment = {}
            if not isinstance(run, dict):
                run = {}
            if not isinstance(summary, dict):
                summary = {}

            ws = assessment.get("weighted_score", 0)
            quality_score = summary.get("quality_score", ws)
            recalculated_eval_score = summary.get("recalculated_eval_score", summary.get("global_score"))
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
            tool_breakdown = _tool_breakdown_json(run.get("tool_calls"))
            judge = judge_by_condition.get(cond, {})

            conn.execute(
                """INSERT OR REPLACE INTO eval_results
                   (id, run_dir, date, eval_type, target, model, condition, reasoning,
                    cto, score, turns, tool_calls, total_tokens, input_tokens,
                    output_tokens, cache_read, cache_create, cost, duration, fixed,
                    scenario, raw_json, output, tool_breakdown, prompt, run_id,
                    quality_score, recalculated_eval_score, quality_delta_vs_control,
                    token_ratio_vs_control, time_ratio_vs_control, cost_ratio_vs_control,
                    score_per_1k_tokens, score_per_minute, top_tools,
                    judge_score, judge_stdev, judge_model, judge_reliable,
                    judge_samples, judge_error, judge_cost_usd)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                           ?, ?, ?, ?, ?, ?,
                           ?, ?, ?, ?, ?, ?, ?, ?, ?,
                           ?, ?, ?, ?, ?, ?, ?)""",
                (
                    result_id, entry.name, date_str,
                    eval_type.split("-reasoning")[0] if "-reasoning" in eval_type else eval_type,
                    target, model_name, cond,
                    reasoning if reasoning != "default" else "high",
                    cto, round(ws, 2) if ws else 0,
                    turns, tool_count, total, input_tokens, output_tokens,
                    cache_read, cache_create, round(cost, 4), dur_str, fixed,
                    scenario, json.dumps(side),
                    run.get("output"),
                    tool_breakdown,
                    side.get("prompt"),
                    ((run.get("run_metadata") or {}).get("run_id") if isinstance(run.get("run_metadata"), dict) else None),
                    round(quality_score, 2) if quality_score is not None else None,
                    round(recalculated_eval_score, 2) if recalculated_eval_score is not None else None,
                    _maybe_round(summary.get("quality_delta_vs_control")),
                    _maybe_round(summary.get("token_ratio_vs_control")),
                    _maybe_round(summary.get("time_ratio_vs_control")),
                    _maybe_round(summary.get("cost_ratio_vs_control")),
                    _maybe_round(summary.get("score_per_1k_tokens")),
                    _maybe_round(summary.get("score_per_minute")),
                    json.dumps(summary.get("top_tools")) if summary.get("top_tools") else None,
                    judge.get("judgeScore"),
                    judge.get("judgeStdev"),
                    judge.get("judgeModel"),
                    judge.get("judgeReliable"),
                    judge.get("judgeSamples"),
                    judge.get("judgeError"),
                    judge.get("judgeCostUsd"),
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
            scenario, raw_json, output, tool_breakdown, prompt, run_id,
            quality_score, recalculated_eval_score, quality_delta_vs_control,
            token_ratio_vs_control, time_ratio_vs_control, cost_ratio_vs_control,
            score_per_1k_tokens, score_per_minute, top_tools,
            judge_score, judge_stdev, judge_model, judge_reliable,
            judge_samples, judge_error, judge_cost_usd,
            batch_id, run_index, runs_in_batch)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                   ?, ?, ?, ?, ?, ?,
                   ?, ?, ?, ?, ?, ?, ?, ?, ?,
                   ?, ?, ?, ?, ?, ?, ?,
                   ?, ?, ?)""",
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
            result.get("qualityScore"),
            result.get("recalculatedEvalScore"),
            result.get("qualityDeltaVsControl"),
            result.get("tokenRatioVsControl"),
            result.get("timeRatioVsControl"),
            result.get("costRatioVsControl"),
            result.get("scorePer1kTokens"),
            result.get("scorePerMinute"),
            result.get("topTools"),
            # LLM judge fields
            result.get("judgeScore"),
            result.get("judgeStdev"),
            result.get("judgeModel"),
            1 if result.get("judgeReliable") else (0 if result.get("judgeReliable") is False else None),
            result.get("judgeSamples"),
            result.get("judgeError"),
            result.get("judgeCostUsd"),
            # Batch fields (P1 multi-run)
            result.get("batchId"),
            result.get("runIndex"),
            result.get("runsInBatch"),
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
        "qualityScore": row["quality_score"] if "quality_score" in row.keys() else None,
        "recalculatedEvalScore": row["recalculated_eval_score"] if "recalculated_eval_score" in row.keys() else None,
        "qualityDeltaVsControl": row["quality_delta_vs_control"] if "quality_delta_vs_control" in row.keys() else None,
        "tokenRatioVsControl": row["token_ratio_vs_control"] if "token_ratio_vs_control" in row.keys() else None,
        "timeRatioVsControl": row["time_ratio_vs_control"] if "time_ratio_vs_control" in row.keys() else None,
        "costRatioVsControl": row["cost_ratio_vs_control"] if "cost_ratio_vs_control" in row.keys() else None,
        "scorePer1kTokens": row["score_per_1k_tokens"] if "score_per_1k_tokens" in row.keys() else None,
        "scorePerMinute": row["score_per_minute"] if "score_per_minute" in row.keys() else None,
        "topTools": row["top_tools"] if "top_tools" in row.keys() else None,
        # LLM judge fields (P3)
        "judgeScore": row["judge_score"] if "judge_score" in row.keys() else None,
        "judgeStdev": row["judge_stdev"] if "judge_stdev" in row.keys() else None,
        "judgeModel": row["judge_model"] if "judge_model" in row.keys() else None,
        "judgeReliable": bool(row["judge_reliable"]) if "judge_reliable" in row.keys() and row["judge_reliable"] is not None else None,
        "judgeSamples": row["judge_samples"] if "judge_samples" in row.keys() else None,
        "judgeError": row["judge_error"] if "judge_error" in row.keys() else None,
        "judgeCostUsd": row["judge_cost_usd"] if "judge_cost_usd" in row.keys() else None,
        # Multi-run fields (P1)
        "batchId": row["batch_id"] if "batch_id" in row.keys() else None,
        "runIndex": row["run_index"] if "run_index" in row.keys() else None,
        "runsInBatch": row["runs_in_batch"] if "runs_in_batch" in row.keys() else None,
    }


def _maybe_round(value: Any, digits: int = 4) -> float | None:
    if value is None:
        return None
    try:
        return round(float(value), digits)
    except (TypeError, ValueError):
        return None


def _tool_breakdown_json(tool_calls: Any) -> str | None:
    if not isinstance(tool_calls, list):
        return None
    breakdown: dict[str, int] = {}
    for call in tool_calls:
        if not isinstance(call, dict):
            continue
        name = call.get("tool") or call.get("name")
        if not name:
            continue
        breakdown[str(name)] = breakdown.get(str(name), 0) + 1
    if not breakdown:
        return None
    return json.dumps({k: v for k, v in sorted(breakdown.items(), key=lambda item: (-item[1], item[0]))})


def _extract_target(data: dict, metadata: dict) -> str:
    for candidate in (
        data.get("target"),
        metadata.get("target"),
        data.get("target_slug"),
        metadata.get("target_slug"),
    ):
        if isinstance(candidate, str) and candidate.strip():
            return candidate.strip()

    for cond in ALL_CONDITIONS:
        side = data.get(cond)
        if not isinstance(side, dict):
            continue
        run = side.get("run")
        if not isinstance(run, dict):
            continue
        run_metadata = run.get("run_metadata")
        if not isinstance(run_metadata, dict):
            continue
        candidate = run_metadata.get("target")
        if isinstance(candidate, str) and candidate.strip():
            return candidate.strip()

    repo_path = metadata.get("repo_path", "")
    dir_name = data.get("report", {}).get("repo_path", repo_path) if isinstance(data.get("report"), dict) else repo_path
    lower = str(dir_name).lower()
    if "mediawiki" in lower:
        return "mediawiki"
    if "aethyme" in lower:
        return "aethyme"
    if "grc" in lower or "mockup" in lower:
        return "grc"
    return Path(dir_name).name if dir_name else "unknown"


def _load_judge_results(run_dir: Path) -> dict[str, dict[str, Any]]:
    events_path = run_dir / "events.log"
    if not events_path.exists():
        return {}

    try:
        lines = events_path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return {}

    samples_by_condition: dict[str, list[float]] = {}
    results: dict[str, dict[str, Any]] = {}
    sample_re = re.compile(r"\[(?P<condition>[^\]]+)\]\s+sample\s+\d+/\d+:\s+score=(?P<score>-?\d+(?:\.\d+)?)")
    final_re = re.compile(
        r"\[(?P<condition>[^\]]+)\]\s+Judge score:\s+"
        r"(?P<score>-?\d+(?:\.\d+)?)\s+"
        r"\(stdev=(?P<stdev>-?\d+(?:\.\d+)?),\s+"
        r"reliable=(?P<reliable>True|False)\)"
    )

    for line in lines:
        sample_match = sample_re.search(line)
        if sample_match:
            condition = sample_match.group("condition")
            samples_by_condition.setdefault(condition, []).append(float(sample_match.group("score")))
            continue

        final_match = final_re.search(line)
        if final_match:
            condition = final_match.group("condition")
            samples = samples_by_condition.get(condition, [])
            results[condition] = {
                "judgeScore": float(final_match.group("score")),
                "judgeStdev": float(final_match.group("stdev")),
                "judgeModel": "codex",
                "judgeReliable": 1 if final_match.group("reliable") == "True" else 0,
                "judgeSamples": json.dumps([{"score": score} for score in samples]),
                "judgeError": None,
                "judgeCostUsd": None,
            }

    return results
