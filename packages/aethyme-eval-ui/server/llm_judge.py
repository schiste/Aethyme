"""LLM-as-judge scoring via Chau7 MCP + a pluggable CLI backend.

All AI invocations go through Chau7 -> tab -> (codex|claude) CLI. No direct
API calls. Two backends are supported:

  - "codex"        : Codex CLI with --output-schema + --output-last-message.
                     Legacy default.
  - "claude-haiku" : Claude Code CLI with --json-schema + --output-format json.
                     Output parsed from stdout (captured via shell redirect
                     to a file). Runs with --bare + --disallowedTools so the
                     judge has no tools and no CLAUDE.md context — a minimal
                     sandbox. This is the self-preference-bias mitigation:
                     a judge in a different model family from the agents
                     under evaluation (Stureborg 2024, Panickssery 2024).

Intra-rater reliability: the same (task, reference, candidate) triple is
judged N times; we return the mean + stdev across samples. A judge that
scores the same input as 75/78/76 is consistent (stdev ~= 1.2); 40/80/60
is not (stdev ~= 16) and gets flagged unreliable.

Degrades gracefully: every failure path returns `{..., error: "..."}` with
empty samples. An eval run never fails because the judge is misconfigured,
timing out, or the CLI is missing.
"""

from __future__ import annotations

import json
import shlex
import statistics
import tempfile
import time
from pathlib import Path
from typing import Any

# Must go through Chau7 MCP. No anthropic/openai SDK imports.
import mcp_client


DEFAULT_SAMPLES = 3
MAX_OUTPUT_CHARS = 24_000
# Poll cadence for the output file the CLI writes.
# Codex tabs keep reporting status="running" / is_at_prompt=True for their entire
# run, so tab_status is NOT a reliable completion signal. The output file's
# appearance is atomic and trustworthy — that's our signal.
OUTPUT_POLL_S = 2
JUDGE_RUN_TIMEOUT_S = 300  # up to 5 min per sample (large candidates + CLI latency)

# Supported judge backends. Kept as a module constant so callers (main.py,
# calibration_check) can surface the list without import cycles.
JUDGE_BACKENDS = ("codex", "claude-haiku")
DEFAULT_BACKEND = "codex"  # Flipped to claude-haiku after calibration back-test.


JUDGE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["score", "rationale", "key_matches", "key_misses"],
    "properties": {
        "score": {"type": "integer", "minimum": 0, "maximum": 100},
        "rationale": {"type": "string"},
        "key_matches": {"type": "array", "items": {"type": "string"}},
        "key_misses": {"type": "array", "items": {"type": "string"}},
    },
}


JUDGE_INSTRUCTIONS = """You are a strict but fair evaluator of AI agent outputs on code navigation and analysis tasks.

Score the CANDIDATE from 0 to 100 based on how closely it matches the REFERENCE in substance:
- 0-20: Wrong, irrelevant, or empty. Did not engage with the task.
- 21-40: Partially on-topic but missing key elements.
- 41-60: Some correct elements, significant gaps or errors.
- 61-80: Most correct elements present; minor gaps.
- 81-100: Comprehensive and accurate; matches reference in substance.

Judge SUBSTANCE, not format. Plain prose that identifies the right files and explains the right root cause scores as high as text using the exact reference keywords. Penalize missing files, wrong root causes, and hallucinated details - NOT different phrasing.

The CANDIDATE may contain terminal noise, tool output, and prompt echoes. Focus on the agent's actual analysis and conclusions. Ignore tool invocations and raw file dumps except insofar as they reveal what the agent found.

Return a JSON object matching the required schema exactly."""


def _build_judge_prompt(
    task: str,
    reference: dict[str, Any] | str,
    rubric: dict[str, Any] | None,
    candidate: str,
) -> str:
    ref_text = (
        json.dumps(reference, indent=2) if isinstance(reference, dict) else str(reference)
    )
    rubric_text = (
        json.dumps(rubric, indent=2) if rubric else "(use the scoring guidance)"
    )

    truncated_candidate = candidate
    truncation_note = ""
    if len(candidate) > MAX_OUTPUT_CHARS:
        half = MAX_OUTPUT_CHARS // 2
        truncated_candidate = (
            f"{candidate[:half]}\n\n"
            f"[...TRUNCATED {len(candidate) - MAX_OUTPUT_CHARS} CHARS...]\n\n"
            f"{candidate[-half:]}"
        )
        truncation_note = (
            f"\n\nNote: candidate truncated from {len(candidate)} to "
            f"{MAX_OUTPUT_CHARS} chars (head + tail preserved)."
        )

    return (
        f"{JUDGE_INSTRUCTIONS}\n\n"
        f"TASK:\n{task}\n\n"
        f"REFERENCE (ground truth):\n{ref_text}\n\n"
        f"RUBRIC:\n{rubric_text}\n\n"
        f"CANDIDATE (agent's output):\n{truncated_candidate}{truncation_note}\n\n"
        "Return the JSON object per the required schema."
    )


