"""Pretraining-leakage cold-probe for eval scenarios.

A cold-probe runs the subject model with the task prompt and repo identity
only — no tools, no repo access, no file tree — then judges the candidate
against the same reference used by the full eval. A high cold score means
the model already knows the answer; the eval can't discriminate on that
item.

Design:
  - Subject runs via Chau7 tab in a NEUTRAL empty directory so the model
    has no repo files to discover. Tools are disabled explicitly on
    Claude; on Codex we rely on the empty cwd + schema-forced output.
  - Judging reuses `llm_judge.score_with_judge` with a leakage-specific
    instructions string (passed via the `instructions` override).
  - Results are cached at
    eval-runs/leakage-cache/<model>/<eval_type>/<scenario>.json keyed by
    (model_version, task_text_hash, reference_hash, PROBE_PROMPT_VERSION).
    Prompt edits bump PROBE_PROMPT_VERSION to invalidate stale cache.

Fails soft: any step returning an error yields `leakage_score_cold=None`
and a populated `error` field. An eval never blocks on a cold-probe.
"""

from __future__ import annotations

import hashlib
import json
import shlex
import tempfile
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import mcp_client
from llm_judge import score_with_judge


PROBE_PROMPT_VERSION = "v0.1"

SUBJECT_RUN_TIMEOUT_S = 240
OUTPUT_POLL_S = 2

_CACHE_ROOT = (
    Path(__file__).resolve().parent.parent.parent
    / "aethyme"
    / "eval-runs"
    / "leakage-cache"
)


COLD_PROBE_INSTRUCTIONS = """You are participating in a pretraining-leakage diagnostic. You will be given a code analysis task and a repository name. You have NO access to the repository: no file reads, no tools, no search. You must answer from your training knowledge alone.

Instructions:
- Produce a best-guess answer even when uncertain. Do not refuse.
- Base your answer on what you recall from public training data about this repository.
- For each field in the output schema, commit to a concrete value.
- When you are not confident in a specific value, still provide your best guess AND append "(low-confidence)" to that value so downstream scoring can see which items were guessed versus recalled.
- Do not describe your reasoning process. Return only the JSON object matching the required schema."""


COLD_PROBE_USER_TEMPLATE = """Task: {task}

Repository: {repo_name}

You have no repository access, no file reads, and no tools. Answer from your training knowledge only. Return a JSON object matching the required output schema, with "(low-confidence)" appended to any value you are guessing rather than recalling."""


LEAKAGE_JUDGE_INSTRUCTIONS = """You are measuring PRETRAINING LEAKAGE on a code analysis task. The CANDIDATE was produced by a model with NO repository access — only the task text and the repository name. Your job is to decide how much the candidate matches the REFERENCE.

A HIGH score means the model already knew the answer. A LOW score means the model did not know and produced a generic or wrong guess.

Match criterion: any STRUCTURALLY CORRECT answer counts as a match, even when the candidate uses different identifier names than the reference, as long as the semantic shape of the answer is right (right kind of file, right kind of root cause, right conclusion). Do not require exact identifier recall. Do not require identical phrasing.

Score bands (0-100):
- 0-20: Wrong, empty, or generic. No recall of this specific repository's content or conventions.
- 21-40: Plausible structure; correct repository domain but no match to the reference's specific answer shape.
- 41-60: Some elements of the reference's answer are semantically present (roughly the right kind of component, or the right general area).
- 61-80: Substantial semantic overlap: the candidate produced the right shape of answer — right kind of file, right kind of root cause, right conclusion — even with different names than the reference.
- 81-100: Full semantic reproduction. The candidate's answer is equivalent to the reference in substance. Clear memorization signal.

Ignore "(low-confidence)" markers in the candidate when scoring content match — they are for downstream analysis, not for this rubric. Also ignore formatting differences, path styles, and phrasing variation. Judge whether the candidate's substance matches the reference's substance.

Return a JSON object matching the required schema. In `key_matches`, list the semantic elements of the reference the candidate correctly produced. In `key_misses`, list reference elements the candidate missed or got wrong."""


# Below this → clean (counts in headline). At or above → contaminated.
# 0.70 permissive: only full semantic reproduction flags as contaminated.
LEAKAGE_CLEAN_THRESHOLD = 0.70


