from __future__ import annotations

import asyncio
import hashlib
import json
import subprocess
import uuid
from collections import Counter
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from fastapi import BackgroundTasks, FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, Field

from db import import_eval_runs, query_results, insert_result

AETHYME_PKG = Path(__file__).resolve().parents[2] / "aethyme"
AETHYME_PYTHON = AETHYME_PKG / ".venv" / "bin" / "python"
ENGINE_BINARY = AETHYME_PKG / "rust" / "target" / "release" / "aethyme-engine-cli"
EVAL_RUNS_DIR = AETHYME_PKG / "eval-runs"
PREPARATIONS_ROOT = EVAL_RUNS_DIR / "preparations"

app = FastAPI(title="Aethyme Eval API")

app.add_middleware(
    CORSMiddleware,
    allow_origins=["http://localhost:5173"],
    allow_methods=["*"],
    allow_headers=["*"],
)

_index_tasks: dict[str, dict[str, Any]] = {}
_setup_tasks: dict[str, dict[str, Any]] = {}


@app.on_event("startup")
async def startup():
    count = import_eval_runs(EVAL_RUNS_DIR)
    print(f"Imported {count} eval results from {EVAL_RUNS_DIR}")


# ---------------------------------------------------------------------------
# Results
# ---------------------------------------------------------------------------

@app.get("/api/results")
async def get_results(
    eval_type: str | None = None,
    target: str | None = None,
    model: str | None = None,
    condition: str | None = None,
) -> list[dict[str, Any]]:
    return query_results(eval_type=eval_type, target=target, model=model, condition=condition)


@app.get("/api/batches")
async def get_batches(limit: int = 50) -> list[dict[str, Any]]:
    """List recent multi-run batches with minimal summary."""
    from batch_stats import list_batches
    return list_batches(limit=limit)


@app.get("/api/batches/{batch_id}")
async def get_batch(batch_id: str) -> dict[str, Any]:
    """Return aggregated per-condition statistics for a batch.

    Response: {batch: <meta>, conditions: {<cond>: {<field>: {median, q1, q3,
    iqr, min, max, n}, ..., deliverable_success_rate, judge_reliability_rate}}}.
    Any condition with fewer than 4 runs reports median/min/max but omits
    Q1/Q3/IQR (quantiles undefined at that sample size).
    """
    from batch_stats import aggregate_batch
    return aggregate_batch(batch_id)


@app.post("/api/results")
async def add_result(result: dict[str, Any]) -> dict[str, str]:
    insert_result(result)
    return {"status": "ok"}


@app.get("/api/results/{result_id}/output")
async def get_result_output(result_id: str) -> dict[str, Any]:
    from db import get_db
    try:
        conn = get_db()
        row = conn.execute("SELECT output FROM eval_results WHERE id = ?", (result_id,)).fetchone()
    finally:
        conn.close()
    if not row:
        raise HTTPException(status_code=404, detail="Result not found")
    return {"id": result_id, "output": row["output"]}


@app.post("/api/results/reimport")
async def reimport_results() -> dict[str, Any]:
    count = import_eval_runs(EVAL_RUNS_DIR)
    return {"imported": count}


# ---------------------------------------------------------------------------
# Repositories
# ---------------------------------------------------------------------------

def _check_control_clean(control_path: Path) -> dict[str, Any]:
    """Check that the Control repo has no Aethyme contamination."""
    contamination = []

    skills_dirs = [
        control_path / ".codex" / "skills",
        control_path / ".codex" / "skills" / "aethyme",
        control_path / ".codex" / "skills" / "aethyme-navigation",
    ]
    for sd in skills_dirs:
        if sd.exists():
            contamination.append(f"Skill found: {sd.relative_to(control_path)}")

    graph_db = control_path / ".aethyme" / "graph.db"
    if graph_db.exists():
        contamination.append(f"Graph index found: .aethyme/graph.db")

    snippets_json = control_path / ".chau7" / "snippets.json"
    if snippets_json.exists():
        try:
            data = json.loads(snippets_json.read_text(encoding="utf-8"))
            aethyme_snippets = [s for s in data.get("snippets", []) if "aethyme" in (s.get("tags") or [])]
            if aethyme_snippets:
                contamination.append(f"Aethyme snippets found in .chau7/snippets.json ({len(aethyme_snippets)} snippets)")
        except (json.JSONDecodeError, OSError):
            pass

    return {
        "clean": len(contamination) == 0,
        "issues": contamination,
    }


def _check_index(repo_path: Path) -> dict[str, Any]:
    """Check if a repo has a SurrealDB graph index."""
    from datetime import datetime, timezone

    graph_db = repo_path / ".aethyme" / "graph.db"
    if not graph_db.exists():
        return {"indexed": False, "date": None, "path": str(graph_db)}

    try:
        stat = graph_db.stat()
        date = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat()
        # graph.db is a directory (SurrealKV) — sum contents for real size
        if graph_db.is_dir():
            total_bytes = sum(f.stat().st_size for f in graph_db.rglob("*") if f.is_file())
        else:
            total_bytes = stat.st_size
        size_mb = round(total_bytes / 1024 / 1024, 2)
        return {"indexed": True, "date": date, "sizeMb": size_mb, "path": str(graph_db)}
    except OSError:
        return {"indexed": False, "date": None, "path": str(graph_db)}


def _check_snippets(*repo_paths: Path) -> dict[str, Any]:
    """Check if any of the repos have Chau7 snippets generated."""
    from datetime import datetime, timezone

    for repo_path in repo_paths:
        snippets_file = repo_path / ".chau7" / "snippets.json"
        if snippets_file.exists():
            try:
                stat = snippets_file.stat()
                date = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat()
                data = json.loads(snippets_file.read_text(encoding="utf-8"))
                count = len(data.get("snippets", []))
                aethyme_count = sum(
                    1 for s in data.get("snippets", [])
                    if "aethyme" in (s.get("tags") or [])
                )
                return {
                    "present": True,
                    "date": date,
                    "totalSnippets": count,
                    "aethymeSnippets": aethyme_count,
                    "path": str(snippets_file),
                    "repo": str(repo_path),
                }
            except (json.JSONDecodeError, OSError):
                pass

    return {"present": False, "date": None, "totalSnippets": 0, "aethymeSnippets": 0}


def _git_repo_state(repo_path: Path) -> dict[str, Any]:
    if not repo_path.exists():
        return {"path": str(repo_path), "commit": None, "dirty": None}
    try:
        commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=repo_path,
            text=True,
        ).strip()
    except Exception:
        commit = None
    try:
        dirty = bool(
            subprocess.check_output(
                ["git", "status", "--porcelain"],
                cwd=repo_path,
                text=True,
            ).strip()
        )
    except Exception:
        dirty = None
    return {"path": str(repo_path), "commit": commit, "dirty": dirty}


def _preparation_file(preparation_id: str) -> Path:
    return PREPARATIONS_ROOT / f"{preparation_id}.json"


def _fetch_judge_and_batch_fields(row_id: str) -> dict[str, Any]:
    """Read judge + batch columns for an existing row, so a later INSERT OR REPLACE
    doesn't wipe them. Returns camelCase keys ready to splat into insert_result()."""
    import sqlite3
    db_path = Path(__file__).resolve().parent / "evals.db"
    if not db_path.exists():
        return {}
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        row = conn.execute(
            "SELECT judge_score, judge_stdev, judge_model, judge_reliable, "
            "judge_samples, judge_error, judge_cost_usd, judge_elapsed_seconds, "
            "batch_id, run_index, runs_in_batch, deliverable_status, "
            "primary_metric, minimum_meaningful_delta "
            "FROM eval_results WHERE id = ?",
            (row_id,),
        ).fetchone()
    except sqlite3.OperationalError:
        return {}
    finally:
        conn.close()
    if row is None:
        return {}
    return {
        "judgeScore": row["judge_score"],
        "judgeStdev": row["judge_stdev"],
        "judgeModel": row["judge_model"],
        "judgeReliable": (bool(row["judge_reliable"]) if row["judge_reliable"] is not None else None),
        "judgeSamples": row["judge_samples"],
        "judgeError": row["judge_error"],
        "judgeCostUsd": row["judge_cost_usd"],
        "judgeElapsedSeconds": row["judge_elapsed_seconds"],
        "batchId": row["batch_id"],
        "runIndex": row["run_index"],
        "runsInBatch": row["runs_in_batch"],
        "deliverableStatus": row["deliverable_status"],
        "primaryMetric": row["primary_metric"],
        "minimumMeaningfulDelta": row["minimum_meaningful_delta"],
    }


def _write_preparation_snapshot(target: str, payload: dict[str, Any]) -> dict[str, Any]:
    PREPARATIONS_ROOT.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%S")
    preparation_id = f"{stamp}-{target}"
    path = _preparation_file(preparation_id)
    snapshot = {
        "id": preparation_id,
        "target": target,
        "createdAt": datetime.now(UTC).isoformat(),
        **payload,
        "path": str(path),
    }
    path.write_text(json.dumps(snapshot, indent=2), encoding="utf-8")
    return snapshot


