"""Evaluation report helpers for local-first Aethyme benchmarks."""

from __future__ import annotations

import json
import re
import subprocess
from collections import Counter
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .runner import EvaluationRunResult

REPORTS_ROOT = Path(__file__).resolve().parents[2] / "docs" / "reports" / "evals"
EVAL_RUNS_ROOT = Path(__file__).resolve().parents[2] / "eval-runs"

CONDITION_ORDER = ("control", "explore", "leverage")


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
    def baseline_run(self) -> dict[str, Any] | None:
        return self.condition_runs.get("control")

    @property
    def aethyme_run(self) -> dict[str, Any] | None:
        return self.condition_runs.get("leverage")

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
            "baseline_run": self.baseline_run,
            "aethyme_run": self.aethyme_run,
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


def _get_aethyme_commit() -> str:
    """Return the current Aethyme HEAD commit hash, or 'unknown' on failure."""
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"],
            cwd=Path(__file__).resolve().parents[3],
            text=True,
        ).strip()
    except Exception:
        return "unknown"


def create_eval_run_dir(repo_path: Path, eval_type: str) -> Path:
    """Create and return a timestamped eval run directory under eval-runs/."""
    timestamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    slug = _slugify(repo_path.name or "repo")
    run_dir = EVAL_RUNS_ROOT / f"{timestamp}-{slug}-{eval_type}"
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "artifacts").mkdir(exist_ok=True)
    for cond in CONDITION_ORDER:
        (run_dir / "conditions" / cond).mkdir(parents=True, exist_ok=True)
        (run_dir / "chau7" / cond).mkdir(parents=True, exist_ok=True)

    metadata = {
        "timestamp": datetime.now(UTC).isoformat(),
        "aethyme_commit": _get_aethyme_commit(),
        "repo_path": str(repo_path),
        "eval_type": eval_type,
    }
    (run_dir / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    return run_dir


def write_eval_run_artifacts(run_dir: Path, result: dict[str, Any]) -> None:
    """Write all eval artifacts into the run directory structure."""
    artifacts = run_dir / "artifacts"

    # Per-condition prompts
    for cond in CONDITION_ORDER:
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
    ]:
        value = result.get(key)
        if value is not None:
            (artifacts / name).write_text(json.dumps(value, indent=2), encoding="utf-8")

    # Per-condition results and assessments
    for cond in CONDITION_ORDER:
        side = result.get(cond, {})
        if not side:
            continue
        cond_dir = run_dir / "conditions" / cond
        run_data = side.get("run")
        if run_data and isinstance(run_data, dict):
            structured = run_data.get("structured_output")
            if structured is not None:
                (cond_dir / "result.json").write_text(json.dumps(structured, indent=2), encoding="utf-8")
        assessment = side.get("assessment")
        if assessment is not None:
            (cond_dir / "assessment.json").write_text(json.dumps(assessment, indent=2), encoding="utf-8")


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


