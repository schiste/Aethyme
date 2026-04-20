"""Schema + roundtrip tests for the eval_results DB layer.

Covers the invariants the UI depends on:
- All declared columns apply on a fresh DB via `get_db()`.
- `insert_result` → `query_results` roundtrips every field we persist.
- Nullable fields survive None on insert and come back as None.
"""

from __future__ import annotations


REQUIRED_COLUMNS = {
    "id", "run_dir", "date", "eval_type", "target", "model", "condition",
    "reasoning", "cto", "score", "turns", "tool_calls", "total_tokens",
    "input_tokens", "output_tokens", "cache_read", "cache_create", "cost",
    "duration", "fixed", "scenario", "raw_json", "output", "tool_breakdown",
    "prompt", "run_id",
    # P3 judge
    "judge_score", "judge_stdev", "judge_model", "judge_reliable",
    "judge_samples", "judge_error", "judge_cost_usd",
    # P1 batch
    "batch_id", "run_index", "runs_in_batch",
    # Deliverable + pre-registration
    "deliverable_status", "primary_metric", "minimum_meaningful_delta",
}


def test_get_db_creates_all_columns(isolated_db):
    conn = isolated_db.get_db()
    rows = conn.execute("PRAGMA table_info(eval_results)").fetchall()
    cols = {r[1] for r in rows}
    missing = REQUIRED_COLUMNS - cols
    assert not missing, f"missing columns: {missing}"


def test_roundtrip_preserves_new_fields(isolated_db):
    isolated_db.insert_result({
        "id": "test-1",
        "date": "2026-04-20",
        "evalType": "bug-fix",
        "target": "grc",
        "model": "haiku",
        "condition": "explore",
        "reasoning": "high",
        "cto": "on",
        "score": 75,
        "qualityScore": 80.0,
        "recalculatedEvalScore": 78.0,
        "judgeScore": 82.5,
        "judgeReliable": True,
        "judgeStdev": 1.5,
        "batchId": "batch-1",
        "runIndex": 2,
        "runsInBatch": 3,
        "deliverableStatus": "success",
    })

    results = isolated_db.query_results()
    assert len(results) == 1
    row = results[0]
    assert row["id"] == "test-1"
    assert row["judgeScore"] == 82.5
    assert row["judgeReliable"] is True
    assert row["batchId"] == "batch-1"
    assert row["runIndex"] == 2
    assert row["runsInBatch"] == 3
    assert row["deliverableStatus"] == "success"


def test_missing_optional_fields_come_back_as_none(isolated_db):
    isolated_db.insert_result({
        "id": "minimal",
        "date": "2026-04-20",
        "evalType": "bug-fix",
        "target": "grc",
        "model": "haiku",
        "condition": "control-cto-off",
    })
    row = isolated_db.query_results()[0]
    # Everything judge/batch/deliverable is optional — must not error, must be None.
    assert row["judgeScore"] is None
    assert row["judgeReliable"] is None
    assert row["batchId"] is None
    assert row["deliverableStatus"] is None


def test_filter_by_condition(isolated_db):
    for cond in ["explore", "leverage", "control-cto-off"]:
        isolated_db.insert_result({
            "id": f"r-{cond}", "date": "2026-04-20",
            "evalType": "bug-fix", "target": "grc", "model": "haiku",
            "condition": cond,
        })
    explore = isolated_db.query_results(condition="explore")
    assert len(explore) == 1
    assert explore[0]["condition"] == "explore"


def test_upsert_replaces_existing_row(isolated_db):
    isolated_db.insert_result({
        "id": "dup", "date": "2026-04-20",
        "evalType": "bug-fix", "target": "grc", "model": "haiku",
        "condition": "explore", "qualityScore": 50,
    })
    isolated_db.insert_result({
        "id": "dup", "date": "2026-04-20",
        "evalType": "bug-fix", "target": "grc", "model": "haiku",
        "condition": "explore", "qualityScore": 90,
    })
    rows = isolated_db.query_results()
    assert len(rows) == 1
    assert rows[0]["qualityScore"] == 90
