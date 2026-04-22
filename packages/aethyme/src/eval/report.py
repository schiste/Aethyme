"""Evaluation report helpers for local-first Aethyme benchmarks."""

from __future__ import annotations

import json
import math
import re
import subprocess
from collections import Counter
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from src.contracts.eval_artifacts import make_eval_artifact_envelope
from src.contracts.versions import (
    RUN_METADATA_SCHEMA_VERSION,
    contract_versions,
)

from .runner import EvaluationRunResult

REPORTS_ROOT = Path(__file__).resolve().parents[2] / "docs" / "reports" / "evals"
EVAL_RUNS_ROOT = Path(__file__).resolve().parents[2] / "eval-runs"
RUNS_LEDGER = EVAL_RUNS_ROOT / "runs.jsonl"

CONDITION_ORDER = ("control-cto-off", "control-cto-on", "explore", "leverage", "task-conditioned")
# Legacy 3-condition order for backward compat with old result dicts.
_LEGACY_CONDITION_ORDER = ("control", "explore", "leverage")


# All known condition names in canonical display order.
_ALL_KNOWN_CONDITIONS = (
    "control-cto-off", "control-cto-on", "control", "explore", "leverage", "task-conditioned",
)

_RELATIVE_SCORE_WEIGHTS = {
    "quality_delta_multiplier": 1.0,
    "tokens_log_multiplier": 10.0,
    "duration_log_multiplier": 10.0,
    "cost_log_multiplier": 5.0,
}


def _active_conditions(result: dict[str, Any]) -> tuple[str, ...]:
    """Return condition names present in *result*, in canonical order.

    Handles both the 5-condition design (control-cto-off, control-cto-on,
    explore, leverage, task-conditioned) and the legacy 3-condition design
    (control, explore, leverage).
    """
    return tuple(c for c in _ALL_KNOWN_CONDITIONS if result.get(c))


@dataclass(frozen=True)
class EvaluationReport:
    task: str
    condition_prompt_chars: dict[str, int] = field(default_factory=dict)
    navigation_items: int = 0
    risk_items: int = 0
    condition_runs: dict[str, dict[str, Any] | None] = field(default_factory=dict)

    # Legacy convenience properties -----------------------------------------
    @property
    def baseline_prompt_chars(self) -> int:
        return self.condition_prompt_chars.get("control", 0)

    @property
    def aethyme_prompt_chars(self) -> int:
        return self.condition_prompt_chars.get("leverage", 0)

    @property
    def task_conditioned_prompt_chars(self) -> int:
        return self.condition_prompt_chars.get("task-conditioned", 0)

    @property
    def baseline_run(self) -> dict[str, Any] | None:
        return self.condition_runs.get("control")

    @property
    def aethyme_run(self) -> dict[str, Any] | None:
        return self.condition_runs.get("leverage")

    @property
    def task_conditioned_run(self) -> dict[str, Any] | None:
        return self.condition_runs.get("task-conditioned")

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "task": self.task,
            "condition_prompt_chars": self.condition_prompt_chars,
            "navigation_items": self.navigation_items,
            "risk_items": self.risk_items,
            "condition_runs": self.condition_runs,
            # Legacy keys for backward compat
            "baseline_prompt_chars": self.baseline_prompt_chars,
            "aethyme_prompt_chars": self.aethyme_prompt_chars,
            "task_conditioned_prompt_chars": self.task_conditioned_prompt_chars,
            "baseline_run": self.baseline_run,
            "aethyme_run": self.aethyme_run,
            "task_conditioned_run": self.task_conditioned_run,
        }
        return d


def estimate_report(
    task: str,
    # New API: dict-based
    prompts: dict[str, str] | None = None,
    runs: dict[str, EvaluationRunResult | None] | None = None,
    pack: dict[str, Any] | None = None,
    # Legacy positional/keyword API (kept for backward compat)
    baseline_prompt: str | None = None,
    aethyme_prompt: str | None = None,
    baseline_run: EvaluationRunResult | None = None,
    aethyme_run: EvaluationRunResult | None = None,
    # pack can also be passed as positional arg 4 in old call sites
    _legacy_pack: dict[str, Any] | None = None,
) -> EvaluationReport:
    """Create a local evaluation report from prompts, pack, and optional live runs."""
    # Reconcile legacy call patterns
    if prompts is None:
        prompts = {}
        if baseline_prompt is not None:
            prompts["control"] = baseline_prompt
        if aethyme_prompt is not None:
            prompts["leverage"] = aethyme_prompt

    if runs is None:
        runs = {}
        if baseline_run is not None:
            runs["control"] = baseline_run
        if aethyme_run is not None:
            runs["leverage"] = aethyme_run

    if pack is None:
        pack = _legacy_pack or {}

    condition_prompt_chars = {k: len(v) for k, v in prompts.items()}
    condition_runs_dict = {
        k: v.to_dict() if v else None for k, v in runs.items()
    }

    return EvaluationReport(
        task=task,
        condition_prompt_chars=condition_prompt_chars,
        navigation_items=len(pack.get("navigation_order", pack.get("scope", {}).get("navigation_order", []))),
        risk_items=len(pack.get("risk_flags", pack.get("scope", {}).get("risks", []))),
        condition_runs=condition_runs_dict,
    )


def get_aethyme_commit() -> str:
    """Return the current Aethyme HEAD commit hash, or 'unknown' on failure."""
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=Path(__file__).resolve().parents[3],
            text=True,
        ).strip()
    except Exception:
        return "unknown"


