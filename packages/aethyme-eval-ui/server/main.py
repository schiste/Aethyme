from __future__ import annotations

import asyncio
import json
import subprocess
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

app = FastAPI(title="Aethyme Eval API")

app.add_middleware(
    CORSMiddleware,
    allow_origins=["http://localhost:5173"],
    allow_methods=["*"],
    allow_headers=["*"],
)

_index_tasks: dict[str, dict[str, Any]] = {}


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


class IndexRequest(BaseModel):
    target: str

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

@app.post("/api/repositories/index")
async def index_repository(req: IndexRequest, background_tasks: BackgroundTasks) -> dict[str, Any]:
    if not ENGINE_BINARY.exists():
        raise HTTPException(status_code=500, detail=f"Engine binary not found at {ENGINE_BINARY}")

    task_id = f"index-{req.target}"
    _index_tasks[task_id] = {"status": "queued", "target": req.target}
    background_tasks.add_task(_run_index, req.target, task_id)
    return {"success": True, "taskId": task_id}


@app.get("/api/repositories/index/status/{task_id}")
async def index_status(task_id: str) -> dict[str, Any]:
    if task_id not in _index_tasks:
        raise HTTPException(status_code=404, detail="Task not found")
    return _index_tasks[task_id]


# ---------------------------------------------------------------------------
# Plan / Run
# ---------------------------------------------------------------------------

class PlanRequest(BaseModel):
    evalType: str
    target: str
    model: str
    reasoning: str = "high"

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

@app.post("/api/chau7/tabs/create")
async def chau7_tab_create(req: TabCreateRequest) -> dict[str, Any]:
    import mcp_client
    return mcp_client.tab_create(req.directory)


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

_run_state: dict[str, Any] = {
    "status": "idle",
    "plan": None,
    "currentPhase": None,
    "log": [],
    "tabs": {},
    "error": None,
}