def _render_markdown(*, repo_path: Path, result: dict[str, Any]) -> str:
    report = result["report"]
    condition_prompt_chars = report.get("condition_prompt_chars", {})
    lines = [
        f"# Eval Report: {result['task']}",
        "",
        f"Last Updated: {datetime.now(UTC).date().isoformat()}",
        "",
        f"- Repository: `{repo_path}`",
        f"- Generated: `{datetime.now(UTC).isoformat()}`",
        "",
        "## Summary",
        "",
    ]

    # Prompt chars table
    for cond in CONDITION_ORDER:
        if cond in condition_prompt_chars:
            lines.append(f"- {cond.title()} prompt chars: `{condition_prompt_chars[cond]}`")
    lines.append(f"- Navigation items: `{report['navigation_items']}`")
    lines.append(f"- Risk items: `{report['risk_items']}`")
    lines.append("")

    if result.get("signals") is not None:
        lines.extend(
            [
                "## Repo Signals",
                "",
                "```json",
                json.dumps(result["signals"], indent=2),
                "```",
                "",
            ]
        )

    # Render each condition section
    for cond in CONDITION_ORDER:
        side = result.get(cond, {})
        if not side:
            continue
        prompt = side.get("prompt", result.get(f"{cond}_prompt", result.get("baseline_prompt" if cond == "control" else "", "")))
        run = side.get("run", result.get(f"{cond}_run", result.get("baseline_run" if cond == "control" else None)))
        assessment = side.get("assessment", result.get(f"{cond}_assessment"))
        lines.extend(
            [
                f"## {cond.title()}",
                "",
            ]
        )
        lines.extend(_run_section(cond.title(), prompt, run, assessment))
        lines.append("")

    # Diagnostic sections
    lines.extend(_tool_call_analysis_section(result))
    lines.extend(_context_pack_audit_section(result))

    # Comparison table
    lines.extend(
        [
            "## Comparison",
            "",
            "| Metric | " + " | ".join(c.title() for c in CONDITION_ORDER if c in condition_prompt_chars) + " |",
            "| --- | " + " | ".join("---" for c in CONDITION_ORDER if c in condition_prompt_chars) + " |",
            "| Prompt chars | " + " | ".join(f"`{condition_prompt_chars.get(c, '-')}`" for c in CONDITION_ORDER if c in condition_prompt_chars) + " |",
        ]
    )

    # Add run metrics row if any runs exist
    condition_runs = report.get("condition_runs", {})
    active_conditions = [c for c in CONDITION_ORDER if c in condition_prompt_chars]
    durations = []
    for c in active_conditions:
        run = condition_runs.get(c)
        if run and isinstance(run, dict):
            durations.append(f"`{run['duration_seconds']:.3f}s`")
        else:
            durations.append("`-`")
    if any(d != "`-`" for d in durations):
        lines.append("| Wall time | " + " | ".join(durations) + " |")

    lines.extend(
        [
            "",
            f"- Navigation items surfaced: `{report['navigation_items']}`",
            f"- Risk items surfaced: `{report['risk_items']}`",
        ]
    )

    lines.extend(
        [
            "",
            "## Reference",
            "",
            "### Output Schema",
            "",
            "```json",
            json.dumps(result.get("output_schema"), indent=2),
            "```",
            "",
            "### Scoring Rubric",
            "",
            "```json",
            json.dumps(result.get("scoring_rubric"), indent=2),
            "```",
            "",
            "### Reference Output",
            "",
            "```json",
            json.dumps(result.get("reference_output"), indent=2),
            "```",
        ]
    )

    if result.get("challenge") is not None:
        lines.extend(
            [
                "",
                "### Challenge",
                "",
                "```json",
                json.dumps(result.get("challenge"), indent=2),
                "```",
            ]
        )

    if result.get("pack") is not None:
        lines.extend(
            [
                "",
                "## Aethyme Pack",
                "",
                "```json",
                json.dumps(result.get("pack"), indent=2),
                "```",
            ]
        )

    if result.get("explanation"):
        lines.extend(
            [
                "",
                "## Explanation",
                "",
                "```text",
                result["explanation"],
                "```",
            ]
        )

    lines.extend(_manual_sections())

    return "\n".join(lines) + "\n"


def _tool_call_analysis_section(result: dict[str, Any]) -> list[str]:
    """Per-condition tool call frequency tables. Skipped if no condition has tool data."""
    has_any = False
    section_lines: list[str] = []

    for cond in CONDITION_ORDER:
        side = result.get(cond, {})
        if not side:
            continue
        run = side.get("run")
        if not run or not isinstance(run, dict):
            continue
        tool_calls = run.get("tool_calls")
        if not tool_calls or not isinstance(tool_calls, list):
            continue

        has_any = True
        freq: Counter[str] = Counter()
        cli_commands: list[str] = []
        for tc in tool_calls:
            tool_name = tc.get("tool", "unknown")
            freq[tool_name] += 1
            if tool_name in ("shell", "bash", "terminal"):
                summary = tc.get("input_summary", "")
                if summary:
                    cli_commands.append(summary)

        section_lines.extend(
            [
                f"### {cond.title()}",
                "",
                f"Total tool calls: `{len(tool_calls)}`",
                "",
                "| Tool | Count |",
                "| --- | --- |",
            ]
        )
        for tool_name, count in freq.most_common():
            section_lines.append(f"| `{tool_name}` | {count} |")
        section_lines.append("")

        if cli_commands:
            section_lines.append("CLI commands executed:")
            section_lines.append("")
            for cmd in cli_commands:
                section_lines.append(f"- `{cmd}`")
            section_lines.append("")

    if not has_any:
        return []

    return ["", "## Tool Call Analysis", ""] + section_lines