def _wait_for_output_file(output_path: Path, timeout_s: int) -> bool:
    """Poll until the codex `--output-last-message` file appears.

    We used to poll tab_status here, but codex tabs keep reporting
    status=running / is_at_prompt=True for their entire execution,
    so the tab state is not a reliable completion signal. The output
    file being written is atomic and trustworthy.
    """
    elapsed = 0
    while elapsed < timeout_s:
        if output_path.exists():
            # Small safety pause to avoid reading a half-written file
            time.sleep(0.2)
            return True
        time.sleep(OUTPUT_POLL_S)
        elapsed += OUTPUT_POLL_S
    return False


def _build_codex_command(
    prompt_path: Path, output_path: Path, schema_path: Path
) -> str:
    """Build the shell command that runs Codex CLI on the judge prompt.

    Paths are tempfile-generated (no user input), but quoted via shlex for safety.
    """
    parts = [
        "codex",
        "exec",
        "--sandbox",
        "workspace-write",
        "--output-schema",
        shlex.quote(str(schema_path)),
        "--output-last-message",
        shlex.quote(str(output_path)),
        "-",
    ]
    return " ".join(parts) + f" < {shlex.quote(str(prompt_path))}"


# Tools we disallow for the Claude judge: every code-touching tool, every
# network tool. The judge should produce JSON from text, nothing else. Listed
# explicitly rather than wildcarded because claude's CLI whitelist semantics
# ignore unknown names silently — enumeration is the safer form.
_CLAUDE_DISALLOWED_TOOLS = (
    "Bash,Edit,Write,Read,Grep,Glob,WebFetch,WebSearch,"
    "NotebookEdit,Task,TodoWrite,EnterPlanMode,ExitPlanMode"
)


def _build_claude_command(
    prompt_path: Path, output_path: Path, schema_path: Path,
    *, model: str = "haiku",
) -> str:
    """Build the shell command that runs Claude Code CLI on the judge prompt.

    Key differences from codex:
      - Claude writes structured output to stdout (not a dedicated file flag),
        so we redirect stdout to output_path.
      - `--bare` strips hooks, LSP, plugin sync, MCP, and CLAUDE.md autoload.
        Without it, the judge would inherit ~70k cache tokens of project
        context before seeing the first byte of the judging prompt.
      - `--disallowedTools` blocks every code/network tool. The judge must
        produce JSON from text — nothing else.
      - `--json-schema` inlines the schema (claude reads it from the flag
        value, not a path), so we pass the file content inlined with `$(cat)`.
    """
    parts = [
        "claude",
        "-p",
        "--bare",
        "--dangerously-skip-permissions",
        "--model", shlex.quote(model),
        "--output-format", "json",
        "--json-schema",
        # Inline the schema content via $(cat). The schema file is tempfile-
        # generated so the shell substitution is safe.
        f'"$(cat {shlex.quote(str(schema_path))})"',
        "--disallowedTools", shlex.quote(_CLAUDE_DISALLOWED_TOOLS),
    ]
    # stdin = prompt, stdout = output file. Claude's -p mode reads prompt
    # from stdin when no positional prompt argument is given.
    return (
        " ".join(parts)
        + f" < {shlex.quote(str(prompt_path))}"
        + f" > {shlex.quote(str(output_path))}"
    )


def _parse_codex_output(raw: str) -> dict[str, Any]:
    """Parse codex --output-last-message: a single JSON object matching JUDGE_SCHEMA."""
    return json.loads(raw)


def _parse_claude_output(raw: str) -> dict[str, Any]:
    """Parse claude --output-format json: a JSON array of events.

    The schema-validated payload lives in the `result` event's
    `structured_output` field. Failing that (schema validation off / skipped),
    fall back to parsing the `result` field's text as JSON directly.
    """
    # Claude returns a JSON array of events; extract the terminal result event.
    events = json.loads(raw)
    if isinstance(events, list):
        for ev in reversed(events):
            if isinstance(ev, dict) and ev.get("type") == "result":
                so = ev.get("structured_output")
                if isinstance(so, dict):
                    return so
                # Fallback: parse the result text as JSON.
                text = ev.get("result")
                if isinstance(text, str) and text.strip():
                    return json.loads(text)
                break
        raise ValueError("claude output had no result event with structured_output")
    # If someone passed --output-format text (shouldn't happen), try direct parse.
    if isinstance(events, dict):
        return events
    raise ValueError(f"unexpected claude output shape: {type(events).__name__}")