def _run_eval_background(plan: dict[str, Any], req: RunRequest) -> None:
    import mcp_client
    import time
    import traceback

    def log(msg: str) -> None:
        _run_state["log"].append(msg)
        print(f"[eval-run] {msg}")

    _run_state["status"] = "running"
    _run_state["plan"] = plan
    _run_state["log"] = []
    _run_state["error"] = None

    log(f"Starting {req.evalType} on {req.target} with {req.model}")
    log(f"Plan has {len(plan['phases'])} phases")

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

    # Phase: Prepare — generate prompt files for ALL eval types
    _run_state["currentPhase"] = "prepare"
    prompt_files = plan["paths"]["prompt_files"]
    eval_type = req.evalType

    # Task text per eval type
    EVAL_TASKS = {
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
        "bug-fix": """\
A test is failing in this repository. Find the root cause and fix the bug so all tests pass.

The test file is correct. The bug is in the source code.""",
        "navigation-ctf": """\
Find the manifest that manages the main code entrypoint, identify the entrypoint file it controls, and name the top-level area that owns both.

Produce a structured analysis with config_target, code_target, management_area, and relationship_chain.""",
    }

    bare_task = EVAL_TASKS.get(eval_type, f"Analyze this repository for: {eval_type}")
    log(f"Eval type: {eval_type}")
    log(f"Bare task: {len(bare_task)} chars")

    try:
        # Generate enriched prompt for leverage condition
        aethyme_dir = next((c["directory"] for c in conditions if c["name"] == "leverage"), "")
        leverage_path = prompt_files.get("leverage", "")

        if leverage_path and Path(leverage_path).exists() and Path(leverage_path).stat().st_size > len(bare_task):
            enriched_prompt = Path(leverage_path).read_text(encoding="utf-8")
            log(f"Using cached enriched prompt: {len(enriched_prompt)} chars")
        elif aethyme_dir and ENGINE_BINARY.exists():
            log(f"Generating enriched prompt from engine...")
            enriched_proc = subprocess.run(
                [str(ENGINE_BINARY), "prompt", "--repo", aethyme_dir, "--task", bare_task, "--focus", "overview"],
                capture_output=True, text=True, timeout=600,
            )
            if enriched_proc.returncode == 0 and enriched_proc.stdout.strip():
                enriched_prompt = enriched_proc.stdout
                log(f"Enriched prompt generated: {len(enriched_prompt)} chars")
            else:
                enriched_prompt = bare_task
                log(f"WARNING: Engine failed, using bare for leverage. stderr: {enriched_proc.stderr[:200]}")
        else:
            enriched_prompt = bare_task
            log("No engine available, using bare prompt for leverage")

        # Write per-condition prompt files
        for cond_name, prompt_path in prompt_files.items():
            if cond_name == "leverage":
                prompt_text = enriched_prompt
            else:
                prompt_text = bare_task
            Path(prompt_path).write_text(prompt_text, encoding="utf-8")
            log(f"Wrote {cond_name}: {prompt_path} ({len(prompt_text)} chars)")
    except Exception as e:
        log(f"ERROR writing prompts: {e}")
        import traceback
        log(traceback.format_exc())

    # Verify prompt files exist
    missing = [f for f in prompt_files.values() if not Path(f).exists()]
    if missing:
        log(f"WARNING: Missing prompt files: {missing}")
    else:
        log(f"All {len(prompt_files)} prompt files ready")

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

    # Phase: Create tabs
    _run_state["currentPhase"] = "creating tabs"
    for cond in conditions:
        cond_name = cond["name"]
        directory = cond["directory"]
        log(f"[{cond_name}] Creating tab in {directory}")
        try:
            result = mcp_client.tab_create(directory)
            if result.get("error"):
                log(f"[{cond_name}] ERROR from tab_create: {result['error']}")
                continue
            tab_id = result.get("tab_id", "")
            if not tab_id:
                log(f"[{cond_name}] ERROR: tab_create returned empty tab_id: {result}")
                continue
            tabs[cond_name] = tab_id
            log(f"[{cond_name}] Tab created: {tab_id[:12]}...")

            if cond.get("cto_override"):
                time.sleep(1)
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

    # Session directories per repo type
    CONTROL_SESSION_DIR = Path.home() / ".claude" / "projects" / "-Users-christophehenner-Downloads-Repositories-Playground-Mediawiki-Mediawiki---Control"
    AETHYME_SESSION_DIR = Path.home() / ".claude" / "projects" / "-Users-christophehenner-Downloads-Repositories-Playground-Mediawiki-Mediawiki---Aethyme"
    # TODO: derive these from the target dynamically

    session_files: dict[str, Path] = {}  # condition_name → session JSONL path

    for cond in conditions:
        cond_name = cond["name"]
        tab_id = tabs.get(cond_name)
        if not tab_id:
            log(f"[{cond_name}] SKIP: no tab_id")
            continue

        # Determine session directory for this condition
        is_control = cond_name.startswith("control")
        session_dir = CONTROL_SESSION_DIR if is_control else AETHYME_SESSION_DIR

        # Snapshot existing files BEFORE launching
        existing_files = set(session_dir.glob("*.jsonl")) if session_dir.exists() else set()

        time.sleep(3)

        try:
            if backend == "claude":
                cmd = f"claude --dangerously-skip-permissions --model {model_name}"
                log(f"[{cond_name}] Executing: {cmd}")
                exec_result = mcp_client.tab_exec(tab_id, cmd)
                log(f"[{cond_name}] tab_exec result: {exec_result}")

                # Poll until Claude is ready
                log(f"[{cond_name}] Waiting for Claude to initialize...")
                ready = False
                for attempt in range(20):
                    time.sleep(2)
                    try:
                        status = mcp_client.tab_status(tab_id)
                        app = status.get("active_app", "?")
                        st = status.get("status", "?")
                        if attempt % 3 == 0:
                            log(f"[{cond_name}] Poll #{attempt}: app={app} status={st}")
                        if app == "Claude" and st == "waitingForInput":
                            ready = True
                            log(f"[{cond_name}] Claude ready after {(attempt+1)*2}s")
                            break
                    except Exception as e:
                        log(f"[{cond_name}] Poll error: {e}")

                if not ready:
                    log(f"[{cond_name}] WARNING: Claude not ready after 40s, attempting prompt anyway")

                # Identify the new session file (created after snapshot)
                if session_dir.exists():
                    current_files = set(session_dir.glob("*.jsonl"))
                    new_files = current_files - existing_files
                    if new_files:
                        session_file = max(new_files, key=lambda p: p.stat().st_mtime)
                        session_files[cond_name] = session_file
                        log(f"[{cond_name}] Session file: {session_file.name}")
                    else:
                        log(f"[{cond_name}] WARNING: No new session file detected")

                # Send prompt
                prompt_file = cond.get("prompt_file", "")
                log(f"[{cond_name}] Prompt file: {prompt_file}")
                if prompt_file:
                    try:
                        if not Path(prompt_file).exists():
                            log(f"[{cond_name}] ERROR: Prompt file does not exist: {prompt_file}")
                            continue

                        prompt_text = Path(prompt_file).read_text(encoding="utf-8")
                        log(f"[{cond_name}] Sending {len(prompt_text)} chars to input...")
                        send_result = mcp_client.tab_send_input(tab_id, prompt_text)
                        log(f"[{cond_name}] tab_send_input result: {send_result}")

                        time.sleep(2)

                        log(f"[{cond_name}] Submitting prompt...")
                        submit_result = mcp_client.tab_submit_prompt(tab_id)
                        log(f"[{cond_name}] tab_submit_prompt result: {submit_result}")

                        # Wait for Claude to start processing before moving to next tab
                        time.sleep(3)
                        try:
                            post_status = mcp_client.tab_status(tab_id)
                            post_app = post_status.get("active_app", "?")
                            post_st = post_status.get("status", "?")
                            post_prompt = post_status.get("is_at_prompt", "?")
                            log(f"[{cond_name}] After submit: app={post_app} status={post_st} at_prompt={post_prompt}")
                        except Exception:
                            pass
                    except Exception as e:
                        log(f"[{cond_name}] ERROR sending prompt: {e}")
                        log(traceback.format_exc())
                else:
                    log(f"[{cond_name}] No prompt file specified")
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
                # Claude shows ❯ prompt when done (waitingForInput with is_at_prompt or idle)
                if app == "Claude" and at_prompt:
                    completed.add(cond_name)
                    log(f"[{cond_name}] Completed (round {poll_round})")
                elif st == "idle" and app != "Claude":
                    # Claude exited
                    completed.add(cond_name)
                    log(f"[{cond_name}] Claude exited (round {poll_round})")
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

    # Phase: Collect — gather session data using tracked session files
    _run_state["currentPhase"] = "collecting"
    log("Collecting results...")
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
        duration_str = "-"
        if first_user_ts and last_assistant_ts:
            try:
                from datetime import datetime as dt
                t1 = dt.fromisoformat(first_user_ts.replace("Z", "+00:00"))
                t2 = dt.fromisoformat(last_assistant_ts.replace("Z", "+00:00"))
                secs = (t2 - t1).total_seconds()
                duration_str = f"{secs:.0f}s" if secs < 60 else f"{secs/60:.1f}m"
            except Exception:
                pass

        return {
            "input_tokens": input_tok, "output_tokens": output_tok,
            "cache_read": cache_read, "cache_create": cache_create,
            "turns": turns, "tool_calls": total_tools, "tool_names": tool_names,
            "total_tokens": total_tok, "cost": round(cost, 4),
            "duration": duration_str, "output": final_output, "prompt": prompt_text,
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

            from db import insert_result
            from datetime import datetime, timezone
            cto = "off" if cond_name == "control-cto-off" else "on"
            tool_breakdown_json = json.dumps(
                {k: v for k, v in sorted(data["tool_names"].items(), key=lambda x: -x[1]) if k != "Agent"}
            )
            result_id = f"{run_id}-{cond_name}"
            insert_result({
                "id": result_id,
                "runId": run_id,
                "date": datetime.now(timezone.utc).isoformat()[:19],
                "evalType": req.evalType,
                "target": req.target,
                "model": req.model,
                "condition": cond_name,
                "reasoning": req.reasoning,
                "cto": cto,
                "score": 0,
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
                "toolBreakdown": tool_breakdown_json,
            })
            log(f"[{cond_name}] Stored: {result_id} (output: {len(data.get('output',''))} chars)")
        except Exception as e:
            log(f"[{cond_name}] ERROR collecting: {e}")
            import traceback
            log(traceback.format_exc())

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
    background_tasks.add_task(_run_eval_background, plan, req)

    return {
        "status": "running",
        "plan": plan,
        "currentPhase": "launch",
        "log": ["Plan generated. Launching agents via Chau7 MCP..."],
        "error": None,
    }


@app.get("/api/run/status")
async def run_status() -> dict[str, Any]:
    return _run_state