_CLAUDE_DISALLOWED_COLD_TOOLS = (
    "Bash,Edit,Write,Read,Grep,Glob,WebFetch,WebSearch,"
    "NotebookEdit,Task,TodoWrite,EnterPlanMode,ExitPlanMode"
)


@dataclass
class LeakageResult:
    leakage_score_cold: float | None
    raw_judge_score: float | None
    cold_candidate: str | None
    probe_prompt_version: str
    model: str
    eval_type: str
    scenario: str
    is_clean: bool | None
    cache_hit: bool
    elapsed_seconds: float
    error: str | None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _cache_key(task: str, reference: dict[str, Any] | str) -> str:
    h = hashlib.sha256()
    h.update(task.encode("utf-8"))
    ref_blob = (
        json.dumps(reference, sort_keys=True)
        if isinstance(reference, dict)
        else str(reference)
    ).encode("utf-8")
    h.update(ref_blob)
    h.update(PROBE_PROMPT_VERSION.encode("utf-8"))
    return h.hexdigest()[:16]


def _cache_path(model: str, eval_type: str, scenario: str, key: str) -> Path:
    safe_model = model.replace("/", "_")
    safe_scenario = (scenario or "default").replace("/", "_")
    return _CACHE_ROOT / safe_model / eval_type / f"{safe_scenario}.{key}.json"