def _context_pack_audit_section(result: dict[str, Any]) -> list[str]:
    """Context pack summary stats and navigation context dump."""
    pack = result.get("pack")
    nav_context = result.get("navigation_context")
    if pack is None and nav_context is None:
        return []

    lines = ["", "## Context Pack Audit", ""]

    if pack is not None:
        nav_order = pack.get("navigation_order", pack.get("scope", {}).get("navigation_order", []))
        anchors = pack.get("anchors", [])
        in_scope = pack.get("scope", {}).get("in_scope_files", [])
        cli_commands = pack.get("commands", nav_context.get("commands", []) if nav_context else [])
        lines.extend(
            [
                "### Pack Summary",
                "",
                f"- Anchors: `{len(anchors)}`",
                f"- Navigation order items: `{len(nav_order)}`",
                f"- In-scope files: `{len(in_scope)}`",
                f"- CLI commands: `{len(cli_commands)}`",
                "",
            ]
        )

    if nav_context is not None:
        lines.extend(
            [
                "### Navigation Context",
                "",
                "```json",
                json.dumps(nav_context, indent=2),
                "```",
                "",
            ]
        )

    lines.extend(
        [
            "<!-- Signal-to-Noise Assessment",
            "Rate the relevance of the navigation context provided to the leverage condition:",
            "- Anchors: were the starting points useful?",
            "- Scope: did in-scope files cover what the agent needed?",
            "- Navigation order: was the reading order helpful?",
            "- Noise: what was included but not needed?",
            "-->",
        ]
    )

    return lines


def _manual_sections() -> list[str]:
    """Placeholder sections for manual post-run analysis."""
    return [
        "",
        "## Graph Quality Notes",
        "",
        "<!-- Post-run analysis of graph quality:",
        "- Did the graph capture the right structural relationships?",
        "- Were important edges missing or spurious?",
        "- How did graph coverage affect each condition's performance?",
        "-->",
        "",
        "## Prompt Effectiveness",
        "",
        "<!-- Post-run analysis of prompt design:",
        "- Did the control prompt give the agent enough to work with?",
        "- Did the explore prompt's CLI commands get used effectively?",
        "- Did the leverage prompt's context file provide the right framing?",
        "- What prompt changes would improve the next run?",
        "-->",
        "",
        "## Lessons & Action Items",
        "",
        "<!-- Post-run action items:",
        "- [ ] ",
        "- [ ] ",
        "- [ ] ",
        "-->",
    ]


def _run_section(
    label: str,
    prompt: str,
    run: dict[str, Any] | None,
    assessment: dict[str, Any] | None,
) -> list[str]:
    lines = [
        "### Prompt",
        "",
        "```text",
        prompt,
        "```",
        "",
    ]
    if run is None:
        lines.extend(
            [
                "### Run Metrics",
                "",
                "- input tokens: `null`",
                "- output tokens: `null`",
                "- retries: `null`",
                "- wall time: `null`",
                "",
                "### Final Output Message",
                "",
                "```text",
                f"{label} runner not executed.",
                "```",
                "",
                "### Structured Output",
                "",
                "```json",
                "null",
                "```",
            ]
        )
        if assessment is not None:
            lines.extend(
                [
                    "",
                    "### Assessment",
                    "",
                    "```json",
                    json.dumps(assessment, indent=2),
                    "```",
                ]
            )
        return lines

    lines.extend(
        [
            "### Run Metrics",
            "",
            f"- command: `{run['command']}`",
            f"- exit code: `{run['exit_code']}`",
            f"- input tokens: `{run['input_tokens']}`",
            f"- output tokens: `{run['output_tokens']}`",
            f"- retries: `{run['retries']}`",
            f"- review burden: `{run['review_burden']}`",
            f"- wall time: `{run['duration_seconds']:.3f}s`",
            "",
            "### Final Output Message",
            "",
            "```text",
            run.get("final_output_message") or "",
            "```",
            "",
            "### Structured Output",
            "",
            "```json",
            json.dumps(run.get("structured_output"), indent=2),
            "```",
            "",
            "### Raw Run Record",
            "",
            "```json",
            json.dumps(run, indent=2),
            "```",
        ]
    )

    # Tool calls subsection
    tool_calls = run.get("tool_calls")
    if tool_calls and isinstance(tool_calls, list):
        lines.extend(
            [
                "",
                "### Tool Calls",
                "",
                f"Total: `{len(tool_calls)}`",
                "",
            ]
        )
        for tc in tool_calls:
            tool_name = tc.get("tool", "unknown")
            summary = tc.get("input_summary", "")
            lines.append(f"- `{tool_name}({summary})`")

    if assessment is not None:
        lines.extend(
            [
                "",
                "### Assessment",
                "",
                "```json",
                json.dumps(assessment, indent=2),
                "```",
            ]
        )
    return lines


def _slugify(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-") or "repo"