def create_eval_run_dir(
    repo_path: Path,
    eval_type: str,
    conditions: tuple[str, ...] | None = None,
    *,
    model: dict[str, str] | None = None,
    run_name: str | None = None,
) -> Path:
    """Create and return a timestamped eval run directory under eval-runs/.

    *conditions* overrides which condition subdirectories to create.
    Defaults to ``CONDITION_ORDER`` (5-condition design).

    *model* is an optional dict with keys like ``name``, ``provider``,
    ``reasoning``, ``backend`` — stored in metadata for reproducibility.
    """
    if run_name:
        run_dir = EVAL_RUNS_ROOT / run_name
    else:
        timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
        slug = _slugify(repo_path.name or "repo")
        run_dir = EVAL_RUNS_ROOT / f"{timestamp}-{slug}-{eval_type}"
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "artifacts").mkdir(exist_ok=True)
    (run_dir / "phases").mkdir(exist_ok=True)
    for cond in (conditions or CONDITION_ORDER):
        (run_dir / "conditions" / cond).mkdir(parents=True, exist_ok=True)
        (run_dir / "chau7" / cond).mkdir(parents=True, exist_ok=True)

    metadata: dict[str, Any] = {
        "schema_version": RUN_METADATA_SCHEMA_VERSION,
        "timestamp": datetime.now(UTC).isoformat(),
        "aethyme_commit": get_aethyme_commit(),
        "repo_path": str(repo_path),
        "eval_type": eval_type,
        "conditions": list(conditions or CONDITION_ORDER),
        "contract_versions": contract_versions(),
    }
    if model:
        metadata["model"] = model
    (run_dir / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    return run_dir


def write_eval_run_artifacts(run_dir: Path, result: dict[str, Any]) -> None:
    """Write all eval artifacts into the run directory structure."""
    artifacts = run_dir / "artifacts"
    artifacts.mkdir(parents=True, exist_ok=True)
    active = _active_conditions(result)

    # Per-condition prompts
    for cond in active:
        side = result.get(cond, {})
        if side and side.get("prompt"):
            (artifacts / f"{cond}-prompt.txt").write_text(side["prompt"], encoding="utf-8")

    # Shared artifacts
    for name, key in [
        ("output-schema.json", "output_schema"),
        ("scoring-rubric.json", "scoring_rubric"),
        ("reference-output.json", "reference_output"),
        ("navigation-context.json", "navigation_context"),
        ("pack.json", "pack"),
        ("task-spec.json", "task_spec"),
        ("challenge.json", "challenge"),
        ("signals.json", "signals"),
    ]:
        value = result.get(key)
        if value is not None:
            wrapped = make_eval_artifact_envelope(
                name.removesuffix(".json"),
                value,
                run_id=result.get("run_id"),
                eval_type=result.get("eval_type"),
                target=result.get("target"),
            )
            (artifacts / name).write_text(json.dumps(wrapped, indent=2), encoding="utf-8")

    # Per-condition results, assessments, and full run data
    for cond in active:
        side = result.get(cond, {})
        if not side:
            continue
        cond_dir = run_dir / "conditions" / cond
        cond_dir.mkdir(parents=True, exist_ok=True)

        run_data = side.get("run")
        if run_data and isinstance(run_data, dict):
            # Structured output
            structured = run_data.get("structured_output")
            if structured is not None:
                (cond_dir / "result.json").write_text(json.dumps(structured, indent=2), encoding="utf-8")
            # Full run record (includes stdout, stderr, tokens, etc.)
            (cond_dir / "run-record.json").write_text(json.dumps(run_data, indent=2, default=str), encoding="utf-8")
            # Raw stdout/stderr as separate files for easy inspection
            stdout = run_data.get("stdout", "")
            if stdout:
                (cond_dir / "raw-stdout.txt").write_text(stdout, encoding="utf-8")
            stderr = run_data.get("stderr", "")
            if stderr:
                (cond_dir / "raw-stderr.txt").write_text(stderr, encoding="utf-8")

        assessment = side.get("assessment")
        if assessment is not None:
            (cond_dir / "assessment.json").write_text(json.dumps(assessment, indent=2), encoding="utf-8")


def store_condition_raw(
    run_dir: Path,
    condition: str,
    *,
    stdout: str = "",
    stderr: str = "",
    structured_output: dict[str, Any] | None = None,
    tokens: dict[str, int] | None = None,
    duration_seconds: float | None = None,
    tool_calls: list[dict[str, Any]] | None = None,
    aethyme_usage: dict[str, Any] | None = None,
    exit_code: int | None = None,
    command: str | None = None,
) -> Path:
    """Store a condition's raw output immediately after it completes.

    Call this as soon as you have the result — before scoring.  This ensures
    raw data is preserved even if scoring or reporting fails later.
    """
    cond_dir = run_dir / "conditions" / condition
    cond_dir.mkdir(parents=True, exist_ok=True)

    if stdout:
        (cond_dir / "raw-stdout.txt").write_text(stdout, encoding="utf-8")
    if stderr:
        (cond_dir / "raw-stderr.txt").write_text(stderr, encoding="utf-8")
    if structured_output is not None:
        (cond_dir / "result.json").write_text(
            json.dumps(structured_output, indent=2), encoding="utf-8"
        )
    if tool_calls is not None:
        (cond_dir / "tool-calls.json").write_text(
            json.dumps(tool_calls, indent=2), encoding="utf-8"
        )
    if aethyme_usage is not None:
        (cond_dir / "aethyme-usage.json").write_text(
            json.dumps(aethyme_usage, indent=2), encoding="utf-8"
        )

    meta: dict[str, Any] = {"stored_at": datetime.now(UTC).isoformat()}
    meta["schema_version"] = RUN_METADATA_SCHEMA_VERSION
    meta["contract_versions"] = contract_versions()
    if tokens:
        meta["tokens"] = tokens
    if duration_seconds is not None:
        meta["duration_seconds"] = duration_seconds
    if exit_code is not None:
        meta["exit_code"] = exit_code
    if command:
        meta["command"] = command
    if aethyme_usage is not None:
        meta["aethyme_usage"] = aethyme_usage
    (cond_dir / "run-metadata.json").write_text(
        json.dumps(meta, indent=2), encoding="utf-8"
    )
    return cond_dir


def store_condition_chau7(
    run_dir: Path,
    condition: str,
    *,
    run_id: str | None = None,
    session_id: str | None = None,
    transcript: list[dict[str, Any]] | None = None,
    tool_calls: list[dict[str, Any]] | None = None,
    tab_output: str | None = None,
) -> Path:
    """Store Chau7 telemetry for a condition.

    Call this after pulling transcript and tool_calls from Chau7 MCP
    (``run_transcript`` and ``run_tool_calls``).  This is mandatory for
    every condition in every eval run — not an optional step.
    """
    chau7_dir = run_dir / "chau7" / condition
    chau7_dir.mkdir(parents=True, exist_ok=True)

    meta: dict[str, Any] = {"stored_at": datetime.now(UTC).isoformat()}
    meta["schema_version"] = RUN_METADATA_SCHEMA_VERSION
    meta["contract_versions"] = contract_versions()
    if run_id:
        meta["run_id"] = run_id
        (chau7_dir / "run-id.txt").write_text(run_id, encoding="utf-8")
    if session_id:
        meta["session_id"] = session_id
        (chau7_dir / "session-id.txt").write_text(session_id, encoding="utf-8")
    if transcript is not None:
        meta["transcript_turns"] = len(transcript)
        (chau7_dir / "transcript.json").write_text(
            json.dumps(transcript, indent=2, default=str), encoding="utf-8"
        )
    if tool_calls is not None:
        meta["tool_call_count"] = len(tool_calls)
        (chau7_dir / "tool-calls.json").write_text(
            json.dumps(tool_calls, indent=2, default=str), encoding="utf-8"
        )
    if tab_output:
        (chau7_dir / "tab-output.txt").write_text(tab_output, encoding="utf-8")

    (chau7_dir / "metadata.json").write_text(
        json.dumps(meta, indent=2), encoding="utf-8"
    )
    return chau7_dir


def _extract_ledger_record(
    result: dict[str, Any],
    *,
    repo_path: Path | None = None,
    run_dir: Path | None = None,
) -> dict[str, Any]:
    """Extract a compact scorecard record from a full result dict.

    One record per eval run — just the data you'd query across runs.
    Everything else lives in the full ``complete-result.json``.
    """
    active = _active_conditions(result)
    rp = repo_path or Path(result.get("report", {}).get("repo_path", "."))

    record: dict[str, Any] = {
        "timestamp": datetime.now(UTC).isoformat(),
        "task": result.get("task", "unknown"),
        "eval_type": result.get("eval_type", "unknown"),
        "repo": rp.name,
        "aethyme_commit": get_aethyme_commit()[:8],
    }

    scenario = result.get("scenario")
    if scenario:
        record["scenario"] = scenario

    model = result.get("model")
    if model:
        record["model"] = {
            k: model[k]
            for k in ("name", "provider", "backend", "reasoning")
            if k in model
        }

    if run_dir:
        record["run_dir"] = run_dir.name

    conds: dict[str, dict[str, Any]] = {}
    for cond in active:
        side = result.get(cond, {})
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

        entry: dict[str, Any] = {}
        ws = assessment.get("weighted_score")
        if ws is not None:
            entry["score"] = ws
        gs = summary.get("global_score")
        if gs is not None:
            entry["global_score"] = gs
        c = run.get("cost_usd")
        if c is not None and c > 0:
            entry["cost_usd"] = round(c, 4)
        dur = run.get("duration_seconds")
        if dur is not None and dur > 0:
            entry["duration_s"] = round(dur, 1)
        t = run.get("num_turns")
        if t:
            entry["turns"] = t
        tc = summary.get("tool_call_count")
        if tc is not None:
            entry["tool_call_count"] = tc
        total = _total_tokens(run)
        if total is not None:
            entry["total_tokens"] = total
        spt = summary.get("score_per_1k_tokens")
        if spt is not None:
            entry["score_per_1k_tokens"] = spt
        spm = summary.get("score_per_minute")
        if spm is not None:
            entry["score_per_minute"] = spm
        top_tools = summary.get("top_tools")
        if top_tools:
            entry["top_tools"] = top_tools
        for tok_key in ("input_tokens", "output_tokens", "cache_read_tokens", "cache_create_tokens"):
            v = run.get(tok_key)
            if v:
                entry[tok_key] = v
        run_metadata = run.get("run_metadata")
        if isinstance(run_metadata, dict):
            if run_metadata.get("run_id"):
                entry["run_id"] = run_metadata["run_id"]
            if run_metadata.get("repo_commit"):
                entry["repo_commit"] = run_metadata["repo_commit"]
            if run_metadata.get("config_hash"):
                entry["config_hash"] = run_metadata["config_hash"]

        if entry:
            conds[cond] = entry

    record["conditions"] = conds
    return record


def append_to_ledger(
    result: dict[str, Any],
    *,
    repo_path: Path | None = None,
    run_dir: Path | None = None,
) -> Path:
    """Append a compact scorecard record to the JSONL ledger.

    Returns the ledger path.  Safe to call multiple times — each call
    appends exactly one line.
    """
    record = _extract_ledger_record(result, repo_path=repo_path, run_dir=run_dir)
    RUNS_LEDGER.parent.mkdir(parents=True, exist_ok=True)
    with open(RUNS_LEDGER, "a", encoding="utf-8") as f:
        f.write(json.dumps(record, separators=(",", ":")) + "\n")
    return RUNS_LEDGER


def finalize_eval_run(
    run_dir: Path,
    result: dict[str, Any],
    *,
    repo_path: Path | None = None,
    eval_type: str = "unknown",
    baseline_override: dict[str, Any] | None = None,
) -> Path:
    """Write the complete result dump and final report for an eval run.

    Call this after all conditions have been collected, scored, and
    (optionally) enriched with Chau7 telemetry.  Writes:

    1. ``complete-result.json`` — the entire result dict (the single
       source of truth; every field that was available at eval time).
    2. ``report.md`` — the final human-readable markdown report.
    3. All per-condition artifacts via ``write_eval_run_artifacts()``.
    4. One line to ``eval-runs/runs.jsonl`` ledger.

    ``baseline_override`` lets callers supply a rolling-history baseline
    instead of the co-run control values, stabilizing comparison metrics
    across runs.
    """
    augment_result_with_summary_metrics(result, baseline_override=baseline_override)

    # 1. Complete dump
    (run_dir / "complete-result.json").write_text(
        json.dumps(result, indent=2, default=str), encoding="utf-8"
    )

    # 2. Structured artifacts
    write_eval_run_artifacts(run_dir, result)

    # 3. Markdown report
    rp = repo_path or Path(result.get("report", {}).get("repo_path", "."))
    content = _render_markdown(repo_path=rp, result=result)
    (run_dir / "report.md").write_text(content, encoding="utf-8")

    # 4. Also write to docs/reports/evals/ for discoverability
    REPORTS_ROOT.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    slug = _slugify(rp.name or "repo")
    report_path = REPORTS_ROOT / f"{timestamp}-{slug}-{eval_type}.md"
    report_path.write_text(content, encoding="utf-8")

    # 5. Append to JSONL ledger
    append_to_ledger(result, repo_path=rp, run_dir=run_dir)

    # 6. Print scorecard to stdout
    print_scorecard(result)

    return run_dir


def augment_result_with_summary_metrics(
    result: dict[str, Any],
    *,
    baseline_override: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Attach per-condition summaries and control-relative comparison metrics.

    When `baseline_override` is provided it replaces the co-run control values
    used as the denominator for ratio metrics. This is how callers apply a
    *rolling baseline* computed from historical control-cto-off runs instead
    of the current batch's own control row — single-run variance in the
    co-run control otherwise propagates into every other condition's
    derived score.

    baseline_override shape:
        {"source": "rolling", "window_size": N,
         "quality_score": ..., "total_tokens": ...,
         "duration_seconds": ..., "cost_usd": ...}
    """
    active = _active_conditions(result)
    if not active:
        return result

    baseline_condition = (
        "control-cto-off"
        if "control-cto-off" in active
        else ("control" if "control" in active else active[0])
    )
    baseline_side = result.get(baseline_condition, {})
    if not isinstance(baseline_side, dict):
        baseline_side = {}
    baseline_assessment = baseline_side.get("assessment") or {}
    baseline_run = baseline_side.get("run") or {}
    if not isinstance(baseline_assessment, dict):
        baseline_assessment = {}
    if not isinstance(baseline_run, dict):
        baseline_run = {}

    baseline_source = "co-run"
    baseline_window_size: int | None = None

    if baseline_override:
        # Rolling (or otherwise externally-supplied) baseline wins over the
        # co-run control values.
        baseline_source = str(baseline_override.get("source", "override"))
        raw_window = baseline_override.get("window_size")
        baseline_window_size = int(raw_window) if isinstance(raw_window, int) else None

        baseline_quality = baseline_override.get("quality_score")
        baseline_quality = (
            float(baseline_quality) if isinstance(baseline_quality, (int, float)) else None
        )
        baseline_tokens = baseline_override.get("total_tokens")
        baseline_tokens = (
            int(baseline_tokens) if isinstance(baseline_tokens, (int, float)) and baseline_tokens > 0 else None
        )
        baseline_duration = baseline_override.get("duration_seconds")
        baseline_duration = (
            float(baseline_duration)
            if isinstance(baseline_duration, (int, float)) and baseline_duration > 0
            else None
        )
        baseline_cost = baseline_override.get("cost_usd")
        baseline_cost = (
            float(baseline_cost) if isinstance(baseline_cost, (int, float)) and baseline_cost > 0 else None
        )
    else:
        baseline_quality = baseline_assessment.get("weighted_score")
        baseline_quality = (
            float(baseline_quality) if isinstance(baseline_quality, (int, float)) else None
        )
        baseline_tokens = _total_tokens(baseline_run)
        baseline_duration = baseline_run.get("duration_seconds")
        baseline_duration = (
            float(baseline_duration)
            if isinstance(baseline_duration, (int, float)) and baseline_duration > 0
            else None
        )
        baseline_cost = baseline_run.get("cost_usd")
        baseline_cost = (
            float(baseline_cost)
            if isinstance(baseline_cost, (int, float)) and baseline_cost > 0
            else None
        )

    global_scores: dict[str, float] = {}
    quality_scores: dict[str, float] = {}

    for cond in active:
        side = result.get(cond, {})
        if not isinstance(side, dict):
            continue
        assessment = side.get("assessment") or {}
        run = side.get("run") or {}
        if not isinstance(assessment, dict):
            assessment = {}
        if not isinstance(run, dict):
            run = {}

        summary = _build_condition_summary(
            cond=cond,
            assessment=assessment,
            run=run,
            baseline_condition=baseline_condition,
            baseline_quality=baseline_quality,
            baseline_tokens=baseline_tokens,
            baseline_duration=baseline_duration,
            baseline_cost=baseline_cost,
        )
        side["summary_metrics"] = summary

        quality = summary.get("quality_score")
        if isinstance(quality, (int, float)):
            quality_scores[cond] = float(quality)
        global_score = summary.get("global_score")
        if isinstance(global_score, (int, float)):
            global_scores[cond] = float(global_score)

    result["comparison"] = {
        "baseline_condition": baseline_condition,
        "baseline_source": baseline_source,
        "baseline_window_size": baseline_window_size,
        "recalculated_eval_score_formula": (
            "100 + quality_delta_vs_control "
            "+ 10 * ln(token_ratio_vs_control) "
            "+ 10 * ln(time_ratio_vs_control) "
            "+ 5 * ln(cost_ratio_vs_control)"
        ),
        "relative_score_weights": _RELATIVE_SCORE_WEIGHTS,
        "baseline_interpretation": {
            "100": "equal overall value to the control baseline",
            ">100": "better overall value than the control baseline",
            "<100": "worse overall value than the control baseline",
        },
        "relative_metrics": {
            "quality_delta_vs_control": "quality_score - control_cto_off_quality_score",
            "token_ratio_vs_control": "control_cto_off_total_tokens / total_tokens",
            "time_ratio_vs_control": "control_cto_off_duration_seconds / duration_seconds",
            "cost_ratio_vs_control": "control_cto_off_cost_usd / cost_usd",
        },
        "best_quality_condition": max(quality_scores, key=quality_scores.get) if quality_scores else None,
        "best_relative_condition": max(global_scores, key=global_scores.get) if global_scores else None,
        "baseline_quality_score": baseline_quality,
        "baseline_total_tokens": baseline_tokens,
        "baseline_duration_seconds": round(baseline_duration, 3) if baseline_duration is not None else None,
        "baseline_cost_usd": round(baseline_cost, 4) if baseline_cost is not None else None,
    }
    return result


def write_explain_repo_markdown_report(
    *,
    repo_path: Path,
    result: dict[str, Any],
    run_dir: Path | None = None,
) -> Path:
    """Persist a standard markdown report for an explain-repo evaluation."""
    content = _render_markdown(repo_path=repo_path, result=result)

    # Always write to docs/reports/evals/
    REPORTS_ROOT.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    slug = _slugify(repo_path.name or "repo")
    report_path = REPORTS_ROOT / f"{timestamp}-{slug}-explain-repo.md"
    report_path.write_text(content, encoding="utf-8")

    # Dual-write to run_dir if provided
    if run_dir is not None:
        write_eval_run_artifacts(run_dir, result)
        (run_dir / "report.md").write_text(content, encoding="utf-8")

    return report_path


def write_navigation_ctf_markdown_report(
    *,
    repo_path: Path,
    result: dict[str, Any],
    run_dir: Path | None = None,
) -> Path:
    content = _render_markdown(repo_path=repo_path, result=result)

    REPORTS_ROOT.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    slug = _slugify(repo_path.name or "repo")
    report_path = REPORTS_ROOT / f"{timestamp}-{slug}-navigation-ctf.md"
    report_path.write_text(content, encoding="utf-8")

    if run_dir is not None:
        write_eval_run_artifacts(run_dir, result)
        (run_dir / "report.md").write_text(content, encoding="utf-8")

    return report_path


def write_bug_fix_markdown_report(
    *,
    repo_path: Path,
    result: dict[str, Any],
    run_dir: Path | None = None,
) -> Path:
    content = _render_markdown(repo_path=repo_path, result=result)

    REPORTS_ROOT.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    slug = _slugify(repo_path.name or "repo")
    report_path = REPORTS_ROOT / f"{timestamp}-{slug}-bug-fix.md"
    report_path.write_text(content, encoding="utf-8")

    if run_dir is not None:
        write_eval_run_artifacts(run_dir, result)
        (run_dir / "report.md").write_text(content, encoding="utf-8")

    return report_path


def print_scorecard(result: dict[str, Any], *, file: Any = None) -> None:
    """Print the scorecard table to stdout (or *file*).

    Call this after ``assemble_bug_fix_result()`` to display results in the
    chat / terminal.  Output is a fixed-width table that renders well in
    both terminal and markdown contexts.
    """
    import sys

    out = file or sys.stdout
    active = _active_conditions(result)

    # Header
    hdr = (
        f"{'Condition':<22s} {'Qual':>6s} {'Recalc':>7s} {'Tools':>6s} "
        f"{'Cost':>8s} {'Duration':>9s} {'Total Tok':>10s} "
        f"{'Score/1K':>9s} {'Score/Min':>10s}"
    )
    sep = "-" * len(hdr)
    print(sep, file=out)
    print(hdr, file=out)
    print(sep, file=out)

    for cond in active:
        side = result.get(cond, {})
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

        ws = assessment.get("weighted_score")
        score = f"{ws:.2f}" if ws is not None else "-"
        gs = summary.get("global_score")
        global_score = f"{gs:.2f}" if gs is not None else "-"
        tc = summary.get("tool_call_count")
        tool_count = str(tc) if tc is not None else "-"

        c = run.get("cost_usd")
        cost = f"${c:.3f}" if c is not None and c > 0 else "-"

        dur = run.get("duration_seconds")
        duration = f"{dur:.1f}s" if dur is not None and dur > 0 else "-"

        total = _total_tokens(run)
        total_tok = f"{total:,}" if total is not None else "-"
        spt = summary.get("score_per_1k_tokens")
        score_per_tokens = f"{spt:.2f}" if spt is not None else "-"
        spm = summary.get("score_per_minute")
        score_per_minute = f"{spm:.2f}" if spm is not None else "-"

        print(
            f"{_cond_label(cond):<22s} {score:>6s} {global_score:>7s} {tool_count:>6s} "
            f"{cost:>8s} {duration:>9s} {total_tok:>10s} "
            f"{score_per_tokens:>9s} {score_per_minute:>10s}",
            file=out,
        )

    print(sep, file=out)


def _total_tokens(run: dict[str, Any]) -> int | None:
    """Sum all token types for a run (input + output + cache_read + cache_create).

    Returns None if no token data is available at all.
    """
    keys = ("input_tokens", "output_tokens", "cache_read_tokens", "cache_create_tokens")
    vals = [run.get(k) for k in keys]
    if all(v is None for v in vals):
        return None
    return sum(v or 0 for v in vals)


def _tool_frequency(run: dict[str, Any]) -> Counter[str]:
    freq: Counter[str] = Counter()
    tool_calls = run.get("tool_calls")
    if not isinstance(tool_calls, list):
        return freq
    for tc in tool_calls:
        if not isinstance(tc, dict):
            continue
        name = tc.get("tool") or tc.get("name") or "unknown"
        freq[str(name)] += 1
    return freq


def _top_tools(freq: Counter[str], *, limit: int = 3) -> list[dict[str, Any]]:
    return [
        {"name": tool_name, "count": count}
        for tool_name, count in freq.most_common(limit)
    ]


def _ratio(numerator: float | int | None, denominator: float | int | None) -> float | None:
    if numerator is None or denominator is None:
        return None
    if denominator <= 0:
        return None
    return float(numerator) / float(denominator)


def _build_condition_summary(
    *,
    cond: str,
    assessment: dict[str, Any],
    run: dict[str, Any],
    baseline_condition: str,
    baseline_quality: float | None,
    baseline_tokens: int | None,
    baseline_duration: float | None,
    baseline_cost: float | None,
) -> dict[str, Any]:
    quality = assessment.get("weighted_score")
    quality_score = float(quality) if isinstance(quality, (int, float)) else None
    total_tokens = _total_tokens(run)
    duration_seconds = run.get("duration_seconds")
    duration_seconds = (
        float(duration_seconds)
        if isinstance(duration_seconds, (int, float)) and duration_seconds > 0
        else None
    )
    cost_usd = run.get("cost_usd")
    cost_usd = float(cost_usd) if isinstance(cost_usd, (int, float)) and cost_usd > 0 else None

    tool_freq = _tool_frequency(run)
    tool_call_count = sum(tool_freq.values()) if tool_freq else 0

    quality_delta = (
        quality_score - baseline_quality
        if quality_score is not None and baseline_quality is not None
        else None
    )
    token_ratio = _ratio(baseline_tokens, total_tokens) if baseline_tokens is not None else None
    duration_ratio = _ratio(baseline_duration, duration_seconds) if baseline_duration is not None else None
    cost_ratio = _ratio(baseline_cost, cost_usd) if baseline_cost is not None else None

    global_score = None
    if quality_delta is not None:
        global_score = 100.0 + quality_delta
        if token_ratio is not None and token_ratio > 0:
            global_score += _RELATIVE_SCORE_WEIGHTS["tokens_log_multiplier"] * math.log(token_ratio)
        if duration_ratio is not None and duration_ratio > 0:
            global_score += _RELATIVE_SCORE_WEIGHTS["duration_log_multiplier"] * math.log(duration_ratio)
        if cost_ratio is not None and cost_ratio > 0:
            global_score += _RELATIVE_SCORE_WEIGHTS["cost_log_multiplier"] * math.log(cost_ratio)

    summary: dict[str, Any] = {
        "condition": cond,
        "baseline_condition": baseline_condition,
        "quality_score": round(quality_score, 2) if quality_score is not None else None,
        "global_score": round(global_score, 2) if global_score is not None else None,
        "recalculated_eval_score": round(global_score, 2) if global_score is not None else None,
        "quality_delta_vs_control": round(quality_delta, 2) if quality_delta is not None else None,
        "tool_call_count": tool_call_count,
        "top_tools": _top_tools(tool_freq),
        "total_tokens": total_tokens,
        "duration_seconds": round(duration_seconds, 3) if duration_seconds is not None else None,
        "cost_usd": round(cost_usd, 4) if cost_usd is not None else None,
        "token_ratio_vs_control": round(token_ratio, 4) if token_ratio is not None else None,
        "time_ratio_vs_control": round(duration_ratio, 4) if duration_ratio is not None else None,
        "cost_ratio_vs_control": round(cost_ratio, 4) if cost_ratio is not None else None,
        "score_per_1k_tokens": (
            round((quality_score * 1000.0) / total_tokens, 4)
            if quality_score is not None and total_tokens and total_tokens > 0
            else None
        ),
        "score_per_minute": (
            round((quality_score * 60.0) / duration_seconds, 4)
            if quality_score is not None and duration_seconds and duration_seconds > 0
            else None
        ),
        "efficiency_components": {
            "quality_delta_vs_control": round(quality_delta, 4) if quality_delta is not None else None,
            "token_ratio_vs_control": round(token_ratio, 4) if token_ratio is not None else None,
            "time_ratio_vs_control": round(duration_ratio, 4) if duration_ratio is not None else None,
            "cost_ratio_vs_control": round(cost_ratio, 4) if cost_ratio is not None else None,
        },
    }

    deliverable_status = (
        run.get("structured_output", {}).get("deliverable_status")
        if isinstance(run.get("structured_output"), dict)
        else None
    )
    if deliverable_status:
        summary["deliverable_status"] = deliverable_status

    return summary


def _cond_label(cond: str) -> str:
    """Human-readable label for a condition name."""
    labels = {
        "control-cto-off": "Control (CTO off)",
        "control-cto-on": "Control (CTO on)",
        "control": "Control",
        "explore": "Explore",
        "leverage": "Leverage",
        "task-conditioned": "Task-Conditioned",
    }
    return labels.get(cond, cond.title())


def _render_markdown(*, repo_path: Path, result: dict[str, Any]) -> str:
    """Render the definitive eval report markdown.

    Section order is fixed and non-negotiable:
    Meta -> Model -> Scorecard -> Score Breakdown -> Prompts ->
    Agent Output -> Tool Call Analysis -> Aethyme Usage ->
    (legacy diagnostics, when present) ->
    Verdict -> Notes -> Raw Data
    """
    lines: list[str] = []
    _section_meta(lines, repo_path, result)
    _section_model(lines, result)
    _section_scorecard(lines, result)
    _section_score_breakdown(lines, result)
    _section_prompts(lines, result)
    _section_agent_output(lines, result)
    _section_tool_calls(lines, result)
    _section_aethyme_usage(lines, result)
    if _should_render_legacy_eval_diagnostics(result):
        _section_legacy_eval_diagnostics(lines, result)
    _section_verdict(lines, result)
    _section_notes(lines, result)
    _section_raw_data(lines, result)
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# Section renderers — each appends to *lines* in place.
# ---------------------------------------------------------------------------


def _section_meta(
    lines: list[str], repo_path: Path, result: dict[str, Any],
) -> None:
    active = _active_conditions(result)
    lines.extend([
        f"# Eval Report: {result.get('task', 'unknown')}",
        "",
        "## Meta",
        "",
        f"- Date: {datetime.now(UTC).date().isoformat()}",
        f"- Repository: `{repo_path}`",
        f"- Eval Type: {result.get('eval_type', 'unknown')}",
    ])
    scenario = result.get("scenario", "")
    if scenario:
        lines.append(f"- Scenario: {scenario}")
    lines.extend([
        f"- Conditions: {', '.join(active)}",
        f"- Aethyme Commit: `{get_aethyme_commit()}`",
        "",
    ])


def _section_model(lines: list[str], result: dict[str, Any]) -> None:
    model = result.get("model", {})
    lines.extend(["## Model", ""])
    if not model:
        lines.extend(["N/A", ""])
        return
    lines.extend([
        f"- Name: {model.get('name', 'N/A')}",
        f"- Provider: {model.get('provider', 'N/A')}",
        f"- Backend: {model.get('backend', 'N/A')}",
        f"- Reasoning: {model.get('reasoning', 'N/A')}",
        f"- Permission Mode: {model.get('permission_mode', 'N/A')}",
        "",
    ])


def _section_scorecard(lines: list[str], result: dict[str, Any]) -> None:
    """Scorecard with quality, efficiency, tools, and usage metrics."""
    active = _active_conditions(result)
    lines.extend([
        "## Scorecard",
        "",
        "| Condition | Quality | Recalculated Eval | Tools | Cost | Duration | Total Tokens | Score / 1K Tokens | Score / Minute |",
        "|---|---|---|---|---|---|---|---|---|",
    ])
    for cond in active:
        side = result.get(cond, {})
        if not isinstance(side, dict):
            side = {}
        assessment = side.get("assessment") or {}
        run = side.get("run") or {}
        summary = side.get("summary_metrics") or {}
        if not isinstance(assessment, dict):
            assessment = {}
        if not isinstance(run, dict):
            run = {}
        if not isinstance(summary, dict):
            summary = {}

        ws = assessment.get("weighted_score")
        score = f"{ws}" if ws is not None else "-"
        gs = summary.get("global_score")
        global_score = f"{gs}" if gs is not None else "-"
        tc = summary.get("tool_call_count")
        tool_count = f"{tc}" if tc is not None else "-"

        c = run.get("cost_usd")
        cost = f"${c:.3f}" if c is not None and c > 0 else "-"

        dur = run.get("duration_seconds")
        duration = f"{dur:.1f}s" if dur is not None and dur > 0 else "-"

        total = _total_tokens(run)
        total_tok = f"{total:,}" if total is not None else "-"
        spt = summary.get("score_per_1k_tokens")
        score_per_tokens = f"{spt:.2f}" if spt is not None else "-"
        spm = summary.get("score_per_minute")
        score_per_minute = f"{spm:.2f}" if spm is not None else "-"

        lines.append(
            f"| {_cond_label(cond)} | {score} | {global_score} | {tool_count} | {cost} | {duration} "
            f"| {total_tok} | {score_per_tokens} | {score_per_minute} |"
        )
    lines.append("")


def _section_score_breakdown(lines: list[str], result: dict[str, Any]) -> None:
    """Per-component score breakdown (only rendered when scores exist)."""
    active = _active_conditions(result)

    # Find component names/weights from first condition that has them.
    # The assessment uses "scores" (raw 0-1 values) and "weights" (integers
    # like 60 meaning 60%).
    components: list[tuple[str, float]] = []
    for cond in active:
        side = result.get(cond, {})
        if not isinstance(side, dict):
            continue
        assessment = side.get("assessment") or {}
        if not isinstance(assessment, dict):
            continue
        cs = assessment.get("scores", {})
        weights = assessment.get("weights", {})
        if cs:
            components = [(name, weights.get(name, 0)) for name in cs]
            break

    if not components:
        return

    header = (
        "| Component | Weight | "
        + " | ".join(_cond_label(c) for c in active)
        + " |"
    )
    sep = "|---|---| " + " | ".join("---" for _ in active) + " |"

    lines.extend(["## Score Breakdown", "", header, sep])

    for comp_name, weight in components:
        # Weights are stored as integers (60 = 60%), not fractions.
        weight_pct = f"{weight:.0f}%" if weight >= 1 else f"{weight * 100:.0f}%"
        vals: list[str] = []
        for cond in active:
            side = result.get(cond, {})
            if isinstance(side, dict):
                a = side.get("assessment") or {}
                cs = a.get("scores", {}) if isinstance(a, dict) else {}
                v = cs.get(comp_name)
                vals.append(f"{v:.3f}" if v is not None else "-")
            else:
                vals.append("-")
        label = comp_name.replace("_", " ").title()
        lines.append(f"| {label} | {weight_pct} | " + " | ".join(vals) + " |")
    lines.append("")


def _section_prompts(lines: list[str], result: dict[str, Any]) -> None:
    """Verbatim prompt for each condition."""
    active = _active_conditions(result)
    lines.extend(["## Prompts", ""])
    for cond in active:
        side = result.get(cond, {})
        prompt = side.get("prompt", "") if isinstance(side, dict) else ""
        lines.extend([
            f"### {_cond_label(cond)}",
            "",
            "```text",
            prompt or "N/A",
            "```",
            "",
        ])


def _section_agent_output(lines: list[str], result: dict[str, Any]) -> None:
    """Structured output JSON for each condition."""
    active = _active_conditions(result)
    lines.extend(["## Agent Output", ""])
    for cond in active:
        side = result.get(cond, {})
        if isinstance(side, dict):
            run = side.get("run") or {}
            structured = run.get("structured_output") if isinstance(run, dict) else None
        else:
            structured = None
        lines.extend([
            f"### {_cond_label(cond)}",
            "",
            "```json",
            json.dumps(structured, indent=2) if structured is not None else "null",
            "```",
            "",
        ])


def _section_tool_calls(lines: list[str], result: dict[str, Any]) -> None:
    """Per-condition tool call frequency tables (skipped if no data)."""
    active = _active_conditions(result)
    has_any = False
    section_lines: list[str] = []

    for cond in active:
        side = result.get(cond, {})
        if not isinstance(side, dict):
            continue
        run = side.get("run")
        if not run or not isinstance(run, dict):
            continue
        tool_calls = run.get("tool_calls")
        if not tool_calls or not isinstance(tool_calls, list):
            continue

        has_any = True
        summary = side.get("summary_metrics") or {}
        if not isinstance(summary, dict):
            summary = {}
        freq = _tool_frequency(run)

        section_lines.extend([
            f"### {_cond_label(cond)}",
            "",
            f"Total tool calls: {len(tool_calls)}",
            "",
        ])
        top_tools = summary.get("top_tools")
        if top_tools:
            section_lines.extend([
                "Top tools: "
                + ", ".join(f"`{item['name']}` x{item['count']}" for item in top_tools),
                "",
            ])
        section_lines.extend([
            "| Tool | Count |",
            "|---|---|",
        ])
        for tool_name, count in freq.most_common():
            section_lines.append(f"| `{tool_name}` | {count} |")
        section_lines.append("")

    if not has_any:
        return

    lines.extend(["## Tool Call Analysis", ""] + section_lines)


def _section_aethyme_usage(lines: list[str], result: dict[str, Any]) -> None:
    """Show whether Aethyme was actually invoked by each condition."""
    active = _active_conditions(result)
    rows: list[str] = []
    has_any = False

    for cond in active:
        side = result.get(cond, {})
        run = side.get("run") if isinstance(side, dict) else None
        usage = run.get("aethyme_usage") if isinstance(run, dict) else None
        if not isinstance(usage, dict):
            continue
        has_any = True
        if usage.get("aethyme_used"):
            used = "yes"
        else:
            used = "no"
        kinds = ", ".join(
            str(item.get("kind"))
            for item in usage.get("aethyme_commands", [])
            if isinstance(item, dict) and item.get("kind")
        )
        rows.append(
            "| "
            + " | ".join(
                [
                    _cond_label(cond),
                    used,
                    str(usage.get("aethyme_command_count", 0)),
                    kinds or "-",
                    str(usage.get("manual_shell_after_aethyme_count", 0)),
                    str(usage.get("manual_search_after_aethyme_count", 0)),
                ]
            )
            + " |"
        )

    if not has_any:
        return

    lines.extend(
        [
            "## Aethyme Usage",
            "",
            "| Condition | Aethyme Used | Aethyme Commands | Command Kinds | Shell After | Search After |",
            "|---|---|---|---|---|---|",
            *rows,
            "",
        ]
    )


def _should_render_legacy_eval_diagnostics(result: dict[str, Any]) -> bool:
    """Return True when legacy explain-repo style diagnostics are meaningful."""
    if result.get("report") is not None:
        return True
    if result.get("signals") is not None:
        return True
    if result.get("pack") is not None:
        return True
    task = str(result.get("task", "")).strip().lower()
    return "explain" in task


def _section_legacy_eval_diagnostics(lines: list[str], result: dict[str, Any]) -> None:
    """Render legacy explain-repo diagnostic headings for compatibility."""
    report = result.get("report") if isinstance(result.get("report"), dict) else {}
    pack = result.get("pack") if isinstance(result.get("pack"), dict) else {}
    signals = result.get("signals") if isinstance(result.get("signals"), dict) else {}

    lines.extend(["## Context Pack Audit", ""])
    lines.extend([
        f"- Navigation items: {report.get('navigation_items', 0)}",
        f"- Risk items: {report.get('risk_items', 0)}",
    ])
    task = pack.get("task", {}) if isinstance(pack.get("task"), dict) else {}
    if task.get("raw"):
        lines.append(f"- Task: {task.get('raw')}")
    lines.append("")

    lines.extend(["## Graph Quality Notes", ""])
    parser_visibility = signals.get("parser_visibility") if isinstance(signals.get("parser_visibility"), dict) else {}
    if parser_visibility:
        lines.extend([
            f"- Parser visibility: {parser_visibility.get('level', 'unknown')} ({parser_visibility.get('score', 'n/a')})",
            "",
        ])
    else:
        lines.extend(["N/A", ""])

    lines.extend(["## Prompt Effectiveness", ""])
    active = _active_conditions(result)
    prompt_chars_by_condition = report.get("condition_prompt_chars", {}) if isinstance(report, dict) else {}
    if not active:
        lines.extend(["N/A", ""])
    else:
        for cond in active:
            side = result.get(cond, {})
            run = side.get("run") if isinstance(side, dict) else {}
            if not isinstance(run, dict):
                run = {}
            lines.append(f"### {_cond_label(cond)}")
            lines.append("")
            prompt_chars = prompt_chars_by_condition.get(cond)
            if prompt_chars is None:
                prompt_chars = len((side or {}).get("prompt", "")) if isinstance(side, dict) else 0
            lines.append(f"- Prompt chars: {prompt_chars}")
            lines.append(f"- Input tokens: {run.get('input_tokens', 'n/a')}")
            lines.append(f"- Output tokens: {run.get('output_tokens', 'n/a')}")
            final_message = run.get("final_output_message")
            if final_message:
                lines.append(f"- Final output: {final_message}")
            lines.append("")

    lines.extend(["## Lessons & Action Items", ""])
    lines.extend([
        "- Keep context packs concise and explicitly prioritize high-confidence nodes.",
        "- Capture parser visibility deltas across conditions to track graph quality drift.",
        "",
    ])


def _section_verdict(lines: list[str], result: dict[str, Any]) -> None:
    """Auto-generated one-paragraph summary comparing conditions."""
    active = _active_conditions(result)
    scores: dict[str, float] = {}
    costs: dict[str, float] = {}
    global_scores: dict[str, float] = {}
    all_passed = True

    for cond in active:
        side = result.get(cond, {})
        if not isinstance(side, dict):
            continue
        assessment = side.get("assessment") or {}
        run = side.get("run") or {}
        summary = side.get("summary_metrics") or {}
        if isinstance(assessment, dict):
            ws = assessment.get("weighted_score")
            if ws is not None:
                scores[cond] = ws
            cs = assessment.get("component_scores", {})
            if cs.get("fix_test", 1.0) < 1.0:
                all_passed = False
        if isinstance(run, dict):
            c = run.get("cost_usd")
            if c is not None and c > 0:
                costs[cond] = c
        if isinstance(summary, dict):
            gs = summary.get("global_score")
            if gs is not None:
                global_scores[cond] = gs

    lines.extend(["## Verdict", ""])

    if not scores:
        lines.extend(["N/A", ""])
        return

    best = max(scores, key=lambda k: scores[k])
    worst = min(scores, key=lambda k: scores[k])

    parts: list[str] = []
    parts.append(
        f"**{_cond_label(best)}** scored highest ({scores[best]:.2f}/100)"
        + (f", **{_cond_label(worst)}** lowest ({scores[worst]:.2f}/100)" if best != worst else "")
        + "."
    )
    if global_scores:
        best_global = max(global_scores, key=lambda k: global_scores[k])
        parts.append(
            f"Best overall value versus the control baseline: **{_cond_label(best_global)}** "
            f"({global_scores[best_global]:.2f} recalculated eval score)."
        )
    if costs:
        cheapest = min(costs, key=lambda k: costs[k])
        priciest = max(costs, key=lambda k: costs[k])
        if cheapest != priciest:
            parts.append(
                f"Most efficient: {_cond_label(cheapest)} (${costs[cheapest]:.3f}), "
                f"most expensive: {_cond_label(priciest)} (${costs[priciest]:.3f})."
            )
    if all_passed and scores:
        parts.append("All conditions passed tests.")

    lines.extend([" ".join(parts), ""])


def _section_notes(lines: list[str], result: dict[str, Any]) -> None:
    """Free-text notes or explanation."""
    notes = result.get("notes", "")
    explanation = result.get("explanation", "")
    lines.extend(["## Notes", ""])
    if notes:
        lines.extend([notes, ""])
    elif explanation:
        lines.extend([explanation, ""])
    else:
        lines.extend(["N/A", ""])


def _section_raw_data(lines: list[str], result: dict[str, Any]) -> None:
    """All JSON dumps collapsed at the bottom."""
    active = _active_conditions(result)
    lines.extend(["---", "", "## Raw Data", ""])

    # Reference Output
    lines.extend([
        "### Reference Output",
        "",
        "```json",
        json.dumps(result.get("reference_output"), indent=2),
        "```",
        "",
    ])

    # Output Schema
    lines.extend([
        "### Output Schema",
        "",
        "```json",
        json.dumps(result.get("output_schema"), indent=2),
        "```",
        "",
    ])

    # Scoring Rubric
    lines.extend([
        "### Scoring Rubric",
        "",
        "```json",
        json.dumps(result.get("scoring_rubric"), indent=2),
        "```",
        "",
    ])

    # Per-Condition Run Records
    lines.extend(["### Per-Condition Run Records", ""])
    for cond in active:
        side = result.get(cond, {})
        run = side.get("run") if isinstance(side, dict) else None
        lines.extend([
            f"#### {_cond_label(cond)}",
            "",
            "```json",
            json.dumps(run, indent=2, default=str) if run else "null",
            "```",
            "",
        ])

    # Per-Condition Assessments
    lines.extend(["### Per-Condition Assessments", ""])
    for cond in active:
        side = result.get(cond, {})
        assessment = side.get("assessment") if isinstance(side, dict) else None
        lines.extend([
            f"#### {_cond_label(cond)}",
            "",
            "```json",
            json.dumps(assessment, indent=2) if assessment else "null",
            "```",
            "",
        ])

    # Optional extras — only if present
    if result.get("pack") is not None:
        lines.extend([
            "### Context Pack",
            "",
            "```json",
            json.dumps(result["pack"], indent=2),
            "```",
            "",
        ])

    if result.get("navigation_context") is not None:
        lines.extend([
            "### Navigation Context",
            "",
            "```json",
            json.dumps(result["navigation_context"], indent=2),
            "```",
            "",
        ])

    if result.get("challenge") is not None:
        lines.extend([
            "### Challenge",
            "",
            "```json",
            json.dumps(result["challenge"], indent=2),
            "```",
            "",
        ])

    if result.get("signals") is not None:
        lines.extend([
            "### Repo Signals",
            "",
            "```json",
            json.dumps(result["signals"], indent=2),
            "```",
            "",
        ])


def _slugify(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-") or "repo"