def _load_cached(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None


def _store_cached(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".partial")
    tmp.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    tmp.replace(path)


def _neutral_tmpdir() -> Path:
    return Path(tempfile.mkdtemp(prefix="aethyme-cold-probe-"))


def _build_claude_cold_command(
    prompt_path: Path, output_path: Path, schema_path: Path, *, model: str,
) -> str:
    parts = [
        "claude",
        "-p",
        "--dangerously-skip-permissions",
        "--model", shlex.quote(model),
        "--output-format", "json",
        "--json-schema",
        f'"$(cat {shlex.quote(str(schema_path))})"',
        "--disallowedTools", shlex.quote(_CLAUDE_DISALLOWED_COLD_TOOLS),
    ]
    partial = shlex.quote(str(output_path) + ".partial")
    final = shlex.quote(str(output_path))
    return (
        " ".join(parts)
        + f" < {shlex.quote(str(prompt_path))}"
        + f" > {partial} && mv {partial} {final}"
    )


def _build_codex_cold_command(
    prompt_path: Path, output_path: Path, schema_path: Path,
) -> str:
    parts = [
        "codex", "exec",
        "--sandbox", "read-only",
        "--output-schema", shlex.quote(str(schema_path)),
        "--output-last-message", shlex.quote(str(output_path)),
        "-",
    ]
    return " ".join(parts) + f" < {shlex.quote(str(prompt_path))}"


def _parse_claude_cold(raw: str) -> str:
    events = json.loads(raw)
    if isinstance(events, list):
        for ev in reversed(events):
            if isinstance(ev, dict) and ev.get("type") == "result":
                so = ev.get("structured_output")
                if isinstance(so, dict):
                    return json.dumps(so)
                text = ev.get("result")
                if isinstance(text, str) and text.strip():
                    return text
                break
    if isinstance(events, dict):
        return json.dumps(events)
    raise ValueError("claude cold output had no result event")


def _wait_for_file(path: Path, timeout_s: int) -> bool:
    elapsed = 0
    while elapsed < timeout_s:
        if path.exists():
            time.sleep(0.2)
            return True
        time.sleep(OUTPUT_POLL_S)
        elapsed += OUTPUT_POLL_S
    return False


def _run_cold_subject(
    model: str,
    backend: str,
    task: str,
    repo_name: str,
    output_schema: dict[str, Any],
    *,
    log: Any = None,
) -> tuple[str, str | None]:
    def _log(msg: str) -> None:
        if callable(log):
            log(msg)

    if not mcp_client.is_available():
        return "", "Chau7 MCP not available"

    workdir = _neutral_tmpdir()
    prompt_path = workdir / "prompt.txt"
    schema_path = workdir / "schema.json"
    output_path = workdir / "out.json"

    user_msg = COLD_PROBE_USER_TEMPLATE.format(task=task, repo_name=repo_name)
    full_prompt = f"{COLD_PROBE_INSTRUCTIONS}\n\n{user_msg}"
    prompt_path.write_text(full_prompt, encoding="utf-8")
    schema_path.write_text(json.dumps(output_schema, indent=2), encoding="utf-8")

    if backend == "claude":
        cmd = _build_claude_cold_command(
            prompt_path, output_path, schema_path, model=model,
        )
    elif backend == "codex":
        cmd = _build_codex_cold_command(prompt_path, output_path, schema_path)
    else:
        return "", f"unknown cold-probe backend: {backend!r}"

    tab_id = None
    try:
        tab = mcp_client.tab_create(str(workdir))
        tab_id = tab.get("tab_id")
        if not tab_id:
            return "", f"tab_create failed: {tab}"
        _log(f"  cold-probe tab {tab_id} in {workdir}")
        time.sleep(3)

        exec_result = mcp_client.tab_exec(tab_id, cmd)
        if not exec_result.get("ok"):
            return "", f"tab_exec failed: {exec_result.get('error', exec_result)}"

        if not _wait_for_file(output_path, SUBJECT_RUN_TIMEOUT_S):
            return "", f"cold-probe subject timed out after {SUBJECT_RUN_TIMEOUT_S}s"

        raw = output_path.read_text(encoding="utf-8").strip()
        if not raw:
            return "", "cold-probe output was empty"

        if backend == "claude":
            candidate = _parse_claude_cold(raw)
        else:
            candidate = raw
        return candidate, None
    except Exception as e:
        return "", f"{type(e).__name__}: {e}"
    finally:
        if tab_id:
            try:
                mcp_client.tab_close(tab_id, force=True)
            except Exception:
                pass


def cold_probe(
    *,
    model: str,
    backend: str,
    eval_type: str,
    scenario: str,
    task: str,
    reference: dict[str, Any] | str,
    repo_name: str,
    output_schema: dict[str, Any],
    rubric: dict[str, Any] | None = None,
    judge_backend: str = "claude-haiku",
    judge_samples: int = 1,
    tab_directory: str,
    log: Any = None,
    force: bool = False,
) -> LeakageResult:
    """Measure pretraining leakage for (model, eval_type, scenario).

    Returns a `LeakageResult`. Never raises — errors populate `error`
    and `leakage_score_cold` / `is_clean` are None.
    """
    start = time.monotonic()
    key = _cache_key(task, reference)
    cache_path = _cache_path(model, eval_type, scenario, key)

    if not force:
        cached = _load_cached(cache_path)
        if cached and cached.get("probe_prompt_version") == PROBE_PROMPT_VERSION:
            cached["cache_hit"] = True
            cached["elapsed_seconds"] = 0.0
            return LeakageResult(**cached)

    def _log(msg: str) -> None:
        if callable(log):
            log(msg)

    _log(f"cold-probe: model={model} backend={backend} eval_type={eval_type} scenario={scenario}")

    candidate, subj_err = _run_cold_subject(
        model=model,
        backend=backend,
        task=task,
        repo_name=repo_name,
        output_schema=output_schema,
        log=log,
    )
    if subj_err or not candidate:
        return LeakageResult(
            leakage_score_cold=None,
            raw_judge_score=None,
            cold_candidate=None,
            probe_prompt_version=PROBE_PROMPT_VERSION,
            model=model,
            eval_type=eval_type,
            scenario=scenario,
            is_clean=None,
            cache_hit=False,
            elapsed_seconds=round(time.monotonic() - start, 2),
            error=subj_err or "empty cold-probe candidate",
        )

    judge_out = score_with_judge(
        task=task,
        reference=reference,
        candidate=candidate,
        rubric=rubric,
        samples=judge_samples,
        tab_directory=tab_directory,
        backend=judge_backend,
        log=log,
        instructions=LEAKAGE_JUDGE_INSTRUCTIONS,
    )

    judge_err = judge_out.get("error")
    raw_score = judge_out.get("mean_score")
    if judge_err or raw_score is None:
        return LeakageResult(
            leakage_score_cold=None,
            raw_judge_score=None,
            cold_candidate=candidate[:4000],
            probe_prompt_version=PROBE_PROMPT_VERSION,
            model=model,
            eval_type=eval_type,
            scenario=scenario,
            is_clean=None,
            cache_hit=False,
            elapsed_seconds=round(time.monotonic() - start, 2),
            error=f"judge failed: {judge_err}",
        )

    leakage_norm = round(float(raw_score) / 100.0, 3)
    is_clean = leakage_norm < LEAKAGE_CLEAN_THRESHOLD

    result = LeakageResult(
        leakage_score_cold=leakage_norm,
        raw_judge_score=float(raw_score),
        cold_candidate=candidate[:4000],
        probe_prompt_version=PROBE_PROMPT_VERSION,
        model=model,
        eval_type=eval_type,
        scenario=scenario,
        is_clean=is_clean,
        cache_hit=False,
        elapsed_seconds=round(time.monotonic() - start, 2),
        error=None,
    )

    _store_cached(cache_path, result.to_dict())
    return result


def _repo_identity_from_target_dict(target_dict: dict[str, Any]) -> str:
    """Derive a human/model-meaningful repo name from a TARGETS entry.

    Prefers the public GitHub slug (from `setup_source`) when available,
    so the subject model can recognize the repo from training. Falls
    back to `display_name` for private targets (where no recognition
    is expected — which is exactly the right signal).
    """
    src = target_dict.get("setup_source") or ""
    if "github.com/" in src:
        tail = src.split("github.com/", 1)[1]
        tail = tail.removesuffix(".git").rstrip("/")
        parts = tail.split("/")
        if len(parts) >= 2:
            return "/".join(parts[:2])
    return target_dict.get("display_name") or target_dict.get("name") or "unknown"


def cold_probe_for_eval_type(
    *,
    model: str,
    backend: str,
    eval_type: str,
    scenario: str,
    task: str,
    target_dict: dict[str, Any],
    aethyme_pkg_path: str,
    tab_directory: str,
    judge_backend: str = "claude-haiku",
    judge_samples: int = 1,
    log: Any = None,
    force: bool = False,
) -> LeakageResult:
    """Load reference/schema/rubric from schemas.py by eval_type, then probe.

    Mirrors the lookup strategy used by `llm_judge.score_eval_type_with_judge`:
    tries `mediawiki_<key>_reference` then falls back to `<key>_reference`.
    """
    import sys
    if aethyme_pkg_path not in sys.path:
        sys.path.insert(0, aethyme_pkg_path)

    key = eval_type.replace("-", "_")

    try:
        from src.eval import schemas  # type: ignore
    except Exception as e:
        return LeakageResult(
            leakage_score_cold=None, raw_judge_score=None, cold_candidate=None,
            probe_prompt_version=PROBE_PROMPT_VERSION, model=model,
            eval_type=eval_type, scenario=scenario, is_clean=None,
            cache_hit=False, elapsed_seconds=0.0,
            error=f"could not import schemas: {e}",
        )

    def _try(name_stem: str) -> Any | None:
        for prefix in (f"mediawiki_{name_stem}", name_stem):
            fn = getattr(schemas, prefix, None)
            if callable(fn):
                try:
                    return fn()
                except Exception:
                    return None
        return None

    reference = _try(f"{key}_reference")
    output_schema = _try(f"{key}_output_schema")
    rubric = _try(f"{key}_scoring_rubric")

    if reference is None or output_schema is None:
        return LeakageResult(
            leakage_score_cold=None, raw_judge_score=None, cold_candidate=None,
            probe_prompt_version=PROBE_PROMPT_VERSION, model=model,
            eval_type=eval_type, scenario=scenario, is_clean=None,
            cache_hit=False, elapsed_seconds=0.0,
            error=(
                f"no reference/schema for {eval_type}: "
                f"reference={reference is not None}, schema={output_schema is not None}"
            ),
        )

    repo_name = _repo_identity_from_target_dict(target_dict)

    return cold_probe(
        model=model,
        backend=backend,
        eval_type=eval_type,
        scenario=scenario,
        task=task,
        reference=reference,
        repo_name=repo_name,
        output_schema=output_schema,
        rubric=rubric,
        judge_backend=judge_backend,
        judge_samples=judge_samples,
        tab_directory=tab_directory,
        log=log,
        force=force,
    )
