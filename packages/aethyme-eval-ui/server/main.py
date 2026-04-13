from __future__ import annotations

import asyncio
import hashlib
import json
import subprocess
import uuid
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from fastapi import BackgroundTasks, FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

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
    _setup_tasks[task_id]["status"] = "running"
    try:
        current_snapshot = _build_repository_preparation(target)
        if current_snapshot.get("ready") and not force:
            snapshot = _write_preparation_snapshot(target, current_snapshot)
            _setup_tasks[task_id]["status"] = "complete"
            _setup_tasks[task_id]["skipped"] = True
            _setup_tasks[task_id]["output"] = "Playground already ready; setup skipped."
            _setup_tasks[task_id]["preparation"] = snapshot
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

        proc = subprocess.run(
            cmd,
            cwd=str(AETHYME_PKG),
            capture_output=True,
            text=True,
            timeout=1800,
        )
        _setup_tasks[task_id]["output"] = (proc.stdout + proc.stderr)[-8000:]
        if proc.returncode != 0:
            _setup_tasks[task_id]["status"] = "error"
            _setup_tasks[task_id]["error"] = f"setup-playground failed with exit code {proc.returncode}"
            return

        snapshot = _write_preparation_snapshot(target, _build_repository_preparation(target))
        _setup_tasks[task_id]["status"] = "complete"
        _setup_tasks[task_id]["skipped"] = False
        _setup_tasks[task_id]["preparation"] = snapshot
    except subprocess.TimeoutExpired:
        _setup_tasks[task_id]["status"] = "error"
        _setup_tasks[task_id]["error"] = "Playground setup timed out after 1800s"
    except Exception as e:
        _setup_tasks[task_id]["status"] = "error"
        _setup_tasks[task_id]["error"] = str(e)

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
    _setup_tasks[task_id] = {
        "status": "queued",
        "target": req.target,
        "source": source,
        "commit": commit,
        "force": req.force,
    }
    background_tasks.add_task(_run_setup_target, req.target, source, commit, req.force, task_id)
    return {"success": True, "taskId": task_id}


@app.get("/api/repositories/index/status/{task_id}")
async def index_status(task_id: str) -> dict[str, Any]:
    if task_id not in _index_tasks:
        raise HTTPException(status_code=404, detail="Task not found")
    return _index_tasks[task_id]


@app.get("/api/repositories/setup/status/{task_id}")
async def setup_status(task_id: str) -> dict[str, Any]:
    if task_id not in _setup_tasks:
        raise HTTPException(status_code=404, detail="Task not found")
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

@app.post("/api/plan")
async def generate_plan(req: PlanRequest) -> dict[str, Any]:
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
    except FileNotFoundError:
        raise HTTPException(status_code=500, detail=f"Python venv not found at {AETHYME_PYTHON}")

    if proc.returncode != 0:
        raise HTTPException(status_code=500, detail=f"Plan generation failed: {stderr.decode()[:1000]}")

    try:
        return json.loads(stdout.decode())
    except json.JSONDecodeError:
        raise HTTPException(status_code=500, detail=f"Failed to parse plan output: {stdout.decode()[:500]}")


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


def _shared_eval_artifacts(eval_type: str, task: str) -> dict[str, Any]:
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
        "dead-code": ("mediawiki_dead_code_output_schema", "mediawiki_dead_code_scoring_rubric", "mediawiki_dead_code_reference"),
        "migration": ("mediawiki_migration_output_schema", "mediawiki_migration_scoring_rubric", "mediawiki_migration_reference"),
    }

    schema_name, rubric_name, reference_name = mapping.get(eval_type, (None, None, None))
    if schema_name and hasattr(eval_schemas, schema_name):
        artifacts["output_schema"] = getattr(eval_schemas, schema_name)()
    if rubric_name and hasattr(eval_schemas, rubric_name):
        artifacts["scoring_rubric"] = getattr(eval_schemas, rubric_name)()
    if reference_name and hasattr(eval_schemas, reference_name):
        reference = getattr(eval_schemas, reference_name)()
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


def _append_run_log(run_dir: Path | None, msg: str) -> None:
    if run_dir is None:
        return
    try:
        with open(run_dir / "events.log", "a", encoding="utf-8") as f:
            f.write(f"{datetime.now(UTC).isoformat()} {msg}\n")
    except OSError:
        pass