def _read_preparation_snapshot(preparation_id: str) -> dict[str, Any] | None:
    path = _preparation_file(preparation_id)
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def _latest_preparation_snapshot(target: str) -> dict[str, Any] | None:
    if not PREPARATIONS_ROOT.exists():
        return None
    candidates = sorted(PREPARATIONS_ROOT.glob(f"*-{target}.json"))
    if not candidates:
        return None
    latest = candidates[-1]
    try:
        return json.loads(latest.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def _setup_task_request_payload(*, target: str, source: str, commit: str, force: bool) -> dict[str, Any]:
    return {
        "target": target,
        "source": source,
        "commit": commit,
        "force": force,
    }


def _persist_setup_task_state(
    task_id: str,
    *,
    status: str,
    request: dict[str, Any],
    started_at: str | None = None,
    finished_at: str | None = None,
    skipped: bool | None = None,
    output: str | None = None,
    error: str | None = None,
    command: list[str] | None = None,
    return_code: int | None = None,
    preparation: dict[str, Any] | None = None,
) -> None:
    _ensure_aethyme_imports()
    from src.eval.observability import write_setup_task_result

    payload: dict[str, Any] = {
        "status": status,
        "request": request,
    }
    if started_at:
        payload["started_at"] = started_at
    if finished_at:
        payload["finished_at"] = finished_at
    if skipped is not None:
        payload["skipped"] = skipped
    if output is not None:
        payload["output"] = output
    if error is not None:
        payload["error"] = error
    if command is not None:
        payload["command"] = command
    if return_code is not None:
        payload["return_code"] = return_code
    if preparation is not None:
        payload["preparation"] = preparation
    write_setup_task_result(task_id, payload)


def _append_setup_task_log(task_id: str, message: str) -> None:
    _ensure_aethyme_imports()
    from src.eval.observability import append_log, setup_task_dir

    append_log(setup_task_dir(task_id) / "events.log", message)


def _write_setup_task_stream(task_id: str, name: str, content: str) -> None:
    _ensure_aethyme_imports()
    from src.eval.observability import setup_task_dir

    task_dir = setup_task_dir(task_id)
    task_dir.mkdir(parents=True, exist_ok=True)
    (task_dir / name).write_text(content, encoding="utf-8")


def _read_setup_task_state(task_id: str) -> dict[str, Any] | None:
    _ensure_aethyme_imports()
    from src.eval.observability import setup_task_dir

    result_path = setup_task_dir(task_id) / "result.json"
    request_path = setup_task_dir(task_id) / "request.json"
    if not result_path.exists():
        return None
    try:
        result = json.loads(result_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    status = {
        "status": result.get("status", "unknown"),
        "target": result.get("request", {}).get("target"),
        "source": result.get("request", {}).get("source"),
        "commit": result.get("request", {}).get("commit"),
        "force": result.get("request", {}).get("force"),
        "path": str(setup_task_dir(task_id)),
        "skipped": result.get("skipped"),
        "output": result.get("output"),
        "error": result.get("error"),
        "preparation": result.get("preparation"),
        "startedAt": result.get("started_at"),
        "finishedAt": result.get("finished_at"),
    }
    if request_path.exists():
        status["requestPath"] = str(request_path)
    return status


def _plan_request_payload(req: Any) -> dict[str, Any]:
    return {
        "evalType": req.evalType,
        "target": req.target,
        "model": req.model,
        "reasoning": req.reasoning,
        "windowId": getattr(req, "windowId", None),
        "preparationId": getattr(req, "preparationId", None),
        "cleanupDelaySeconds": getattr(req, "cleanupDelaySeconds", 1),
    }


async def _generate_plan(req: Any) -> tuple[dict[str, Any], dict[str, Any]]:
    _ensure_aethyme_imports()
    from src.eval.observability import persist_plan_snapshot

    reasoning_arg = "default" if req.reasoning == "high" else req.reasoning
    cmd = [
        str(AETHYME_PYTHON), "-m", "src.cli", "eval", "run",
        "--eval-type", req.evalType,
        "--target", req.target,
        "--model", req.model,
        "--reasoning", reasoning_arg,
        "--json-output",
    ]
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            cwd=str(AETHYME_PKG),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await proc.communicate()
    except FileNotFoundError as exc:
        raise HTTPException(status_code=500, detail=f"Python venv not found at {AETHYME_PYTHON}") from exc

    if proc.returncode != 0:
        raise HTTPException(status_code=500, detail=f"Plan generation failed: {stderr.decode()[:1000]}")

    try:
        plan = json.loads(stdout.decode())
    except json.JSONDecodeError as exc:
        raise HTTPException(status_code=500, detail=f"Failed to parse plan output: {stdout.decode()[:500]}") from exc

    plan_snapshot = persist_plan_snapshot(plan=plan, request=_plan_request_payload(req))
    cleanup_delay_seconds = getattr(req, "cleanupDelaySeconds", 1)
    plan.setdefault("runtime", {})
    plan["runtime"]["cleanupDelaySeconds"] = cleanup_delay_seconds
    for phase in plan.get("phases", []):
        if phase.get("name") == "cleanup":
            phase["cleanup_delay_seconds"] = cleanup_delay_seconds
            break
    plan["observability"] = {
        "planId": plan_snapshot["id"],
        "planPath": plan_snapshot["path"],
    }
    return plan, plan_snapshot


def _build_repository_preparation(target: str) -> dict[str, Any]:
    _ensure_aethyme_imports()
    from src.eval.targets import get_target

    eval_target = get_target(target)
    validation_errors = eval_target.validate()
    validation = {"valid": len(validation_errors) == 0, "errors": validation_errors}
    control_clean = _check_control_clean(eval_target.control_path)
    aethyme_index = _check_index(eval_target.aethyme_path)
    snippets = _check_snippets(eval_target.control_path, eval_target.aethyme_path)
    engine_binary = {"exists": ENGINE_BINARY.exists(), "path": str(ENGINE_BINARY)}
    control_repo = _git_repo_state(eval_target.control_path)
    aethyme_repo = _git_repo_state(eval_target.aethyme_path)

    errors = [*validation_errors]
    if not control_clean["clean"]:
        errors.extend(control_clean["issues"])
    if not engine_binary["exists"]:
        errors.append(f"Engine binary not found at {ENGINE_BINARY}")

    return {
        "ready": len(errors) == 0,
        "errors": errors,
        "checks": {
            "validation": validation,
            "controlClean": control_clean,
            "engineBinary": engine_binary,
            "aethymeIndex": aethyme_index,
            "snippets": snippets,
            "controlRepo": control_repo,
            "aethymeRepo": aethyme_repo,
        },
    }


@app.get("/api/repositories")
async def get_repositories() -> list[dict[str, Any]]:
    cmd = (
        'from src.eval.targets import list_targets; '
        'import json; '
        'print(json.dumps([{**t.to_dict(), "errors": t.validate()} for t in list_targets()]))'
    )
    try:
        proc = await asyncio.create_subprocess_exec(
            str(AETHYME_PYTHON), "-c", cmd,
            cwd=str(AETHYME_PKG),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await proc.communicate()
    except FileNotFoundError:
        raise HTTPException(status_code=500, detail=f"Python venv not found at {AETHYME_PYTHON}")

    if proc.returncode != 0:
        raise HTTPException(status_code=500, detail=f"targets command failed: {stderr.decode()}")

    try:
        targets = json.loads(stdout.decode())
    except json.JSONDecodeError:
        raise HTTPException(status_code=500, detail=f"Failed to parse targets output: {stdout.decode()[:500]}")

    from datetime import datetime, timezone

    repos = []
    for t in targets:
        errors = t.get("errors", [])
        status = "valid" if not errors else "invalid"

        control_path = Path(t.get("control_path", ""))
        aethyme_path = Path(t.get("aethyme_path", ""))

        control_clean = _check_control_clean(control_path)
        aethyme_index = _check_index(aethyme_path)
        snippets_info = _check_snippets(aethyme_path)

        repos.append({
            "name": t.get("display_name", t.get("name", "")),
            "target": t.get("name", ""),
            "controlPath": t.get("control_path", ""),
            "aethymePath": t.get("aethyme_path", ""),
            "setupSource": t.get("setup_source"),
            "setupCommit": t.get("setup_commit"),
            "setupControlDirName": t.get("setup_control_dir_name"),
            "setupAethymeDirName": t.get("setup_aethyme_dir_name"),
            "setupDest": str(control_path.parent) if control_path else "",
            "validationStatus": status,
            "controlClean": control_clean,
            "aethymeIndex": aethyme_index,
            "snippets": snippets_info,
        })

    return repos


class ValidateRequest(BaseModel):
    target: str

@app.post("/api/repositories/validate")
async def validate_repository(req: ValidateRequest) -> dict[str, Any]:
    target_escaped = req.target.replace('"', '\\"')
    cmd = (
        'from src.eval.targets import get_target; '
        'import json; '
        f't = get_target("{target_escaped}"); '
        'errors = t.validate(); '
        'print(json.dumps({"valid": len(errors) == 0, "errors": errors}))'
    )
    try:
        proc = await asyncio.create_subprocess_exec(
            str(AETHYME_PYTHON), "-c", cmd,
            cwd=str(AETHYME_PKG),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await proc.communicate()
    except FileNotFoundError:
        return {"valid": False, "errors": [f"Python venv not found at {AETHYME_PYTHON}"]}

    if proc.returncode != 0:
        return {"valid": False, "errors": [f"Validation failed: {stderr.decode()[:500]}"]}

    try:
        return json.loads(stdout.decode())
    except json.JSONDecodeError:
        return {"valid": False, "errors": [f"Failed to parse output: {stdout.decode()[:500]}"]}


class PrepareRepositoryRequest(BaseModel):
    target: str


@app.post("/api/repositories/prepare")
async def prepare_repository(req: PrepareRepositoryRequest) -> dict[str, Any]:
    snapshot = _write_preparation_snapshot(req.target, _build_repository_preparation(req.target))
    return snapshot


@app.get("/api/repositories/prepare/{target}")
async def get_latest_repository_preparation(target: str) -> dict[str, Any]:
    snapshot = _latest_preparation_snapshot(target)
    if snapshot is None:
        raise HTTPException(status_code=404, detail="No preparation snapshot found")
    return snapshot


class IndexRequest(BaseModel):
    target: str


class SetupRequest(BaseModel):
    target: str
    source: str | None = None
    commit: str | None = None
    force: bool = False

def _run_index(target: str, task_id: str) -> None:
    _index_tasks[task_id]["status"] = "running"
    try:
        target_escaped = target.replace('"', '\\"')
        cmd_str = (
            'from src.eval.targets import get_target; '
            f't = get_target("{target_escaped}"); '
            'print(str(t.aethyme_path))'
        )
        result = subprocess.run(
            [str(AETHYME_PYTHON), "-c", cmd_str],
            cwd=str(AETHYME_PKG),
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            _index_tasks[task_id]["status"] = "error"
            _index_tasks[task_id]["error"] = result.stderr[:500]
            return

        repo_path = result.stdout.strip()

        proc = subprocess.run(
            [str(ENGINE_BINARY), "index", "--repo", repo_path],
            capture_output=True, text=True, timeout=600,
        )
        if proc.returncode != 0:
            _index_tasks[task_id]["status"] = "error"
            _index_tasks[task_id]["error"] = proc.stderr[:1000]
        else:
            _index_tasks[task_id]["status"] = "complete"
            _index_tasks[task_id]["output"] = proc.stderr[:2000]
    except subprocess.TimeoutExpired:
        _index_tasks[task_id]["status"] = "error"
        _index_tasks[task_id]["error"] = "Indexing timed out after 600s"
    except Exception as e:
        _index_tasks[task_id]["status"] = "error"
        _index_tasks[task_id]["error"] = str(e)


def _run_setup_target(
    target: str,
    source: str,
    commit: str,
    force: bool,
    task_id: str,
) -> None:
    request_payload = _setup_task_request_payload(
        target=target,
        source=source,
        commit=commit,
        force=force,
    )
    started_at = datetime.now(UTC).isoformat()
    _setup_tasks[task_id]["status"] = "running"
    _setup_tasks[task_id]["startedAt"] = started_at
    _append_setup_task_log(task_id, f"Starting setup for {target} at commit {commit}")
    _persist_setup_task_state(
        task_id,
        status="running",
        request=request_payload,
        started_at=started_at,
    )
    try:
        current_snapshot = _build_repository_preparation(target)
        if current_snapshot.get("ready") and not force:
            snapshot = _write_preparation_snapshot(target, current_snapshot)
            finished_at = datetime.now(UTC).isoformat()
            _setup_tasks[task_id]["status"] = "complete"
            _setup_tasks[task_id]["skipped"] = True
            _setup_tasks[task_id]["output"] = "Playground already ready; setup skipped."
            _setup_tasks[task_id]["preparation"] = snapshot
            _setup_tasks[task_id]["finishedAt"] = finished_at
            _append_setup_task_log(task_id, "Playground already ready; setup skipped.")
            _persist_setup_task_state(
                task_id,
                status="complete",
                request=request_payload,
                started_at=started_at,
                finished_at=finished_at,
                skipped=True,
                output="Playground already ready; setup skipped.",
                preparation=snapshot,
            )
            return

        _ensure_aethyme_imports()
        from src.eval.targets import get_target

        eval_target = get_target(target)
        dest_dir = str(eval_target.control_path.parent)
        script = AETHYME_PKG / "scripts" / "eval" / "setup-playground.sh"
        cmd = [
            str(script),
            "--source", source,
            "--name", eval_target.name,
            "--commit", commit,
            "--dest", dest_dir,
        ]
        if eval_target.setup_control_dir_name:
            cmd.extend(["--control-dir-name", eval_target.setup_control_dir_name])
        if eval_target.setup_aethyme_dir_name:
            cmd.extend(["--aethyme-dir-name", eval_target.setup_aethyme_dir_name])
        if force:
            cmd.append("--force")

        _append_setup_task_log(task_id, f"Executing setup-playground.sh for {target}")
        proc = subprocess.run(
            cmd,
            cwd=str(AETHYME_PKG),
            capture_output=True,
            text=True,
            timeout=1800,
        )
        combined_output = proc.stdout + proc.stderr
        _setup_tasks[task_id]["output"] = combined_output[-8000:]
        _write_setup_task_stream(task_id, "stdout.log", proc.stdout)
        _write_setup_task_stream(task_id, "stderr.log", proc.stderr)
        if proc.returncode != 0:
            finished_at = datetime.now(UTC).isoformat()
            _setup_tasks[task_id]["status"] = "error"
            _setup_tasks[task_id]["error"] = f"setup-playground failed with exit code {proc.returncode}"
            _setup_tasks[task_id]["finishedAt"] = finished_at
            _append_setup_task_log(task_id, f"Setup failed with exit code {proc.returncode}")
            _persist_setup_task_state(
                task_id,
                status="error",
                request=request_payload,
                started_at=started_at,
                finished_at=finished_at,
                output=combined_output,
                error=f"setup-playground failed with exit code {proc.returncode}",
                command=cmd,
                return_code=proc.returncode,
            )
            return

        snapshot = _write_preparation_snapshot(target, _build_repository_preparation(target))
        finished_at = datetime.now(UTC).isoformat()
        _setup_tasks[task_id]["status"] = "complete"
        _setup_tasks[task_id]["skipped"] = False
        _setup_tasks[task_id]["preparation"] = snapshot
        _setup_tasks[task_id]["finishedAt"] = finished_at
        _append_setup_task_log(task_id, f"Setup complete; preparation snapshot: {snapshot['id']}")
        _persist_setup_task_state(
            task_id,
            status="complete",
            request=request_payload,
            started_at=started_at,
            finished_at=finished_at,
            skipped=False,
            output=combined_output,
            command=cmd,
            return_code=proc.returncode,
            preparation=snapshot,
        )
    except subprocess.TimeoutExpired:
        finished_at = datetime.now(UTC).isoformat()
        _setup_tasks[task_id]["status"] = "error"
        _setup_tasks[task_id]["error"] = "Playground setup timed out after 1800s"
        _setup_tasks[task_id]["finishedAt"] = finished_at
        _append_setup_task_log(task_id, "Setup timed out after 1800s")
        _persist_setup_task_state(
            task_id,
            status="error",
            request=request_payload,
            started_at=started_at,
            finished_at=finished_at,
            error="Playground setup timed out after 1800s",
        )
    except Exception as e:
        finished_at = datetime.now(UTC).isoformat()
        _setup_tasks[task_id]["status"] = "error"
        _setup_tasks[task_id]["error"] = str(e)
        _setup_tasks[task_id]["finishedAt"] = finished_at
        _append_setup_task_log(task_id, f"Setup raised exception: {e}")
        _persist_setup_task_state(
            task_id,
            status="error",
            request=request_payload,
            started_at=started_at,
            finished_at=finished_at,
            error=str(e),
        )

@app.post("/api/repositories/index")
async def index_repository(req: IndexRequest, background_tasks: BackgroundTasks) -> dict[str, Any]:
    if not ENGINE_BINARY.exists():
        raise HTTPException(status_code=500, detail=f"Engine binary not found at {ENGINE_BINARY}")

    task_id = f"index-{req.target}"
    _index_tasks[task_id] = {"status": "queued", "target": req.target}
    background_tasks.add_task(_run_index, req.target, task_id)
    return {"success": True, "taskId": task_id}


@app.post("/api/repositories/setup")
async def setup_repository(req: SetupRequest, background_tasks: BackgroundTasks) -> dict[str, Any]:
    _ensure_aethyme_imports()
    from src.eval.targets import get_target

    eval_target = get_target(req.target)
    source = req.source or eval_target.setup_source
    commit = req.commit or eval_target.setup_commit
    if not source or not commit:
        raise HTTPException(
            status_code=400,
            detail="Setup requires source and commit for this target",
        )

    task_id = f"setup-{req.target}-{datetime.now(UTC).strftime('%Y%m%dT%H%M%S')}"
    _ensure_aethyme_imports()
    from src.eval.observability import initialize_setup_task_artifacts, setup_task_dir

    request_payload = _setup_task_request_payload(
        target=req.target,
        source=source,
        commit=commit,
        force=req.force,
    )
    task_dir = initialize_setup_task_artifacts(task_id, request_payload)
    _setup_tasks[task_id] = {
        "status": "queued",
        "target": req.target,
        "source": source,
        "commit": commit,
        "force": req.force,
        "path": str(task_dir),
    }
    _append_setup_task_log(task_id, "Setup task queued")
    _persist_setup_task_state(
        task_id,
        status="queued",
        request=request_payload,
    )
    background_tasks.add_task(_run_setup_target, req.target, source, commit, req.force, task_id)
    return {"success": True, "taskId": task_id, "path": str(setup_task_dir(task_id))}


@app.get("/api/repositories/index/status/{task_id}")
async def index_status(task_id: str) -> dict[str, Any]:
    if task_id not in _index_tasks:
        raise HTTPException(status_code=404, detail="Task not found")
    return _index_tasks[task_id]


@app.get("/api/repositories/setup/status/{task_id}")
async def setup_status(task_id: str) -> dict[str, Any]:
    if task_id not in _setup_tasks:
        persisted = _read_setup_task_state(task_id)
        if persisted is None:
            raise HTTPException(status_code=404, detail="Task not found")
        return persisted
    return _setup_tasks[task_id]


# ---------------------------------------------------------------------------
# Plan / Run
# ---------------------------------------------------------------------------

class PlanRequest(BaseModel):
    evalType: str
    target: str
    model: str
    reasoning: str = "high"
    windowId: int | None = None
    cleanupDelaySeconds: int = Field(default=1, ge=0, le=3600)

@app.post("/api/plan")
async def generate_plan(req: PlanRequest) -> dict[str, Any]:
    plan, _ = await _generate_plan(req)
    return plan


# ---------------------------------------------------------------------------
# Chau7 MCP — direct tab control
# ---------------------------------------------------------------------------

@app.get("/api/chau7/status")
async def chau7_status() -> dict[str, Any]:
    import mcp_client
    return {"available": mcp_client.is_available()}


@app.get("/api/chau7/tabs")
async def chau7_tabs() -> list[dict[str, Any]]:
    import mcp_client
    if not mcp_client.is_available():
        raise HTTPException(status_code=503, detail="Chau7 MCP not available")
    return mcp_client.tab_list()


class TabCreateRequest(BaseModel):
    directory: str
    windowId: int | None = None

@app.post("/api/chau7/tabs/create")
async def chau7_tab_create(req: TabCreateRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_create(req.directory, req.windowId)


class TabActionRequest(BaseModel):
    tabId: str

@app.post("/api/chau7/tabs/close")
async def chau7_tab_close(req: TabActionRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_close(req.tabId)


@app.post("/api/chau7/tabs/status")
async def chau7_tab_status(req: TabActionRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_status(req.tabId)


class TabExecRequest(BaseModel):
    tabId: str
    command: str

@app.post("/api/chau7/tabs/exec")
async def chau7_tab_exec(req: TabExecRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_exec(req.tabId, req.command)


class TabCtoRequest(BaseModel):
    tabId: str
    override: str

@app.post("/api/chau7/tabs/cto")
async def chau7_tab_cto(req: TabCtoRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_set_cto(req.tabId, req.override)


class TabOutputRequest(BaseModel):
    tabId: str
    lines: int = 50

@app.post("/api/chau7/tabs/output")
async def chau7_tab_output(req: TabOutputRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_output(req.tabId, req.lines)


# ---------------------------------------------------------------------------
# Run — generates plan and launches via Chau7
# ---------------------------------------------------------------------------

class RunRequest(BaseModel):
    evalType: str
    target: str
    model: str
    reasoning: str = "high"
    windowId: int | None = None
    preparationId: str | None = None
    cleanupDelaySeconds: int = Field(default=1, ge=0, le=3600)
    # P1: number of sequential eval repetitions. Default 3 — protocol requires
    # N>=3 for any comparison across conditions so single-run variance can be
    # distinguished from real effects. runs=1 is still allowed for debugging.
    # All repetitions in a call share a batch_id so the UI can aggregate to
    # median + IQR.
    runs: int = Field(default=3, ge=1, le=20)
    # P3: turn on LLM-as-judge scoring (in addition to keyword score).
    # Judge runs codex via a Chau7 tab (same path as eval agents — no direct API).
    # Intra-rater reliability: judge is called judgeSamples times on the same
    # (task, reference, candidate) triple; we report mean + stdev.
    useJudge: bool = True
    judgeSamples: int = Field(default=3, ge=1, le=10)
    # P8: pre-registration. Declare up front which metric is the primary
    # outcome and how large a delta counts as meaningful. Everything else is
    # marked "exploratory" in the report. This blocks post-hoc cherry-picking
    # ("the one metric where leverage wins").
    # primaryMetric: "quality" | "global_score" | "judge" | "cost" | "time"
    #   — quality (default) is the task-specific scorer output,
    #   — global_score is the control-anchored composite,
    #   — judge is the LLM-judge mean,
    #   — cost/time compare efficiency only.
    primaryMetric: str = "quality"
    # minimumMeaningfulDelta: in the primary metric's units. For quality
    # (0-100) a 5-point delta is a reasonable floor. Serves as the margin
    # in pairwise comparison verdicts (see batch_stats.compare_conditions).
    minimumMeaningfulDelta: float = Field(default=5.0, ge=0.0)

_run_state: dict[str, Any] = {
    "status": "idle",
    "plan": None,
    "currentPhase": None,
    "log": [],
    "tabs": {},
    "error": None,
}


def _claude_projects_root() -> Path:
    return Path.home() / ".claude" / "projects"


def _encode_claude_project_path(repo_path: Path) -> str:
    resolved = str(repo_path.resolve())
    return resolved.replace("/", "-").replace(" ", "-")


def _session_dir_for_repo(repo_path: Path) -> Path:
    return _claude_projects_root() / _encode_claude_project_path(repo_path)


def _session_file_for_repo(repo_path: Path, session_id: str) -> Path:
    return _session_dir_for_repo(repo_path) / f"{session_id}.jsonl"


def _first_user_message_text(session_file: Path) -> str:
    try:
        with open(session_file, encoding="utf-8") as f:
            for line in f:
                try:
                    msg = json.loads(line)
                except Exception:
                    continue
                if msg.get("type") != "user":
                    continue
                content = msg.get("message", {}).get("content", "")
                if isinstance(content, list):
                    return "\n".join(
                        block.get("text", "")
                        for block in content
                        if isinstance(block, dict)
                    ).strip()
                return str(content).strip()
    except OSError:
        return ""
    return ""


def _ensure_aethyme_imports() -> None:
    import sys

    root = str(AETHYME_PKG)
    if root not in sys.path:
        sys.path.insert(0, root)


def _read_session_events(session_file: Path) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    try:
        with open(session_file, encoding="utf-8") as f:
            for line in f:
                try:
                    msg = json.loads(line)
                except Exception:
                    continue
                if isinstance(msg, dict):
                    events.append(msg)
    except OSError:
        return []

    sub_dir = session_file.parent / session_file.stem / "subagents"
    if sub_dir.exists():
        for sub_file in sorted(sub_dir.glob("*.jsonl")):
            events.extend(_read_session_events(sub_file))

    events.sort(key=lambda item: item.get("timestamp", ""))
    return events


def _extract_tool_calls(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    tool_calls: list[dict[str, Any]] = []
    for msg in events:
        if msg.get("type") != "assistant":
            continue
        content = msg.get("message", {}).get("content", [])
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            tool_calls.append(
                {
                    "timestamp": msg.get("timestamp"),
                    "name": block.get("name"),
                    "id": block.get("id"),
                    "input": block.get("input"),
                }
            )
    return tool_calls


def _dead_code_task_for_target(target: str) -> str:
    if target == "aethyme":
        return """\
Find all public top-level functions in `packages/aethyme/src/indexing/` that are never called from outside that directory.

Scope:
- Check every Python file in `packages/aethyme/src/indexing/` for public top-level function definitions
- For each public function, search the entire repo outside `packages/aethyme/src/indexing/` for call sites
- Search at least `packages/aethyme/src/`, `packages/aethyme/tests/`, and `packages/aethyme/scripts/`
- Exclude private helpers whose names start with `_`

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public top-level function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you."""

    return """\
Find all public methods in `includes/Watchlist/` that are never called from outside that directory.

Scope:
- Check every PHP file in `includes/Watchlist/` for public function definitions
- For each public function, search the ENTIRE codebase (outside `includes/Watchlist/`) for call sites
- Exclude test files (`tests/`) and vendor files (`vendor/`) from the caller search
- Exclude constructors (`__construct`, `__destruct`)

For each unused function, report:
- The function name
- The file it's defined in (relative path)
- Why you believe it's unused (what you searched for and didn't find)

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you."""


def _shared_eval_artifacts(eval_type: str, task: str, *, target: str) -> dict[str, Any]:
    _ensure_aethyme_imports()
    from src.eval import schemas as eval_schemas
    from src.eval.report import contract_versions

    artifacts: dict[str, Any] = {
        "task_spec": {
            "eval_type": eval_type,
            "task": task,
        },
        "contract_versions": contract_versions(),
        "pack": {
            "status": "not_generated_in_eval_ui_server",
            "eval_type": eval_type,
            "task": task,
        },
        "reference_output": {
            "status": "not_generated_in_eval_ui_server",
            "eval_type": eval_type,
        },
        "scoring_rubric": {
            "method": "server_keyword_heuristic",
            "eval_type": eval_type,
            "status": "placeholder",
        },
    }

    mapping: dict[str, tuple[str | None, str | None, str | None]] = {
        "explain-repo": ("explain_repo_output_schema", "explain_repo_scoring_rubric", None),
        "navigation-ctf": ("navigation_ctf_output_schema", "navigation_ctf_scoring_rubric", None),
        "bug-fix": ("bug_fix_output_schema", "bug_fix_scoring_rubric", None),
        "bug-fix-1": ("mediawiki_bug_fix_1_output_schema", "mediawiki_bug_fix_1_scoring_rubric", "mediawiki_bug_fix_1_reference"),
        "impact-analysis": ("mediawiki_impact_analysis_output_schema", None, "mediawiki_impact_analysis_reference"),
        "feature-localization": ("mediawiki_feature_localization_output_schema", None, "mediawiki_feature_localization_reference"),
        "config-audit": ("mediawiki_config_audit_output_schema", None, "mediawiki_config_audit_reference"),
        "migration": ("mediawiki_migration_output_schema", "mediawiki_migration_scoring_rubric", "mediawiki_migration_reference"),
    }

    if eval_type == "dead-code":
        schema_name = "dead_code_output_schema_for_target"
        rubric_name = "dead_code_scoring_rubric_for_target"
        reference_name = "dead_code_reference_for_target"
    else:
        schema_name, rubric_name, reference_name = mapping.get(eval_type, (None, None, None))
    if schema_name and hasattr(eval_schemas, schema_name):
        schema_fn = getattr(eval_schemas, schema_name)
        artifacts["output_schema"] = schema_fn(target) if eval_type == "dead-code" else schema_fn()
    if rubric_name and hasattr(eval_schemas, rubric_name):
        rubric_fn = getattr(eval_schemas, rubric_name)
        artifacts["scoring_rubric"] = rubric_fn(target) if eval_type == "dead-code" else rubric_fn()
    if reference_name and hasattr(eval_schemas, reference_name):
        ref_fn = getattr(eval_schemas, reference_name)
        reference = ref_fn(target) if eval_type == "dead-code" else ref_fn()
        artifacts["reference_output"] = reference
        if eval_type in {"navigation-ctf", "bug-fix"}:
            artifacts["reference"] = reference
        if eval_type == "navigation-ctf":
            artifacts["challenge"] = {**artifacts["task_spec"], "reference_output": reference}
    return artifacts


def _condition_run_metadata(
    *,
    repo_path: Path,
    run_id: str,
    condition: str,
    req: Any,
    session_id: str | None,
    started_at: str | None,
    finished_at: str | None,
    status: str,
) -> dict[str, Any]:
    _ensure_aethyme_imports()
    from src.contracts.versions import RUN_METADATA_SCHEMA_VERSION, contract_versions
    from src.indexing.repository_snapshot import capture_snapshot

    snapshot = capture_snapshot(repo_path)
    config_payload = {
        "eval_type": req.evalType,
        "target": req.target,
        "model": req.model,
        "reasoning": req.reasoning,
        "condition": condition,
        "window_id": req.windowId,
    }
    config_hash = hashlib.sha256(
        json.dumps(config_payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()

    metadata: dict[str, Any] = {
        "schema_version": RUN_METADATA_SCHEMA_VERSION,
        "contract_versions": contract_versions(),
        "run_id": run_id,
        "phase": f"eval:{condition}",
        "status": status,
        "eval_type": req.evalType,
        "target": req.target,
        "repo_path": str(repo_path),
        "repo_commit": snapshot.commit,
        "repo_dirty": snapshot.dirty,
        "repo_snapshot_key": snapshot.cache_key,
        "repo_fingerprint": snapshot.fingerprint,
        "config_hash": config_hash,
        "model": req.model,
        "reasoning": req.reasoning,
    }
    if session_id:
        metadata["session_id"] = session_id
    if started_at:
        metadata["started_at"] = started_at
    if finished_at:
        metadata["finished_at"] = finished_at
    return metadata


def _assessment_from_score(eval_type: str, score: float) -> dict[str, Any]:
    return {
        "weighted_score": round(score, 1),
        "max_score": 100,
        "scores": {"overall": round(score / 100, 3)},
        "weights": {"overall": 100},
        "method": "server_keyword_heuristic",
        "eval_type": eval_type,
    }


def _parse_and_score_bug_fix_1_output(
    output_text: str,
    *,
    cost: float,
    repo_path: Path,
) -> tuple[dict[str, Any] | None, dict[str, Any]]:
    import sys

    sys.path.insert(0, str(AETHYME_PKG))
    from src.eval.scoring import parse_structured_output, score_mediawiki_bug_fix_1
    from src.eval.schemas import mediawiki_bug_fix_1_reference

    candidate = parse_structured_output(output_text)
    if candidate is None:
        return None, {
            "weighted_score": 0.0,
            "max_score": 100,
            "scores": {
                "files_identified": 0.0,
                "root_cause_quality": 0.0,
                "fix_plan_quality": 0.0,
                "testing_quality": 0.0,
                "efficiency": 0.0,
            },
            "weights": {
                "files_identified": 35,
                "root_cause_quality": 25,
                "fix_plan_quality": 15,
                "testing_quality": 15,
                "efficiency": 10,
            },
            "method": "structured_parse_failed",
            "eval_type": "bug-fix-1",
            "error": "output_not_valid_json_object",
        }

    reference = mediawiki_bug_fix_1_reference()
    assessment = score_mediawiki_bug_fix_1(
        candidate,
        reference,
        cost_usd=cost,
        repo_path=str(repo_path.resolve()),
    )
    assessment["method"] = "score_mediawiki_bug_fix_1"
    assessment["eval_type"] = "bug-fix-1"
    return candidate, assessment


def _parse_and_score_dead_code_output(
    output_text: str,
    *,
    cost: float,
    target: str,
    repo_path: Path,
) -> tuple[dict[str, Any] | None, dict[str, Any]]:
    import sys

    sys.path.insert(0, str(AETHYME_PKG))
    from src.eval.scoring import parse_structured_output, score_dead_code
    from src.eval.schemas import dead_code_reference_for_target

    candidate = parse_structured_output(output_text)
    if candidate is None:
        return None, {
            "weighted_score": 0.0,
            "max_score": 100,
            "scores": {
                "functions_found": 0.0,
                "false_positives": 0.0,
                "efficiency": 0.0,
            },
            "weights": {
                "functions_found": 60,
                "false_positives": 20,
                "efficiency": 20,
            },
            "method": "structured_parse_failed",
            "eval_type": "dead-code",
            "target": target,
            "error": "output_not_valid_json_object",
        }

    reference = dead_code_reference_for_target(target)
    assessment = score_dead_code(
        candidate,
        reference,
        cost_usd=cost,
        repo_path=str(repo_path.resolve()),
    )
    assessment["method"] = "score_dead_code"
    assessment["eval_type"] = "dead-code"
    assessment["target"] = target
    return candidate, assessment


def _missing_deliverable_assessment(
    eval_type: str,
    *,
    expected_output_file: str | None,
    output_snapshot: dict[str, Any],
    fallback_chars: int,
    last_tab_status: dict[str, Any] | None,
) -> dict[str, Any]:
    return {
        "weighted_score": 0.0,
        "max_score": 100,
        "scores": {"overall": 0.0},
        "weights": {"overall": 100},
        "method": "missing_deliverable",
        "eval_type": eval_type,
        "error": "expected_output_file_missing_or_incomplete",
        "expected_output_file": expected_output_file,
        "output_snapshot": output_snapshot,
        "fallback_output_chars": fallback_chars,
        "last_tab_status": last_tab_status or {},
    }


def _append_run_log(run_dir: Path | None, msg: str) -> None:
    if run_dir is None:
        return
    try:
        with open(run_dir / "events.log", "a", encoding="utf-8") as f:
            f.write(f"{datetime.now(UTC).isoformat()} {msg}\n")
    except OSError:
        pass


def _file_observability(path: Path) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "path": str(path),
        "exists": path.exists(),
    }
    if not path.exists():
        return payload
    try:
        content = path.read_text(encoding="utf-8")
    except OSError:
        return payload
    payload["size_bytes"] = len(content.encode("utf-8"))
    payload["sha256"] = hashlib.sha256(content.encode("utf-8")).hexdigest()
    payload["chars"] = len(content)
    return payload


def _materialize_eval_inputs(
    plan: dict[str, Any],
    req: Any,
    conditions: list[dict[str, Any]],
    *,
    log: Any,
) -> dict[str, Any]:
    started_at = datetime.now(UTC).isoformat()
    prompt_files = plan["paths"]["prompt_files"]
    eval_type = req.evalType

    eval_tasks = {
        "explain-repo": """\
Explain this repository.

Produce your analysis in exactly these sections:

1. **Summary** — One paragraph: what this is, primary language, scale, purpose.
2. **Entry Points** — List each runtime entry file with its path and one-line purpose.
3. **Bootstrap Chain** — The initialization sequence from entry point to running application.
4. **Key Subsystems** — Top 5-8 subsystems, each with: directory, responsibility, and one representative file with a brief explanation of what it does.
5. **Testing Strategy** — Frameworks used, directory structure, how to run tests.
6. **Frontend Approach** — Bundling, frameworks, module system, how assets are served.
7. **Extension Model** — How plugins/extensions/skins integrate with the core.

Keep each section concise. Cite file paths and line numbers where relevant.
Do not exhaustively list directories — focus on architectural understanding.""",
        "bug-fix-1": """\
Bug report (T419918): Viewing a diff/revision on a watchlisted page marks all revisions as 'seen' instead of only the one viewed.

Identify which files need editing and explain how you would fix this bug. Do NOT apply the fix — only report your analysis.

Write exactly one JSON object with this shape:
{
  "files_to_edit": [
    {
      "path": "relative/path.php",
      "what_to_change": "specific change needed in this file"
    }
  ],
  "root_cause": "trace from viewing a diff/revision to the wrong watchlist behavior",
  "fix_plan": "step-by-step fix plan including signature and deprecation strategy",
  "testing": "how to verify the fix and what regressions to check"
}

Rules:
- Output JSON only, no markdown fences and no prose before or after the JSON.
- Use repo-relative file paths.
- Include the release notes file if the deprecation should be documented.
- In `testing`, explicitly cover diff/revision behavior and watchlist notification regression checks.""",
        "impact-analysis": """\
WikiPage::doViewUpdates() in includes/Page/WikiPage.php is being refactored to accept different parameters.

List every file that calls this method and would need updating. For each call site, provide:
- File path
- Line number
- The exact code at that line

Be thorough — check all of includes/ for direct calls, indirect calls via subclasses, and references in comments/docs.""",
        "feature-localization": """\
When a user clicks "Watch" on a wiki page, what code runs?

Trace the full execution chain from the HTTP request handler to the database write. List each class and method in the chain, in execution order. For each step, provide:
- File path
- Class::method name
- One-line description of what it does and what it calls next

Start from the Action handler and end at the database write.""",
        "config-audit": """\
MediaWiki has rate limiting for API requests. Find:

(a) The configuration variable that controls rate limits — give the exact variable name
(b) Where the default value is defined — give the exact file path and line number
(c) The class that enforces rate limiting at runtime — give the file path and class name
(d) How a site admin disables rate limiting for a specific action — explain the configuration change needed

Cite file paths and line numbers for every answer.""",
        "bug-fix": """\
A test is failing in this repository. Find the root cause and fix the bug so all tests pass.

The test file is correct. The bug is in the source code.""",
        "navigation-ctf": """\
Find the manifest that manages the main code entrypoint, identify the entrypoint file it controls, and name the top-level area that owns both.

Produce a structured analysis with config_target, code_target, management_area, and relationship_chain.""",
        "migration": """\
We are renaming the class `WatchedItemStore` to `WatchlistNotificationStore`.

List every PHP file outside of tests/ and vendor/ that references `WatchedItemStore` and would need updating. For each file, provide:
- The file path (relative to repo root)
- What change is needed (e.g., "update class reference", "update type hint", "update service wiring")

Exclude `includes/Watchlist/WatchedItemStore.php` itself — that's the file being renamed, not a reference to it.

Be exhaustive — missing a file means the rename breaks production. Search thoroughly across all of `includes/`, `maintenance/`, `docs/`, and root-level files.""",
    }

    if eval_type == "dead-code":
        bare_task = _dead_code_task_for_target(req.target)
    else:
        bare_task = eval_tasks.get(eval_type, f"Analyze this repository for: {eval_type}")
    log(f"Eval type: {eval_type}")
    log(f"Bare task: {len(bare_task)} chars")

    shared_artifacts = _shared_eval_artifacts(eval_type, bare_task, target=req.target)
    shared_artifacts["target"] = req.target
    shared_artifacts["model"] = plan.get("meta", {}).get("model")
    shared_artifacts["scenario"] = plan.get("meta", {}).get("scenario")
    shared_artifacts["signals"] = {}

    output_files: dict[str, Path] = {}
    for cond in conditions:
        cond_name = cond["name"]
        repo_dir = Path(cond["directory"])
        output_suffix = "json" if eval_type in {"bug-fix-1", "dead-code"} else "md"
        output_path = repo_dir / f".aethyme-eval-output-{cond_name}.{output_suffix}"
        output_files[cond_name] = output_path
        output_path.unlink(missing_ok=True)

    for prompt_path in prompt_files.values():
        Path(prompt_path).unlink(missing_ok=True)

    enriched_prompt = bare_task
    enrichment_meta: dict[str, Any] | None = None
    enrichment_observability: dict[str, Any] = {
        "status": "not_attempted",
        "mode": "bare_prompt",
        "engine_binary": str(ENGINE_BINARY),
    }
    try:
        aethyme_dir = next(
            (
                c["directory"]
                for c in conditions
                if c["name"] in {"task-conditioned", "leverage"}
            ),
            "",
        )
        import re as _re
        subsystem_match = _re.search(r'`([a-zA-Z0-9_/.-]+/)`', bare_task)
        subsystem = subsystem_match.group(1) if subsystem_match else None

        if aethyme_dir and ENGINE_BINARY.exists():
            cmd = [str(ENGINE_BINARY), "prompt", "--repo", aethyme_dir, "--task", bare_task, "--focus", "overview"]
            if subsystem:
                cmd.extend(["--subsystem", subsystem])
                log(f"Generating enriched prompt with subsystem context: {subsystem}")
            else:
                log("Generating enriched prompt from engine...")
            enrichment_started = datetime.now(UTC).isoformat()
            enriched_proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
            enrichment_finished = datetime.now(UTC).isoformat()
            enrichment_observability = {
                "status": "success" if enriched_proc.returncode == 0 and bool(enriched_proc.stdout.strip()) else "degraded",
                "mode": "engine_prompt",
                "command": cmd,
                "engine_binary": str(ENGINE_BINARY),
                "repo_path": aethyme_dir,
                "task": bare_task,
                "focus": "overview",
                "subsystem": subsystem,
                "started_at": enrichment_started,
                "finished_at": enrichment_finished,
                "return_code": enriched_proc.returncode,
                "stdout_chars": len(enriched_proc.stdout or ""),
                "stderr_chars": len(enriched_proc.stderr or ""),
                "stdout": enriched_proc.stdout,
                "stderr": enriched_proc.stderr,
            }
            if enriched_proc.returncode == 0 and enriched_proc.stdout.strip():
                enriched_prompt = enriched_proc.stdout
                log(f"Enriched prompt generated: {len(enriched_prompt)} chars")
                enrichment_meta = {
                    "mode": "engine_prompt",
                    "repo_path": aethyme_dir,
                    "task": bare_task,
                    "focus": "overview",
                    "subsystem": subsystem,
                    "engine_binary": str(ENGINE_BINARY),
                }
            else:
                log(
                    "WARNING: Engine failed, using bare prompt for task-conditioned input. "
                    f"stderr: {enriched_proc.stderr[:200]}"
                )
        else:
            log("No engine available, using bare prompt for task-conditioned input")
            enrichment_observability = {
                "status": "degraded",
                "mode": "bare_prompt",
                "engine_binary": str(ENGINE_BINARY),
                "repo_path": aethyme_dir,
                "reason": "engine_unavailable_or_missing_task_conditioned_repo",
            }
    except Exception as e:
        log(f"ERROR writing prompts: {e}")
        enrichment_observability = {
            "status": "failed",
            "mode": "bare_prompt",
            "engine_binary": str(ENGINE_BINARY),
            "error": str(e),
        }

    for cond_name, prompt_path in prompt_files.items():
        if cond_name == "task-conditioned":
            task_text = enriched_prompt
        elif cond_name == "leverage":
            task_text = (
                "Use Aethyme tools to navigate the repository graph. "
                "Use them proactively, but do your own analysis.\n\n"
                f"{bare_task}"
            )
        else:
            task_text = bare_task
        out_path = str(output_files[cond_name])
        if eval_type == "bug-fix-1":
            prompt_text = (
                f"IMPORTANT: You MUST save exactly one JSON object to `{out_path}` when done. "
                f"Use the Write tool to create this file. The file contents must be valid JSON and must match the required keys exactly.\n\n"
                f"{task_text}\n\n"
                f"Remember: save JSON only to `{out_path}`."
            )
        elif eval_type == "dead-code":
            prompt_text = (
                f"IMPORTANT: You MUST save exactly one JSON object to `{out_path}` when done. "
                f"Use the Write tool to create this file. The file contents must be valid JSON with exactly this shape:\n"
                "{\n"
                '  "unused_functions": [\n'
                "    {\n"
                '      "function_name": "name",\n'
                '      "defined_in": "relative/path.py",\n'
                '      "reason": "what you searched for and did not find"\n'
                "    }\n"
                "  ]\n"
                "}\n\n"
                f"{task_text}\n\n"
                f"Remember: save JSON only to `{out_path}`."
            )
        else:
            prompt_text = (
                f"IMPORTANT: You MUST save your complete analysis to `{out_path}` when done. "
                f"Use the Write tool to create this file with your full response.\n\n"
                f"{task_text}\n\n"
                f"Remember: save your complete analysis to `{out_path}`."
            )
        Path(prompt_path).write_text(prompt_text, encoding="utf-8")
        log(f"Wrote {cond_name}: {prompt_path} ({len(prompt_text)} chars)")

    missing = [f for f in prompt_files.values() if not Path(f).exists()]
    if missing:
        log(f"WARNING: Missing prompt files: {missing}")
    else:
        log(f"All {len(prompt_files)} prompt files ready")

    shared_artifacts["navigation_context"] = enrichment_meta or {
        "mode": "bare_prompt",
        "task": bare_task,
        "status": "engine_enrichment_unavailable",
    }
    finished_at = datetime.now(UTC).isoformat()
    return {
        "bare_task": bare_task,
        "shared_artifacts": shared_artifacts,
        "prompt_files": prompt_files,
        "output_files": output_files,
        "observability": {
            "status": "success" if not missing else "degraded",
            "started_at": started_at,
            "finished_at": finished_at,
            "eval_type": eval_type,
            "target": req.target,
            "bare_task_chars": len(bare_task),
            "enrichment": enrichment_observability,
            "prompt_files": {
                cond_name: _file_observability(Path(prompt_path))
                for cond_name, prompt_path in prompt_files.items()
            },
            "output_files": {
                cond_name: {
                    "path": str(output_files[cond_name]),
                    "exists": output_files[cond_name].exists(),
                }
                for cond_name in output_files
            },
        },
    }

def _clean_pty_output(raw: str) -> str:
    """Clean terminal rendering noise from PTY log output.

    The PTY log captures TUI frame redraws interleaved with actual content.
    Remove prompt redraws, separator lines, and spinner frames to extract
    the agent's actual tool calls and text responses.
    """
    import re
    lines = raw.split('\n')
    cleaned = []
    for line in lines:
        # Skip separator lines (just ─ characters)
        if re.match(r'^[─━═]+$', line.strip()):
            continue
        # Skip prompt redraws
        if line.strip() in ('❯', '❯ '):
            continue
        # Skip bypass permissions indicator
        if 'bypasspermissionson' in line.replace(' ', ''):
            continue
        if 'bypass permissions on' in line:
            continue
        # Skip spinner frames (just spinner chars + thinking text)
        stripped = line.strip()
        if stripped and len(stripped) < 3 and stripped[0] in '✻✶✳✽✢·⏺':
            continue
        # Skip empty lines that are just whitespace
        if not stripped:
            # Keep one empty line but not many in a row
            if cleaned and cleaned[-1] == '':
                continue
        cleaned.append(line)
    return '\n'.join(cleaned)


def _score_output(eval_type: str, output: str, cost: float, prompt: str = "") -> float:
    """Score agent output against reference. Works with free-form text.

    The prompt text is stripped from the output before scoring to avoid
    inflating scores when the prompt itself contains reference keywords
    (e.g., the task-conditioned prompt with subsystem context).
    """
    if not output or len(output) < 50:
        return 0.0

    # Strip prompt text from output to avoid scoring prompt keywords
    if prompt:
        import re as _re2
        prompt_collapsed = _re2.sub(r'\s+', '', prompt.lower())
        # Remove any substring of output that matches collapsed prompt words
        # Simple approach: remove all prompt words from the output
        for word in set(prompt.split()):
            if len(word) > 5:  # Only strip significant words
                output = output.replace(word, '')

    if eval_type == "bug-fix-1":
        try:
            import sys
            sys.path.insert(0, str(AETHYME_PKG))
            from src.eval.schemas import mediawiki_bug_fix_1_reference
            ref = mediawiki_bug_fix_1_reference()
        except Exception:
            return 0.0

        # PTY log has TUI noise splitting keywords — collapse whitespace for matching
        import re
        text_collapsed = re.sub(r'\s+', ' ', output).lower()

        # Files identified (40%) — check if output mentions the reference files
        ref_files = [f["path"] for f in ref.get("files_to_edit", [])]
        files_found = sum(1 for f in ref_files if f.lower() in text_collapsed)
        files_score = files_found / len(ref_files) if ref_files else 0

        # Root cause (30%) — keyword presence
        rc_kws = ref.get("root_cause_keywords", [])
        rc_found = sum(1 for kw in rc_kws if kw.lower() in text_collapsed)
        rc_score = rc_found / len(rc_kws) if rc_kws else 0

        # Fix plan (20%) — keyword presence
        fp_kws = ref.get("fix_plan_keywords", [])
        fp_found = sum(1 for kw in fp_kws if kw.lower() in text_collapsed)
        fp_score = fp_found / len(fp_kws) if fp_kws else 0

        # Efficiency (10%) — cost relative to $0.50 baseline
        if cost <= 0:
            eff_score = 0.5
        elif cost <= 0.5:
            eff_score = 1.0
        elif cost <= 2.0:
            eff_score = max(0, 1.0 - (cost - 0.5) / 3.0)
        else:
            eff_score = max(0, 0.5 - (cost - 2.0) / 10.0)

        weighted = (files_score * 40 + rc_score * 30 + fp_score * 20 + eff_score * 10)
        return round(weighted, 1)

    if eval_type in ("dead-code", "migration"):
        # Keyword-based scoring using reference keywords
        try:
            import sys
            sys.path.insert(0, str(AETHYME_PKG))
            if eval_type == "dead-code":
                from src.eval.schemas import mediawiki_dead_code_reference
                ref = mediawiki_dead_code_reference()
                kws = ref.get("function_keywords", [])
            else:
                from src.eval.schemas import mediawiki_migration_reference
                ref = mediawiki_migration_reference()
                kws = ref.get("file_keywords", [])
        except Exception:
            return 0.0

        import re
        text_collapsed = re.sub(r'\s+', ' ', output).lower()
        kw_found = sum(1 for kw in kws if kw.lower() in text_collapsed)
        kw_score = kw_found / len(kws) if kws else 0

        if cost <= 0:
            eff_score = 0.5
        elif cost <= 1.0:
            eff_score = 1.0
        elif cost <= 3.0:
            eff_score = max(0, 1.0 - (cost - 1.0) / 4.0)
        else:
            eff_score = max(0, 0.5 - (cost - 3.0) / 10.0)

        weighted = kw_score * 80 + eff_score * 20
        return round(weighted, 1)

    # Other eval types: no scoring yet
    return 0.0


def _run_eval_batch(
    first_plan: dict[str, Any],
    req: RunRequest,
    *,
    plan_snapshot: dict[str, Any] | None,
    preparation_snapshot: dict[str, Any] | None,
    first_run_dir_name: str,
    batch_id: str,
) -> None:
    """Run N sequential evals under a shared batch_id.

    Iteration 1 uses the pre-generated first_plan (so the endpoint can
    surface plan-generation failures synchronously). Iterations 2..N
    regenerate the plan from scratch — each gets its own run_dir, tabs,
    and session files.
    """
    import asyncio
    total = req.runs

    def _append_log(msg: str) -> None:
        _run_state["log"].append(msg)
        print(f"[eval-batch] {msg}")

    _append_log(f"Starting batch of {total} runs (batch_id={batch_id})")

    # Iteration 1: already have the plan
    _run_eval_background(
        first_plan,
        req,
        plan_snapshot=plan_snapshot,
        preparation_snapshot=preparation_snapshot,
        run_dir_name=first_run_dir_name,
        batch_id=batch_id,
        run_index=1,
        runs_in_batch=total,
    )

    # Iterations 2..N: regenerate plan per iteration
    for idx in range(2, total + 1):
        _append_log(f"Batch iteration {idx}/{total}: regenerating plan...")
        try:
            loop = asyncio.new_event_loop()
            plan_i, snapshot_i = loop.run_until_complete(_generate_plan(req))
            loop.close()
        except Exception as exc:
            _append_log(f"Batch iteration {idx} failed during plan generation: {exc}")
            break
        dir_name_i = Path(plan_i["paths"]["run_dir"]).name
        _run_eval_background(
            plan_i,
            req,
            plan_snapshot=snapshot_i,
            preparation_snapshot=preparation_snapshot,
            run_dir_name=dir_name_i,
            batch_id=batch_id,
            run_index=idx,
            runs_in_batch=total,
        )

    _append_log(f"Batch complete ({total} runs, batch_id={batch_id})")


def _run_eval_background(
    plan: dict[str, Any],
    req: RunRequest,
    *,
    plan_snapshot: dict[str, Any] | None = None,
    preparation_snapshot: dict[str, Any] | None = None,
    run_dir_name: str | None = None,
    batch_id: str | None = None,
    run_index: int = 1,
    runs_in_batch: int = 1,
) -> None:
    import mcp_client
    import time
    import traceback
    _ensure_aethyme_imports()
    from src.eval.observability import (
        snapshot_text_output,
        text_output_is_ready,
        write_phase_record,
        write_run_plan,
    )
    from src.eval.report import (
        create_eval_run_dir,
        finalize_eval_run,
        store_condition_chau7,
        store_condition_raw,
    )

    run_dir: Path | None = None
    phase_states: dict[str, dict[str, Any]] = {}

    def log(msg: str) -> None:
        _run_state["log"].append(msg)
        _append_run_log(run_dir, msg)
        print(f"[eval-run] {msg}")

    def start_phase(name: str, *, order: int, details: dict[str, Any] | None = None) -> None:
        started_at = datetime.now(UTC).isoformat()
        phase_states[name] = {
            "order": order,
            "started_at": started_at,
            "details": details or {},
        }
        _run_state["currentPhase"] = name
        if run_dir is not None:
            write_phase_record(
                run_dir,
                phase_name=name,
                status="running",
                started_at=started_at,
                details=details or {},
                order=order,
            )
        log(f"[phase:{name}] started")

    def finish_phase(name: str, *, status: str, details: dict[str, Any] | None = None) -> None:
        state = phase_states.get(name, {})
        merged_details = dict(state.get("details", {}))
        if details:
            merged_details.update(details)
        finished_at = datetime.now(UTC).isoformat()
        if run_dir is not None:
            write_phase_record(
                run_dir,
                phase_name=name,
                status=status,
                started_at=state.get("started_at"),
                finished_at=finished_at,
                details=merged_details,
                order=state.get("order"),
            )
        log(f"[phase:{name}] {status}")

    _run_state["status"] = "running"
    _run_state["plan"] = plan
    _run_state["log"] = []
    _run_state["error"] = None

    log(f"Starting {req.evalType} on {req.target} with {req.model}")
    log(f"Plan has {len(plan['phases'])} phases")
    if req.windowId is not None:
        log(f"Requested Chau7 window_id: {req.windowId}")
    log(f"Cleanup delay: {req.cleanupDelaySeconds}s")

    launch_phase = next((p for p in plan["phases"] if p["name"] == "launch"), None)
    if not launch_phase:
        _run_state["status"] = "error"
        _run_state["error"] = "No launch phase in plan"
        log("ERROR: No launch phase found in plan")
        return

    conditions = launch_phase.get("conditions", [])
    log(f"Backend: {launch_phase.get('backend', '?')}, Model: {launch_phase.get('model', '?')}")
    log(f"Conditions: {[c['name'] for c in conditions]}")

    # Generate run_id for this eval run — groups all conditions together
    import time as _time
    run_id = f"run-{int(_time.time())}-{req.target}-{req.evalType}"
    log(f"Run ID: {run_id}")
    _run_state["run_id"] = run_id
    run_dir = create_eval_run_dir(
        Path(conditions[0]["directory"]).parent if conditions else Path(req.target),
        req.evalType,
        tuple(plan.get("meta", {}).get("conditions", [])),
        model=plan.get("meta", {}).get("model"),
        run_name=run_dir_name,
    )
    _run_state["run_dir"] = str(run_dir)
    log(f"Run dir: {run_dir}")
    if preparation_snapshot is not None:
        log(f"Preparation: {preparation_snapshot.get('id')} ready={preparation_snapshot.get('ready')}")
    try:
        metadata_path = run_dir / "metadata.json"
        metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata["plan_run_dir"] = plan.get("paths", {}).get("run_dir")
        if plan_snapshot is not None:
            metadata["plan"] = {
                "id": plan_snapshot.get("id"),
                "path": plan_snapshot.get("path"),
            }
        if preparation_snapshot is not None:
            metadata["preparation"] = {
                "id": preparation_snapshot.get("id"),
                "ready": preparation_snapshot.get("ready"),
                "path": preparation_snapshot.get("path"),
            }
        metadata_path.write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    except Exception:
        pass
    write_run_plan(
        run_dir,
        plan=plan,
        request=_plan_request_payload(req),
        plan_snapshot=plan_snapshot,
    )

    # Artifact build is separate from repository preparation and from tab execution.
    start_phase("build inputs", order=1, details={"prompt_files": list(plan.get("paths", {}).get("prompt_files", {}).keys())})
    prepared_inputs = _materialize_eval_inputs(plan, req, conditions, log=log)
    prompt_files = prepared_inputs["prompt_files"]
    output_files = prepared_inputs["output_files"]
    shared_artifacts = prepared_inputs["shared_artifacts"]
    shared_artifacts["run_id"] = run_id
    shared_artifacts["preparation"] = preparation_snapshot
    bare_task = prepared_inputs["bare_task"]
    condition_payloads: dict[str, dict[str, Any]] = {}
    finish_phase("build inputs", status=prepared_inputs["observability"]["status"], details=prepared_inputs["observability"])

    tabs: dict[str, str] = {}

    # Phase: Clean up stale MCP tabs
    start_phase("cleanup stale tabs", order=2)
    stale_tab_count = 0
    try:
        existing_tabs = mcp_client.tab_list()
        mcp_tabs = [t for t in existing_tabs if t.get("is_mcp_controlled")]
        stale_tab_count = len(mcp_tabs)
        if mcp_tabs:
            log(f"Closing {len(mcp_tabs)} stale MCP tabs...")
            for t in mcp_tabs:
                try:
                    mcp_client.tab_close(t["tab_id"])
                except Exception:
                    pass
            time.sleep(2)
    except Exception as e:
        log(f"WARNING: Could not clean stale tabs: {e}")
        finish_phase("cleanup stale tabs", status="degraded", details={"closed_tabs": stale_tab_count, "error": str(e)})
    else:
        finish_phase("cleanup stale tabs", status="success", details={"closed_tabs": stale_tab_count})

    # Phase: Create tabs — wait for each shell to be ready before moving on
    start_phase("creating tabs", order=3, details={"requested_conditions": [c["name"] for c in conditions]})
    for cond in conditions:
        cond_name = cond["name"]
        directory = cond["directory"]
        log(f"[{cond_name}] Creating tab in {directory}")
        try:
            result = mcp_client.tab_create(directory, req.windowId)
            if result.get("error"):
                log(f"[{cond_name}] ERROR from tab_create: {result['error']}")
                continue
            tab_id = result.get("tab_id", "")
            if not tab_id:
                log(f"[{cond_name}] ERROR: tab_create returned empty tab_id: {result}")
                continue
            tabs[cond_name] = tab_id
            log(f"[{cond_name}] Tab created: {tab_id[:12]}...")

            # Wait for shell to be ready before anything else
            shell_ready = False
            for attempt in range(15):
                time.sleep(2)
                try:
                    status = mcp_client.tab_status(tab_id)
                    if status.get("is_at_prompt") and not status.get("shell_loading"):
                        shell_ready = True
                        log(f"[{cond_name}] Shell ready after {(attempt+1)*2}s")
                        break
                except Exception:
                    pass
            if not shell_ready:
                log(f"[{cond_name}] WARNING: Shell not ready after 30s")

            if cond.get("cto_override"):
                try:
                    cto_result = mcp_client.tab_set_cto(tab_id, cond["cto_override"])
                    log(f"[{cond_name}] CTO set to {cond['cto_override']}: {cto_result}")
                except Exception as e:
                    log(f"[{cond_name}] WARNING: CTO set failed: {e}")
        except Exception as e:
            log(f"[{cond_name}] ERROR creating tab: {e}")
            log(traceback.format_exc())

    _run_state["tabs"] = tabs
    tab_status = "success" if len(tabs) == len(conditions) else "degraded"
    finish_phase(
        "creating tabs",
        status=tab_status,
        details={
            "created_tabs": {name: tab_id for name, tab_id in tabs.items()},
            "created_count": len(tabs),
            "expected_count": len(conditions),
        },
    )

    # Phase: Launch agents — with session file tracking
    start_phase(
        "launching agents",
        order=4,
        details={"backend": launch_phase.get("backend", "claude"), "model": launch_phase.get("model", "haiku")},
    )
    backend = launch_phase.get("backend", "claude")
    model_name = launch_phase.get("model", "haiku")

    session_files: dict[str, Path] = {}  # condition_name → session JSONL path
    session_ids: dict[str, str] = {}  # condition_name → explicit Claude session id

    for cond in conditions:
        cond_name = cond["name"]
        tab_id = tabs.get(cond_name)
        if not tab_id:
            log(f"[{cond_name}] SKIP: no tab_id")
            continue

        repo_dir = Path(cond["directory"])
        session_id = str(uuid.uuid4())
        session_ids[cond_name] = session_id
        expected_session_file = _session_file_for_repo(repo_dir, session_id)
        log(f"[{cond_name}] Planned Claude session_id: {session_id}")

        time.sleep(3)

        try:
            if backend == "claude":
                # Verify shell is actually ready before exec
                for retry in range(3):
                    try:
                        pre_status = mcp_client.tab_status(tab_id)
                        if pre_status.get("is_at_prompt") and pre_status.get("status") == "idle":
                            break
                    except Exception:
                        pass
                    time.sleep(3)

                cmd = (
                    f"claude --dangerously-skip-permissions --model {model_name} "
                    f"--session-id {session_id} -n aethyme-{cond_name}"
                )
                log(f"[{cond_name}] Executing: {cmd}")
                exec_result = mcp_client.tab_exec(tab_id, cmd)
                log(f"[{cond_name}] tab_exec result: {exec_result}")

                # Wait for Claude to start
                log(f"[{cond_name}] Waiting 12s for Claude to start...")
                time.sleep(12)

                # Send prompt first — session file is created when Claude receives first message
                prompt_file = cond.get("prompt_file", "")
                if prompt_file:
                    if not Path(prompt_file).exists():
                        log(f"[{cond_name}] ERROR: Prompt file missing: {prompt_file}")
                        continue

                    prompt_text = Path(prompt_file).read_text(encoding="utf-8")
                    log(f"[{cond_name}] Sending {len(prompt_text)} chars...")
                    mcp_client.tab_send_input(tab_id, prompt_text)
                    time.sleep(2)
                    mcp_client.tab_submit_prompt(tab_id)
                    log(f"[{cond_name}] Prompt submitted")
                    time.sleep(5)  # give Claude time to create session file

                # Use the explicit session_id to locate the exact JSONL file.
                for _ in range(10):
                    if expected_session_file.exists():
                        break
                    time.sleep(1)

                if expected_session_file.exists():
                    session_files[cond_name] = expected_session_file
                    log(f"[{cond_name}] Session file: {expected_session_file.name}")

                    received_prompt = _first_user_message_text(expected_session_file)
                    prompt_marker = prompt_text[:200].strip()
                    if prompt_marker and prompt_marker in received_prompt:
                        log(f"[{cond_name}] Prompt verified in session JSONL")
                    else:
                        log(f"[{cond_name}] WARNING: Prompt not verified in session JSONL")
                else:
                    log(f"[{cond_name}] WARNING: Expected session file missing: {expected_session_file}")
            else:
                prompt_file = cond.get("prompt_file", "")
                result_file = cond.get("result_file", "")
                schema_file = plan["paths"].get("schema_file", "")
                cmd = (
                    f"codex exec --sandbox workspace-write "
                    f"--output-schema {schema_file} "
                    f"--output-last-message {result_file} "
                    f"- < {prompt_file}"
                )
                log(f"[{cond_name}] Executing: {cmd[:120]}...")
                exec_result = mcp_client.tab_exec(tab_id, cmd)
                log(f"[{cond_name}] tab_exec result: {exec_result}")
        except Exception as e:
            log(f"[{cond_name}] ERROR: {e}")
            log(traceback.format_exc())

    log("All agents launched.")
    log(f"Tab mapping: {json.dumps({k: v[:12] for k, v in tabs.items()}, indent=2)}")
    finish_phase(
        "launching agents",
        status="success" if backend != "claude" or len(session_ids) == len(tabs) else "degraded",
        details={
            "session_ids": session_ids,
            "session_files": {k: str(v) for k, v in session_files.items()},
            "launched_tabs": list(tabs.keys()),
        },
    )

    # Phase: Monitor — poll until all agents complete
    start_phase("monitoring", order=5, details={"poll_interval_seconds": 10, "max_rounds": 120})
    log("Monitoring agents...")
    completed: set[str] = set()
    output_ready: dict[str, dict[str, Any]] = {}
    output_snapshots: dict[str, dict[str, Any]] = {}
    last_tab_statuses: dict[str, dict[str, Any]] = {}
    for poll_round in range(120):  # Up to 20 minutes
        time.sleep(10)
        all_done = True
        for cond_name, tab_id in tabs.items():
            if cond_name in completed:
                continue
            try:
                output_file = output_files.get(cond_name)
                snapshot = snapshot_text_output(output_file) if output_file else {"exists": False}
                output_snapshots[cond_name] = snapshot
                if text_output_is_ready(snapshot):
                    completed.add(cond_name)
                    output_ready[cond_name] = snapshot
                    log(
                        f"[{cond_name}] Completed via output file "
                        f"({snapshot.get('chars', 0)} chars, age={snapshot.get('age_seconds', 0)}s)"
                    )
                    continue

                status = mcp_client.tab_status(tab_id)
                app = status.get("active_app", "?")
                st = status.get("status", "?")
                at_prompt = status.get("is_at_prompt", False)
                is_agent = app in ("Claude", "Cline")
                raw_st = status.get("raw_status", "")
                last_tab_statuses[cond_name] = {
                    "active_app": app,
                    "status": st,
                    "raw_status": raw_st,
                    "is_at_prompt": at_prompt,
                }

                if poll_round % 3 == 0 and (snapshot.get("exists") or st in ("waitingForInput", "idle")):
                    log(
                        f"[{cond_name}] Waiting for output file: "
                        f"exists={snapshot.get('exists', False)} chars={snapshot.get('chars', 0)} "
                        f"age={snapshot.get('age_seconds', 0)}s app={app} status={st} raw={raw_st}"
                    )

                # Completion is file-first. Idle tab states are diagnostic only.
                if is_agent and (at_prompt or raw_st == "waitingForInput") and st in ("waitingForInput", "idle"):
                    all_done = False
                elif not is_agent and app != "?" and st == "idle":
                    all_done = False
                else:
                    all_done = False
            except Exception as e:
                log(f"[{cond_name}] Poll error: {e}")
                all_done = False

        if poll_round % 6 == 0:
            log(f"Poll round {poll_round}: {len(completed)}/{len(tabs)} complete")

        if all_done:
            break

    incomplete_conditions = sorted(set(tabs) - completed)
    if incomplete_conditions:
        log(f"Monitoring ended with missing deliverables: {incomplete_conditions}")
    log(f"All agents finished. Completed: {list(completed)}")
    finish_phase(
        "monitoring",
        status="success" if len(completed) == len(tabs) else "degraded",
        details={
            "completed": sorted(completed),
            "incomplete": incomplete_conditions,
            "total_tabs": len(tabs),
            "poll_rounds": poll_round + 1,
            "output_ready": output_ready,
            "last_tab_statuses": last_tab_statuses,
        },
    )

    # Phase: Collect — try output file first, fall back to PTY log
    start_phase("collecting", order=6, details={"conditions": list(tabs.keys())})
    log("Collecting results...")

    tab_outputs: dict[str, str] = {}
    deliverable_statuses: dict[str, str] = {}
    final_output_snapshots: dict[str, dict[str, Any]] = {}
    for cond_name, tab_id in tabs.items():
        output_file = output_files.get(cond_name)
        output_snapshot = snapshot_text_output(output_file) if output_file else {"exists": False}
        final_output_snapshots[cond_name] = output_snapshot

        # Try output file first
        if output_file and text_output_is_ready(output_snapshot):
            text = output_file.read_text(encoding="utf-8")
            tab_outputs[cond_name] = text
            deliverable_statuses[cond_name] = "success"
            log(f"[{cond_name}] Output file read: {len(text)} chars")
        else:
            # Fall back to PTY log + cleaning
            try:
                raw = mcp_client.tab_output(tab_id, lines=10000, source="pty_log")
                raw_text = raw.get("output", "")
                cleaned = _clean_pty_output(raw_text)
                tab_outputs[cond_name] = cleaned
                deliverable_statuses[cond_name] = "failed" if not output_snapshot.get("exists") else "degraded"
                log(
                    f"[{cond_name}] PTY log fallback: {len(raw_text)} raw → {len(cleaned)} cleaned chars "
                    f"(deliverable status: {deliverable_statuses[cond_name]})"
                )
            except Exception as e:
                tab_outputs[cond_name] = ""
                deliverable_statuses[cond_name] = "failed" if not output_snapshot.get("exists") else "degraded"
                log(f"[{cond_name}] Failed to capture output: {e}")

    log(f"Session file mapping: {json.dumps({k: v.name for k, v in session_files.items()})}")

    MODEL_PRICING = {
        "haiku": (0.80, 4.00),
        "sonnet": (3.00, 15.00),
        "opus": (15.00, 75.00),
        "gpt-5.4": (2.00, 8.00),
    }

    def count_session(path: Path) -> dict[str, Any]:
        input_tok = output_tok = cache_read = cache_create = turns = 0
        first_user_ts = last_assistant_ts = None
        tool_names: dict[str, int] = {}
        final_output = ""
        prompt_text = ""
        with open(path) as f:
            for line in f:
                try:
                    msg = json.loads(line)
                except Exception:
                    continue
                ts = msg.get("timestamp")
                if msg.get("type") == "user" and first_user_ts is None:
                    first_user_ts = ts
                    c = msg.get("message", {}).get("content", "")
                    if isinstance(c, list):
                        prompt_text = "\n".join(b.get("text", "") for b in c if isinstance(b, dict))
                    else:
                        prompt_text = str(c)
                if msg.get("type") == "assistant":
                    usage = msg.get("message", {}).get("usage", {})
                    input_tok += usage.get("input_tokens", 0)
                    output_tok += usage.get("output_tokens", 0)
                    cache_read += usage.get("cache_read_input_tokens", 0)
                    cache_create += usage.get("cache_creation_input_tokens", 0)
                    turns += 1
                    if ts:
                        last_assistant_ts = ts
                    content = msg.get("message", {}).get("content", [])
                    if isinstance(content, list):
                        for block in content:
                            if isinstance(block, dict):
                                if block.get("type") == "tool_use":
                                    name = block.get("name", "?")
                                    tool_names[name] = tool_names.get(name, 0) + 1
                                elif block.get("type") == "text" and len(block.get("text", "")) > len(final_output):
                                    final_output = block["text"]
        # Sub-agents
        sub_dir = path.parent / path.stem / "subagents"
        if sub_dir.exists():
            for sub_file in sub_dir.glob("*.jsonl"):
                sub = count_session(sub_file)
                input_tok += sub["input_tokens"]; output_tok += sub["output_tokens"]
                cache_read += sub["cache_read"]; cache_create += sub["cache_create"]
                turns += sub["turns"]
                for k, v in sub["tool_names"].items():
                    tool_names[k] = tool_names.get(k, 0) + v
                if sub.get("last_ts") and (last_assistant_ts is None or sub["last_ts"] > last_assistant_ts):
                    last_assistant_ts = sub["last_ts"]
                if len(sub.get("output", "")) > len(final_output):
                    final_output = sub["output"]

        total_tools = sum(tool_names.values())
        if "Agent" in tool_names:
            total_tools -= tool_names["Agent"]

        input_rate, output_rate = MODEL_PRICING.get(req.model, (0.80, 4.00))
        total_input = input_tok + cache_read + cache_create
        cost = (total_input * input_rate + output_tok * output_rate) / 1_000_000
        total_tok = input_tok + output_tok + cache_read + cache_create

        # Duration
        duration_seconds = None
        duration_str = "-"
        if first_user_ts and last_assistant_ts:
            try:
                from datetime import datetime as dt
                t1 = dt.fromisoformat(first_user_ts.replace("Z", "+00:00"))
                t2 = dt.fromisoformat(last_assistant_ts.replace("Z", "+00:00"))
                duration_seconds = (t2 - t1).total_seconds()
                duration_str = (
                    f"{duration_seconds:.0f}s"
                    if duration_seconds < 60
                    else f"{duration_seconds/60:.1f}m"
                )
            except Exception:
                pass

        return {
            "input_tokens": input_tok, "output_tokens": output_tok,
            "cache_read": cache_read, "cache_create": cache_create,
            "turns": turns, "tool_calls": total_tools, "tool_names": tool_names,
            "total_tokens": total_tok, "cost": round(cost, 4),
            "duration": duration_str, "output": final_output, "prompt": prompt_text,
            "duration_seconds": duration_seconds,
            "started_at": first_user_ts,
            "last_ts": last_assistant_ts,
        }

    for cond_name in tabs:
        session_file = session_files.get(cond_name)
        if not session_file:
            log(f"[{cond_name}] No session file tracked — skipping collection")
            continue
        if not session_file.exists():
            log(f"[{cond_name}] Session file gone: {session_file}")
            continue
        log(f"[{cond_name}] Collecting from: {session_file.name}")

        try:
            data = count_session(session_file)
            tools_str = ", ".join(f"{n}:{c}" for n, c in sorted(data["tool_names"].items(), key=lambda x: -x[1])[:5])
            log(f"[{cond_name}] Turns: {data['turns']}, Tools: {data['tool_calls']} ({tools_str}), Tokens: {data['total_tokens']:,}, Cost: ${data['cost']}, Duration: {data['duration']}")

            cond_prompt = Path(prompt_files.get(cond_name, "")).read_text(encoding="utf-8") if prompt_files.get(cond_name) and Path(prompt_files.get(cond_name, "")).exists() else ""
            output_snapshot = final_output_snapshots.get(cond_name, {"exists": False})
            deliverable_status = deliverable_statuses.get(cond_name, "failed")
            full_output = tab_outputs.get(cond_name, "")
            repo_dir = next(
                Path(c["directory"])
                for c in conditions
                if c["name"] == cond_name
            )

            if deliverable_status == "success":
                if len(full_output) > len(data.get("output", "")):
                    data["output"] = full_output
                    log(
                        f"[{cond_name}] Using output file ({len(full_output)} chars) "
                        f"in place of JSONL ({len(data.get('output', ''))} chars)"
                    )
                parsed_candidate = None
                if req.evalType == "bug-fix-1":
                    parsed_candidate, assessment = _parse_and_score_bug_fix_1_output(
                        data["output"],
                        cost=data["cost"],
                        repo_path=repo_dir,
                    )
                    score = float(assessment.get("weighted_score", 0.0))
                    log(
                        f"[{cond_name}] Structured score: {score:.1f} "
                        f"(parsed_candidate={'yes' if parsed_candidate else 'no'})"
                    )
                elif req.evalType == "dead-code":
                    parsed_candidate, assessment = _parse_and_score_dead_code_output(
                        data["output"],
                        cost=data["cost"],
                        target=req.target,
                        repo_path=repo_dir,
                    )
                    score = float(assessment.get("weighted_score", 0.0))
                    log(
                        f"[{cond_name}] Structured score: {score:.1f} "
                        f"(parsed_candidate={'yes' if parsed_candidate else 'no'})"
                    )
                else:
                    score = _score_output(req.evalType, data["output"], data["cost"], prompt=cond_prompt)
                    assessment = _assessment_from_score(req.evalType, score)
                    log(f"[{cond_name}] Score: {score:.1f}")
            else:
                data["output"] = ""
                parsed_candidate = None
                score = 0.0
                assessment = _missing_deliverable_assessment(
                    req.evalType,
                    expected_output_file=str(output_files.get(cond_name)) if output_files.get(cond_name) else None,
                    output_snapshot=output_snapshot,
                    fallback_chars=len(full_output),
                    last_tab_status=last_tab_statuses.get(cond_name),
                )
                log(
                    f"[{cond_name}] Missing deliverable: status={deliverable_status} "
                    f"output_exists={output_snapshot.get('exists', False)} "
                    f"chars={output_snapshot.get('chars', 0)} "
                    f"fallback_chars={len(full_output)}"
                )

            transcript = _read_session_events(session_file)
            tool_calls = _extract_tool_calls(transcript)
            run_metadata = _condition_run_metadata(
                repo_path=repo_dir,
                run_id=run_id,
                condition=cond_name,
                req=req,
                session_id=session_ids.get(cond_name),
                started_at=data.get("started_at"),
                finished_at=data.get("last_ts"),
                status=deliverable_status,
            )
            structured_output = {
                "raw_output": data.get("output", ""),
                "expected_output_file": str(output_files.get(cond_name)) if output_files.get(cond_name) else None,
                "deliverable_status": deliverable_status,
                "output_snapshot": output_snapshot,
                "fallback_output_chars": len(full_output),
            }
            if parsed_candidate is not None:
                structured_output["parsed_candidate"] = parsed_candidate
            if deliverable_status != "success" and full_output:
                structured_output["partial_output"] = full_output
            run_payload = {
                "command": (
                    f"claude --dangerously-skip-permissions --model {req.model} "
                    f"--session-id {session_ids.get(cond_name, '')} -n aethyme-{cond_name}"
                ),
                "stdout": tab_outputs.get(cond_name, ""),
                "stderr": "",
                "exit_code": 0 if cond_name in completed else None,
                "input_tokens": data["input_tokens"],
                "output_tokens": data["output_tokens"],
                "cache_read_tokens": data["cache_read"],
                "cache_create_tokens": data["cache_create"],
                "num_turns": data["turns"],
                "tool_calls": tool_calls,
                "duration_seconds": data.get("duration_seconds"),
                "cost_usd": data["cost"],
                "final_output_message": data.get("output", ""),
                "structured_output": structured_output,
                "run_metadata": run_metadata,
            }
            condition_payloads[cond_name] = {
                "prompt": cond_prompt,
                "run": run_payload,
                "assessment": assessment,
            }

            store_condition_raw(
                run_dir,
                cond_name,
                stdout=tab_outputs.get(cond_name, ""),
                stderr="",
                structured_output=structured_output,
                tokens={
                    "input_tokens": data["input_tokens"],
                    "output_tokens": data["output_tokens"],
                    "cache_read_tokens": data["cache_read"],
                    "cache_create_tokens": data["cache_create"],
                    "total_tokens": data["total_tokens"],
                },
                duration_seconds=data.get("duration_seconds"),
                tool_calls=tool_calls,
                exit_code=0 if deliverable_status == "success" else None,
                command=run_payload["command"],
            )
            store_condition_chau7(
                run_dir,
                cond_name,
                run_id=run_id,
                session_id=session_ids.get(cond_name),
                transcript=transcript,
                tool_calls=tool_calls,
                tab_output=tab_outputs.get(cond_name, ""),
            )

            from db import insert_result
            cto = "off" if cond_name == "control-cto-off" else "on"
            tool_breakdown_json = json.dumps(
                {k: v for k, v in sorted(data["tool_names"].items(), key=lambda x: -x[1]) if k != "Agent"}
            )
            result_id = f"{run_dir.name}-{cond_name}"

            # P3: LLM-as-judge scoring (runs alongside keyword score).
            # Judge uses Codex via a Chau7 tab — same path as eval agents,
            # no direct API calls. Runs in a neutral directory (Aethyme repo
            # root) so it never touches the playground Control or Aethyme repos.
            judge_result: dict[str, Any] | None = None
            if req.useJudge and data.get("output"):
                try:
                    from llm_judge import score_eval_type_with_judge
                    log(
                        f"[{cond_name}] Running LLM judge via Codex "
                        f"(samples={req.judgeSamples})..."
                    )
                    judge_result = score_eval_type_with_judge(
                        eval_type=req.evalType,
                        task=bare_task,
                        candidate=data["output"],
                        aethyme_pkg_path=str(AETHYME_PKG),
                        tab_directory=str(AETHYME_PKG),
                        samples=req.judgeSamples,
                        log=lambda m: log(f"[{cond_name}] {m}"),
                    )
                    if judge_result.get("error"):
                        log(f"[{cond_name}] Judge warning: {judge_result['error']}")
                    else:
                        log(
                            f"[{cond_name}] Judge score: "
                            f"{judge_result['mean_score']:.1f} "
                            f"(stdev={judge_result['stdev']:.2f}, "
                            f"reliable={judge_result['reliable']})"
                        )
                except Exception as e:
                    log(f"[{cond_name}] Judge failed: {type(e).__name__}: {e}")
                    judge_result = {
                        "mean_score": None, "stdev": None, "samples": [],
                        "backend": "codex", "reliable": False,
                        "error": f"{type(e).__name__}: {e}",
                    }

            insert_result({
                "id": result_id,
                "runId": run_id,
                "runDir": run_dir.name,
                "date": datetime.now(UTC).isoformat()[:19],
                "evalType": req.evalType,
                "target": req.target,
                "model": req.model,
                "condition": cond_name,
                "reasoning": req.reasoning,
                "cto": cto,
                "score": score,
                "turns": data["turns"],
                "toolCalls": data["tool_calls"],
                "totalTokens": data["total_tokens"],
                "inputTokens": data["input_tokens"],
                "outputTokens": data["output_tokens"],
                "cacheRead": data["cache_read"],
                "cacheCreate": data["cache_create"],
                "cost": data["cost"],
                "duration": data["duration"],
                "fixed": 0,
                "output": data.get("output"),
                "prompt": data.get("prompt"),
                "rawJson": json.dumps(condition_payloads[cond_name]),
                "toolBreakdown": tool_breakdown_json,
                # P3: judge fields. judgeModel stores the backend ("codex")
                # for now — when multiple judge backends exist we can extend it
                # to capture the specific model string from the Chau7 session.
                "judgeScore": judge_result["mean_score"] if judge_result else None,
                "judgeStdev": judge_result["stdev"] if judge_result else None,
                "judgeModel": judge_result.get("backend") if judge_result else None,
                "judgeReliable": judge_result["reliable"] if judge_result else None,
                "judgeSamples": json.dumps(judge_result["samples"]) if judge_result else None,
                "judgeError": judge_result.get("error") if judge_result else None,
                # judgeCostUsd deprecated — cost tracked in Chau7 session telemetry.
                "judgeCostUsd": None,
                # P9: wall-clock overhead the judge added for this row.
                "judgeElapsedSeconds": judge_result.get("elapsed_seconds") if judge_result else None,
                # P1: batch metadata
                "batchId": batch_id,
                "runIndex": run_index,
                "runsInBatch": runs_in_batch,
                # Whether the agent produced the required structured output.
                "deliverableStatus": deliverable_statuses.get(cond_name),
                # Pre-registration (P8) — the run request declared these up
                # front; carrying them on every row keeps them queryable
                # even if the plan snapshot is lost.
                "primaryMetric": req.primaryMetric,
                "minimumMeaningfulDelta": req.minimumMeaningfulDelta,
            })
            log(f"[{cond_name}] Stored: {result_id} (output: {len(data.get('output',''))} chars)")
        except Exception as e:
            log(f"[{cond_name}] ERROR collecting: {e}")
            import traceback
            log(traceback.format_exc())
    finish_phase(
        "collecting",
        status=(
            "success"
            if len(condition_payloads) == len(tabs) and all(v == "success" for v in deliverable_statuses.values())
            else "degraded"
        ),
        details={
            "collected_conditions": sorted(condition_payloads.keys()),
            "tab_outputs": {cond: len(text) for cond, text in tab_outputs.items()},
            "deliverable_statuses": deliverable_statuses,
            "output_snapshots": final_output_snapshots,
        },
    )

    result: dict[str, Any] = {
        "task": bare_task,
        "eval_type": req.evalType,
        "target": req.target,
        "scenario": plan.get("meta", {}).get("scenario"),
        "model": plan.get("meta", {}).get("model"),
        "run_id": run_id,
        **shared_artifacts,
    }
    result.update(condition_payloads)
    result["report"] = {
        "task": bare_task,
        "condition_prompt_chars": {
            cond_name: len(side.get("prompt", ""))
            for cond_name, side in condition_payloads.items()
        },
        "navigation_items": len((shared_artifacts.get("navigation_context") or {}).get("commands", []))
        if isinstance(shared_artifacts.get("navigation_context"), dict)
        else 0,
        "risk_items": 0,
        "condition_runs": {
            cond_name: side.get("run")
            for cond_name, side in condition_payloads.items()
        },
        "deliverable_statuses": deliverable_statuses,
        "baseline_prompt_chars": len(condition_payloads.get("control-cto-off", {}).get("prompt", "")),
        "aethyme_prompt_chars": len(condition_payloads.get("leverage", {}).get("prompt", "")),
        "task_conditioned_prompt_chars": len(condition_payloads.get("task-conditioned", {}).get("prompt", "")),
        "baseline_run": condition_payloads.get("control-cto-off", {}).get("run"),
        "aethyme_run": condition_payloads.get("leverage", {}).get("run"),
        "task_conditioned_run": condition_payloads.get("task-conditioned", {}).get("run"),
    }

    start_phase("finalizing", order=7, details={"conditions": sorted(condition_payloads.keys())})

    # Compute rolling control baseline from history for this (model, eval_type,
    # target, scenario) tuple — stabilizes global_score against single-run
    # variance in the co-run control. Falls back to the co-run control when
    # fewer than MIN_ROLLING_SAMPLES historical rows exist.
    from baseline import compute_rolling_baseline
    rolling_baseline = compute_rolling_baseline(
        model=req.model,
        eval_type=req.evalType,
        target=req.target,
        scenario=result.get("scenario"),
    )
    if rolling_baseline:
        log(
            f"Rolling baseline: quality={rolling_baseline['quality_score']:.1f} "
            f"tokens={rolling_baseline['total_tokens']:,} "
            f"duration={rolling_baseline['duration_seconds']:.1f}s "
            f"cost=${rolling_baseline['cost_usd']:.2f} "
            f"(n={rolling_baseline['window_size']})"
        )
    else:
        log("Rolling baseline: insufficient history — falling back to co-run control")

    finalize_eval_run(
        run_dir,
        result,
        repo_path=Path(req.target),
        eval_type=req.evalType,
        baseline_override=rolling_baseline,
    )
    result["report_path"] = str(run_dir / "report.md")

    # Upsert finalized per-condition rows so first-class DB columns include
    # summary metrics computed during finalize_eval_run().
    for cond_name, side in condition_payloads.items():
        if not isinstance(side, dict):
            continue
        assessment = side.get("assessment") or {}
        run = side.get("run") or {}
        summary = side.get("summary_metrics") or {}
        if not isinstance(assessment, dict):
            assessment = {}
        if not isinstance(run, dict):
            run = {}
        if not isinstance(summary, dict):
            summary = {}

        tool_calls = run.get("tool_calls")
        if isinstance(tool_calls, list):
            breakdown = Counter(
                str(call.get("tool"))
                for call in tool_calls
                if isinstance(call, dict) and call.get("tool")
            )
            tool_breakdown_json = json.dumps(
                {k: v for k, v in sorted(breakdown.items(), key=lambda item: (-item[1], item[0]))}
            ) if breakdown else None
        else:
            tool_breakdown_json = None

        # Preserve judge + batch fields that were written during the collect phase.
        # insert_result uses INSERT OR REPLACE, so any column not set here would
        # be nulled out. We read the existing row first and forward those fields.
        row_id = f"{run_dir.name}-{cond_name}"
        judge_carry = _fetch_judge_and_batch_fields(row_id)

        insert_result({
            "id": row_id,
            "runId": ((run.get("run_metadata") or {}).get("run_id") if isinstance(run.get("run_metadata"), dict) else run_id),
            "runDir": run_dir.name,
            "date": datetime.now(UTC).isoformat()[:19],
            "evalType": req.evalType,
            "target": req.target,
            "model": req.model,
            "condition": cond_name,
            "reasoning": req.reasoning,
            "cto": ("off" if cond_name == "control-cto-off" else "on"),
            "score": assessment.get("weighted_score", 0),
            "turns": run.get("num_turns", 0),
            "toolCalls": summary.get("tool_call_count", 0),
            "totalTokens": summary.get("total_tokens", 0),
            "inputTokens": run.get("input_tokens", 0),
            "outputTokens": run.get("output_tokens", 0),
            "cacheRead": run.get("cache_read_tokens", 0),
            "cacheCreate": run.get("cache_create_tokens", 0),
            "cost": run.get("cost_usd", 0),
            "duration": (f"{run.get('duration_seconds', 0):.0f}s" if run.get("duration_seconds") else "-"),
            "fixed": 0,
            "scenario": result.get("scenario"),
            "output": run.get("output"),
            "prompt": side.get("prompt"),
            "rawJson": json.dumps(side),
            "toolBreakdown": tool_breakdown_json,
            "qualityScore": summary.get("quality_score"),
            "recalculatedEvalScore": summary.get("recalculated_eval_score", summary.get("global_score")),
            "qualityDeltaVsControl": summary.get("quality_delta_vs_control"),
            "tokenRatioVsControl": summary.get("token_ratio_vs_control"),
            "timeRatioVsControl": summary.get("time_ratio_vs_control"),
            "costRatioVsControl": summary.get("cost_ratio_vs_control"),
            "scorePer1kTokens": summary.get("score_per_1k_tokens"),
            "scorePerMinute": summary.get("score_per_minute"),
            "topTools": (json.dumps(summary.get("top_tools")) if summary.get("top_tools") else None),
            **judge_carry,
        })

    _run_state["result"] = result
    log(f"Finalized eval run: {run_dir / 'report.md'}")
    finish_phase(
        "finalizing",
        status="success",
        details={
            "report_path": str(run_dir / "report.md"),
            "complete_result_path": str(run_dir / "complete-result.json"),
        },
    )

    # Phase: Cleanup
    start_phase(
        "cleanup",
        order=8,
        details={
            "tabs": list(tabs.keys()),
            "cleanup_delay_seconds": req.cleanupDelaySeconds,
        },
    )
    if req.cleanupDelaySeconds > 0:
        log(f"Waiting {req.cleanupDelaySeconds}s before closing tabs...")
        time.sleep(req.cleanupDelaySeconds)
    log("Closing tabs...")
    for cond_name, tab_id in tabs.items():
        try:
            mcp_client.tab_close(tab_id)
            log(f"[{cond_name}] Tab closed")
        except Exception as e:
            log(f"[{cond_name}] Close error: {e}")

    _run_state["status"] = "complete"
    _run_state["currentPhase"] = "done"
    finish_phase(
        "cleanup",
        status="success",
        details={
            "closed_tabs": list(tabs.keys()),
            "cleanup_delay_seconds": req.cleanupDelaySeconds,
        },
    )
    log("Eval run complete.")


@app.post("/api/run")
async def launch_run(req: RunRequest, background_tasks: BackgroundTasks) -> dict[str, Any]:
    import mcp_client

    if not mcp_client.is_available():
        return {
            "status": "error",
            "plan": None,
            "currentPhase": None,
            "log": ["Chau7 MCP not available — is Chau7 running?"],
            "error": "MCP socket not found",
        }

    preparation_snapshot = (
        _read_preparation_snapshot(req.preparationId)
        if req.preparationId
        else _latest_preparation_snapshot(req.target)
    )
    if preparation_snapshot is None:
        return {
            "status": "error",
            "plan": None,
            "currentPhase": None,
            "log": ["No repository preparation snapshot found. Prepare the target first."],
            "error": "Missing preparation snapshot",
        }
    if not preparation_snapshot.get("ready"):
        return {
            "status": "error",
            "plan": None,
            "currentPhase": None,
            "log": ["Repository preparation is not ready.", *preparation_snapshot.get("errors", [])],
            "error": "Preparation not ready",
        }

    try:
        plan, plan_snapshot = await _generate_plan(req)
    except HTTPException as exc:
        return {
            "status": "error", "plan": None, "currentPhase": None,
            "log": [str(exc.detail)],
            "error": str(exc.detail),
        }

    # Launch in background. For multi-run (req.runs > 1) a wrapper loops
    # the background job N times, regenerating the plan per iteration so
    # each run gets its own run_dir / tab set, all sharing one batch_id.
    run_dir_name = Path(plan["paths"]["run_dir"]).name

    if req.runs > 1:
        import time as _time
        batch_id = f"batch-{int(_time.time())}-{req.target}-{req.evalType}"
        background_tasks.add_task(
            _run_eval_batch,
            plan,
            req,
            plan_snapshot=plan_snapshot,
            preparation_snapshot=preparation_snapshot,
            first_run_dir_name=run_dir_name,
            batch_id=batch_id,
        )
        launch_message = (
            f"Plan generated. Running batch of {req.runs} sequential evals "
            f"(batch_id={batch_id}). Each run gets its own run_dir; all share batch metadata."
        )
    else:
        background_tasks.add_task(
            _run_eval_background,
            plan,
            req,
            plan_snapshot=plan_snapshot,
            preparation_snapshot=preparation_snapshot,
            run_dir_name=run_dir_name,
            batch_id=None,
            run_index=1,
            runs_in_batch=1,
        )
        launch_message = (
            "Plan generated. Building evaluation inputs, then launching agents via Chau7 MCP..."
        )

    return {
        "status": "running",
        "plan": plan,
        "currentPhase": "build inputs",
        "log": [
            f"Using preparation {preparation_snapshot['id']}.",
            launch_message,
        ],
        "error": None,
    }


@app.get("/api/run/status")
async def run_status() -> dict[str, Any]:
    return _run_state