def _build_command(
    backend: str, prompt_path: Path, output_path: Path, schema_path: Path
) -> str:
    if backend == "codex":
        return _build_codex_command(prompt_path, output_path, schema_path)
    if backend == "claude-haiku":
        return _build_claude_command(
            prompt_path, output_path, schema_path, model="haiku"
        )
    raise ValueError(
        f"unknown judge backend: {backend!r} (expected one of {JUDGE_BACKENDS})"
    )


def _parse_output(backend: str, raw: str) -> dict[str, Any]:
    if backend == "codex":
        return _parse_codex_output(raw)
    if backend == "claude-haiku":
        return _parse_claude_output(raw)
    raise ValueError(f"unknown judge backend: {backend!r}")


def _run_judge_once(
    tab_id: str,
    prompt_path: Path,
    output_path: Path,
    schema_path: Path,
    *,
    backend: str = DEFAULT_BACKEND,
) -> dict[str, Any]:
    """Run one judge call inside tab_id, parse the JSON output file.

    Raises on any failure: non-zero exit, timeout, missing output, invalid JSON.
    """
    output_path.unlink(missing_ok=True)

    cmd = _build_command(backend, prompt_path, output_path, schema_path)
    exec_result = mcp_client.tab_exec(tab_id, cmd)
    if not exec_result.get("ok"):
        raise RuntimeError(
            f"tab_exec failed: {exec_result.get('error', exec_result)}"
        )

    if not _wait_for_output_file(output_path, JUDGE_RUN_TIMEOUT_S):
        raise TimeoutError(
            f"{backend} did not write output within {JUDGE_RUN_TIMEOUT_S}s"
        )

    raw = output_path.read_text(encoding="utf-8").strip()
    if not raw:
        raise ValueError("output file was empty")

    try:
        parsed = _parse_output(backend, raw)
    except (json.JSONDecodeError, ValueError) as e:
        raise ValueError(f"output was not valid JSON: {e}; raw={raw[:300]}")

    score = parsed.get("score")
    if not isinstance(score, (int, float)) or not 0 <= score <= 100:
        raise ValueError(f"invalid score in output: {score!r}")

    return {
        "score": float(score),
        "rationale": str(parsed.get("rationale", ""))[:500],
        "key_matches": [str(s)[:120] for s in parsed.get("key_matches", [])][:20],
        "key_misses": [str(s)[:120] for s in parsed.get("key_misses", [])][:20],
    }