def _materialize_eval_inputs(
    plan: dict[str, Any],
    req: Any,
    conditions: list[dict[str, Any]],
    *,
    log: Any,
) -> dict[str, Any]:
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

Produce your analysis in exactly these sections:

1. **Root Cause** — What code path leads to the wrong behavior. Trace from the user action (viewing a diff) to the incorrect result (all revisions marked seen).
2. **Files to Edit** — List each file that needs changes, with the specific function or method to modify and what the change should be.
3. **Fix Plan** — Step-by-step description of the fix. Include method signature changes, parameter changes, and deprecation approach if applicable.
4. **Testing** — How to verify the fix works. What behavior to test before and after.

Cite file paths and line numbers where relevant.""",
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
        "dead-code": """\
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

Be thorough — check every public function, not just a sample. Missing a truly unused function or falsely flagging a used one both count against you.""",
        "migration": """\
We are renaming the class `WatchedItemStore` to `WatchlistNotificationStore`.

List every PHP file outside of tests/ and vendor/ that references `WatchedItemStore` and would need updating. For each file, provide:
- The file path (relative to repo root)
- What change is needed (e.g., "update class reference", "update type hint", "update service wiring")

Exclude `includes/Watchlist/WatchedItemStore.php` itself — that's the file being renamed, not a reference to it.

Be exhaustive — missing a file means the rename breaks production. Search thoroughly across all of `includes/`, `maintenance/`, `docs/`, and root-level files.""",
    }

    bare_task = eval_tasks.get(eval_type, f"Analyze this repository for: {eval_type}")
    log(f"Eval type: {eval_type}")
    log(f"Bare task: {len(bare_task)} chars")

    shared_artifacts = _shared_eval_artifacts(eval_type, bare_task)
    shared_artifacts["target"] = req.target
    shared_artifacts["model"] = plan.get("meta", {}).get("model")
    shared_artifacts["scenario"] = plan.get("meta", {}).get("scenario")
    shared_artifacts["signals"] = {}

    output_files: dict[str, Path] = {}
    for cond in conditions:
        cond_name = cond["name"]
        repo_dir = Path(cond["directory"])
        output_path = repo_dir / f".aethyme-eval-output-{cond_name}.md"
        output_files[cond_name] = output_path
        output_path.unlink(missing_ok=True)

    for prompt_path in prompt_files.values():
        Path(prompt_path).unlink(missing_ok=True)

    enriched_prompt = bare_task
    enrichment_meta: dict[str, Any] | None = None
    try:
        aethyme_dir = next((c["directory"] for c in conditions if c["name"] == "leverage"), "")
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
            enriched_proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
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
                log(f"WARNING: Engine failed, using bare for leverage. stderr: {enriched_proc.stderr[:200]}")
        else:
            log("No engine available, using bare prompt for leverage")
    except Exception as e:
        log(f"ERROR writing prompts: {e}")

    for cond_name, prompt_path in prompt_files.items():
        task_text = enriched_prompt if cond_name == "leverage" else bare_task
        out_path = str(output_files[cond_name])
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
    return {
        "bare_task": bare_task,
        "shared_artifacts": shared_artifacts,
        "prompt_files": prompt_files,
        "output_files": output_files,
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
    (e.g., the leverage prompt with subsystem context).
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


def _run_eval_background(
    plan: dict[str, Any],
    req: RunRequest,
    *,
    preparation_snapshot: dict[str, Any] | None = None,
    run_dir_name: str | None = None,
) -> None:
    import mcp_client
    import time
    import traceback
    _ensure_aethyme_imports()
    from src.eval.report import (
        create_eval_run_dir,
        finalize_eval_run,
        store_condition_chau7,
        store_condition_raw,
    )

    run_dir: Path | None = None

    def log(msg: str) -> None:
        _run_state["log"].append(msg)
        _append_run_log(run_dir, msg)
        print(f"[eval-run] {msg}")

    _run_state["status"] = "running"
    _run_state["plan"] = plan
    _run_state["log"] = []
    _run_state["error"] = None

    log(f"Starting {req.evalType} on {req.target} with {req.model}")
    log(f"Plan has {len(plan['phases'])} phases")
    if req.windowId is not None:
        log(f"Requested Chau7 window_id: {req.windowId}")

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
        if preparation_snapshot is not None:
            metadata["preparation"] = {
                "id": preparation_snapshot.get("id"),
                "ready": preparation_snapshot.get("ready"),
                "path": preparation_snapshot.get("path"),
            }
        metadata_path.write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    except Exception:
        pass

    # Artifact build is separate from repository preparation and from tab execution.
    _run_state["currentPhase"] = "build inputs"
    prepared_inputs = _materialize_eval_inputs(plan, req, conditions, log=log)
    prompt_files = prepared_inputs["prompt_files"]
    output_files = prepared_inputs["output_files"]
    shared_artifacts = prepared_inputs["shared_artifacts"]
    shared_artifacts["run_id"] = run_id
    shared_artifacts["preparation"] = preparation_snapshot
    bare_task = prepared_inputs["bare_task"]
    condition_payloads: dict[str, dict[str, Any]] = {}

    tabs: dict[str, str] = {}

    # Phase: Clean up stale MCP tabs
    _run_state["currentPhase"] = "cleanup stale tabs"
    try:
        existing_tabs = mcp_client.tab_list()
        mcp_tabs = [t for t in existing_tabs if t.get("is_mcp_controlled")]
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

    # Phase: Create tabs — wait for each shell to be ready before moving on
    _run_state["currentPhase"] = "creating tabs"
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

    # Phase: Launch agents — with session file tracking
    _run_state["currentPhase"] = "launching agents"
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

    # Phase: Monitor — poll until all agents complete
    _run_state["currentPhase"] = "monitoring"
    log("Monitoring agents...")
    completed: set[str] = set()
    for poll_round in range(120):  # Up to 20 minutes
        time.sleep(10)
        all_done = True
        for cond_name, tab_id in tabs.items():
            if cond_name in completed:
                continue
            try:
                status = mcp_client.tab_status(tab_id)
                app = status.get("active_app", "?")
                st = status.get("status", "?")
                at_prompt = status.get("is_at_prompt", False)
                # Done when agent is at prompt (finished response) or shell is back (agent exited)
                # Chau7 may report active_app as "Claude" or "Cline" depending on version
                # Some tabs report is_at_prompt=false but raw_status=waitingForInput when done
                is_agent = app in ("Claude", "Cline")
                raw_st = status.get("raw_status", "")
                if is_agent and (at_prompt or raw_st == "waitingForInput") and st in ("waitingForInput", "idle"):
                    completed.add(cond_name)
                    log(f"[{cond_name}] Completed (round {poll_round}, app={app}, at_prompt={at_prompt}, raw={raw_st})")
                elif not is_agent and app != "?" and st == "idle":
                    completed.add(cond_name)
                    log(f"[{cond_name}] Agent exited (round {poll_round})")
                else:
                    all_done = False
            except Exception as e:
                log(f"[{cond_name}] Poll error: {e}")
                all_done = False

        if poll_round % 6 == 0:
            log(f"Poll round {poll_round}: {len(completed)}/{len(tabs)} complete")

        if all_done:
            break

    log(f"All agents finished. Completed: {list(completed)}")

    # Phase: Collect — try output file first, fall back to PTY log
    _run_state["currentPhase"] = "collecting"
    log("Collecting results...")

    tab_outputs: dict[str, str] = {}
    for cond_name, tab_id in tabs.items():
        # Try output file first
        output_file = output_files.get(cond_name)
        if output_file and output_file.exists():
            text = output_file.read_text(encoding="utf-8")
            tab_outputs[cond_name] = text
            log(f"[{cond_name}] Output file read: {len(text)} chars")
        else:
            # Fall back to PTY log + cleaning
            try:
                raw = mcp_client.tab_output(tab_id, lines=10000, source="pty_log")
                raw_text = raw.get("output", "")
                cleaned = _clean_pty_output(raw_text)
                tab_outputs[cond_name] = cleaned
                log(f"[{cond_name}] PTY log fallback: {len(raw_text)} raw → {len(cleaned)} cleaned chars")
            except Exception as e:
                tab_outputs[cond_name] = ""
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

            # Use tab output as the full analysis (JSONL misses final assistant response)
            full_output = tab_outputs.get(cond_name, "")
            if len(full_output) > len(data.get("output", "")):
                data["output"] = full_output
                log(f"[{cond_name}] Using tab output ({len(full_output)} chars) instead of JSONL ({len(data.get('output', ''))} chars)")

            # Score the output — pass the prompt to exclude it from keyword matching
            cond_prompt = Path(prompt_files.get(cond_name, "")).read_text(encoding="utf-8") if prompt_files.get(cond_name) and Path(prompt_files.get(cond_name, "")).exists() else ""
            score = _score_output(req.evalType, data["output"], data["cost"], prompt=cond_prompt)
            log(f"[{cond_name}] Score: {score:.1f}")

            transcript = _read_session_events(session_file)
            tool_calls = _extract_tool_calls(transcript)
            repo_dir = next(
                Path(c["directory"])
                for c in conditions
                if c["name"] == cond_name
            )
            run_metadata = _condition_run_metadata(
                repo_path=repo_dir,
                run_id=run_id,
                condition=cond_name,
                req=req,
                session_id=session_ids.get(cond_name),
                started_at=data.get("started_at"),
                finished_at=data.get("last_ts"),
                status="success" if cond_name in completed else "degraded",
            )
            structured_output = {
                "raw_output": data.get("output", ""),
            }
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
            assessment = _assessment_from_score(req.evalType, score)
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
                exit_code=0 if cond_name in completed else None,
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
            from datetime import datetime, timezone
            cto = "off" if cond_name == "control-cto-off" else "on"
            tool_breakdown_json = json.dumps(
                {k: v for k, v in sorted(data["tool_names"].items(), key=lambda x: -x[1]) if k != "Agent"}
            )
            result_id = f"{run_dir.name}-{cond_name}"
            insert_result({
                "id": result_id,
                "runId": run_id,
                "runDir": run_dir.name,
                "date": datetime.now(timezone.utc).isoformat()[:19],
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
            })
            log(f"[{cond_name}] Stored: {result_id} (output: {len(data.get('output',''))} chars)")
        except Exception as e:
            log(f"[{cond_name}] ERROR collecting: {e}")
            import traceback
            log(traceback.format_exc())

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
        "baseline_prompt_chars": len(condition_payloads.get("control-cto-off", {}).get("prompt", "")),
        "aethyme_prompt_chars": len(condition_payloads.get("leverage", {}).get("prompt", "")),
        "baseline_run": condition_payloads.get("control-cto-off", {}).get("run"),
        "aethyme_run": condition_payloads.get("leverage", {}).get("run"),
    }

    finalize_eval_run(run_dir, result, repo_path=Path(req.target), eval_type=req.evalType)
    result["report_path"] = str(run_dir / "report.md")
    _run_state["result"] = result
    log(f"Finalized eval run: {run_dir / 'report.md'}")

    # Phase: Cleanup
    _run_state["currentPhase"] = "cleanup"
    log("Closing tabs...")
    for cond_name, tab_id in tabs.items():
        try:
            mcp_client.tab_close(tab_id)
            log(f"[{cond_name}] Tab closed")
        except Exception as e:
            log(f"[{cond_name}] Close error: {e}")

    _run_state["status"] = "complete"
    _run_state["currentPhase"] = "done"
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

    # Generate plan first
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
    except FileNotFoundError:
        return {
            "status": "error", "plan": None, "currentPhase": None,
            "log": [f"Python venv not found at {AETHYME_PYTHON}"],
            "error": "Missing venv",
        }

    if proc.returncode != 0:
        return {
            "status": "error", "plan": None, "currentPhase": None,
            "log": [f"Plan generation failed: {stderr.decode()[:500]}"],
            "error": stderr.decode()[:500],
        }

    try:
        plan = json.loads(stdout.decode())
    except json.JSONDecodeError:
        return {
            "status": "error", "plan": None, "currentPhase": None,
            "log": ["Failed to parse plan output"],
            "error": "Invalid JSON",
        }

    # Launch in background
    run_dir_name = Path(plan["paths"]["run_dir"]).name
    background_tasks.add_task(
        _run_eval_background,
        plan,
        req,
        preparation_snapshot=preparation_snapshot,
        run_dir_name=run_dir_name,
    )

    return {
        "status": "running",
        "plan": plan,
        "currentPhase": "build inputs",
        "log": [
            f"Using preparation {preparation_snapshot['id']}.",
            "Plan generated. Building evaluation inputs, then launching agents via Chau7 MCP...",
        ],
        "error": None,
    }


@app.get("/api/run/status")
async def run_status() -> dict[str, Any]:
    return _run_state