def score_with_judge(
    task: str,
    reference: dict[str, Any] | str,
    candidate: str,
    *,
    rubric: dict[str, Any] | None = None,
    samples: int = DEFAULT_SAMPLES,
    tab_directory: str,
    backend: str = DEFAULT_BACKEND,
    log: Any = None,
) -> dict[str, Any]:
    """Score a candidate via a CLI-based judge in a Chau7 tab, N samples (intra-rater).

    Returns:
      {
        "mean_score": float 0-100,
        "stdev": float,
        "samples": [ {score, rationale, key_matches, key_misses}, ... ],
        "backend": "codex" | "claude-haiku",
        "reliable": bool (stdev < 10 = consistent judge),
        "error": optional error string,
      }

    `backend` picks the judge CLI (see JUDGE_BACKENDS). Swapping backends is
    the primary self-preference-bias mitigation: Codex agents should not be
    judged by Codex — use claude-haiku instead.

    `tab_directory` is the cwd for the Chau7 tab. Use a neutral directory
    (NOT a playground Control repo — those must stay untouched).

    Never raises.
    """
    def _log(msg: str) -> None:
        if callable(log):
            log(msg)

    if backend not in JUDGE_BACKENDS:
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": f"unknown backend {backend!r}; expected one of {JUDGE_BACKENDS}",
        }

    if not mcp_client.is_available():
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": "Chau7 MCP not available",
        }

    if not candidate or len(candidate) < 50:
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": "candidate too short to judge",
        }

    tmpdir = Path(tempfile.mkdtemp(prefix="aethyme-judge-"))
    prompt_path = tmpdir / "prompt.txt"
    schema_path = tmpdir / "schema.json"
    try:
        prompt_path.write_text(
            _build_judge_prompt(task, reference, rubric, candidate),
            encoding="utf-8",
        )
        schema_path.write_text(
            json.dumps(JUDGE_SCHEMA, indent=2), encoding="utf-8"
        )
    except Exception as e:
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": f"failed to write judge artifacts: {e}",
        }

    # One tab per sample. Codex keeps the tab in status="running" even after
    # writing its output file, so reusing the same tab for sequential samples
    # is fragile (next tab_exec may queue or interrupt). Fresh tabs per sample
    # are cheap (~3s each) and deterministic.
    sample_results: list[dict[str, Any]] = []
    errors: list[str] = []
    judge_start = time.monotonic()  # P9: measure total judge overhead

    for i in range(samples):
        output_path = tmpdir / f"out-{i + 1}.json"
        tab_id = None
        try:
            tab = mcp_client.tab_create(tab_directory)
            tab_id = tab.get("tab_id")
            if not tab_id:
                raise RuntimeError(f"tab_create returned no tab_id: {tab}")
            _log(f"  sample {i + 1}/{samples} [{backend}]: judge tab {tab_id} opened")

            # Shell bootstrap pause
            time.sleep(3)

            result = _run_judge_once(
                tab_id, prompt_path, output_path, schema_path, backend=backend
            )
            sample_results.append(result)
            _log(f"  sample {i + 1}/{samples}: score={result['score']:.0f}")
        except Exception as e:
            err = f"sample {i + 1}: {type(e).__name__}: {e}"
            errors.append(err)
            _log(f"  {err}")
        finally:
            if tab_id:
                try:
                    mcp_client.tab_close(tab_id, force=True)
                except Exception as close_err:
                    errors.append(f"sample {i + 1} tab_close: {close_err}")

    elapsed_seconds = round(time.monotonic() - judge_start, 2)

    if not sample_results:
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": "; ".join(errors) or "all judge calls failed",
            "elapsed_seconds": elapsed_seconds,
        }

    scores = [s["score"] for s in sample_results]
    mean_score = statistics.mean(scores)
    stdev = statistics.stdev(scores) if len(scores) > 1 else 0.0

    return {
        "mean_score": round(mean_score, 1),
        "stdev": round(stdev, 2),
        "samples": sample_results,
        "backend": backend,
        "reliable": stdev < 10.0,
        "error": "; ".join(errors) if errors else None,
        "elapsed_seconds": elapsed_seconds,
    }


def score_eval_type_with_judge(
    eval_type: str,
    task: str,
    candidate: str,
    *,
    aethyme_pkg_path: str,
    tab_directory: str,
    samples: int = DEFAULT_SAMPLES,
    backend: str = DEFAULT_BACKEND,
    log: Any = None,
) -> dict[str, Any]:
    """Judge an output for a specific eval type, loading reference + rubric from schemas.py.

    Returns the same shape as score_with_judge() plus `eval_type` and `reference_loaded`.
    """
    import sys
    sys.path.insert(0, aethyme_pkg_path)

    reference: dict[str, Any] | str = ""
    rubric: dict[str, Any] | None = None
    reference_loaded = False

    try:
        from src.eval import schemas  # type: ignore

        ref_func = getattr(
            schemas, f"mediawiki_{eval_type.replace('-', '_')}_reference", None
        )
        rubric_func = getattr(
            schemas, f"mediawiki_{eval_type.replace('-', '_')}_scoring_rubric", None
        )

        if ref_func is None:
            ref_func = getattr(
                schemas, f"{eval_type.replace('-', '_')}_reference", None
            )
            rubric_func = getattr(
                schemas, f"{eval_type.replace('-', '_')}_scoring_rubric", None
            )

        if ref_func is not None:
            reference = ref_func()
            reference_loaded = True
        if rubric_func is not None:
            rubric = rubric_func()
    except Exception as e:
        return {
            "mean_score": 0.0, "stdev": 0.0, "samples": [],
            "backend": backend, "reliable": False,
            "error": f"could not load reference/rubric for {eval_type}: {e}",
            "eval_type": eval_type,
            "reference_loaded": False,
        }

    result = score_with_judge(
        task=task,
        reference=reference,
        candidate=candidate,
        rubric=rubric,
        samples=samples,
        tab_directory=tab_directory,
        backend=backend,
        log=log,
    )
    result["eval_type"] = eval_type
    result["reference_loaded"] = reference_loaded
    return result
