import json
import os
import sys
from pathlib import Path
from typing import Any, TypeAlias, TypedDict, cast

import click
import structlog

# Add src to path for module imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from src.indexing.engine import (
    EngineError,
    engine_runtime_info,
)
from src.indexing.engine import (
    analyze_dead_code as analyze_dead_code_answer,
)
from src.indexing.repository_snapshot import capture_snapshot

# Configure logging
structlog.configure(
    processors=[
        structlog.stdlib.filter_by_level,
        structlog.stdlib.add_logger_name,
        structlog.stdlib.add_log_level,
        structlog.stdlib.PositionalArgumentsFormatter(),
        structlog.processors.TimeStamper(fmt="iso"),
        structlog.processors.StackInfoRenderer(),
        structlog.processors.format_exc_info,
        structlog.dev.ConsoleRenderer(),
    ],
    context_class=dict,
    logger_factory=structlog.stdlib.LoggerFactory(),
    cache_logger_on_first_use=True,
)

logger = structlog.get_logger()

CLIState: TypeAlias = dict[str, str | bool | None]


class AethymeCLIGroup(click.Group):
    """Custom command group for clearer recovery from removed entry points."""

    def get_command(self, ctx: click.Context, cmd_name: str) -> click.Command | None:
        if cmd_name == "explore":
            raise click.UsageError(
                "'explore' was removed from the Python CLI on 2026-05-08. "
                "Use the native binary instead:\n\n"
                '  "$AETHYME_ROOT/rust/target/release/aethyme" explore '
                '--repo "$REPO" --request "<task>" --format answer-json\n\n'
                "The Python CLI still handles graph, task, intents, facts, and analyze."
            )
        return super().get_command(ctx, cmd_name)


class FixRecord(TypedDict):
    """Autofix change proposal emitted by individual fixers."""

    file_path: Path
    original_content: str
    new_content: str
    fix_type: str


def normalize_fixes(raw_fixes: list[dict[str, Any]]) -> list[FixRecord]:
    """Validate fixer output before patch generation."""
    normalized: list[FixRecord] = []
    for raw_fix in raw_fixes:
        file_path = raw_fix.get("file_path")
        original_content = raw_fix.get("original_content")
        new_content = raw_fix.get("new_content")
        fix_type = raw_fix.get("fix_type")

        if isinstance(file_path, str):
            path_value = Path(file_path)
        elif isinstance(file_path, Path):
            path_value = file_path
        else:
            raise ValueError(f"Invalid fix file_path: {file_path!r}")

        if not isinstance(original_content, str):
            raise ValueError(f"Invalid fix original_content for {path_value}")
        if not isinstance(new_content, str):
            raise ValueError(f"Invalid fix new_content for {path_value}")
        if not isinstance(fix_type, str):
            raise ValueError(f"Invalid fix fix_type for {path_value}")

        normalized.append(
            FixRecord(
                file_path=path_value,
                original_content=original_content,
                new_content=new_content,
                fix_type=fix_type,
            )
        )

    return normalized


def get_state(ctx: click.Context) -> CLIState:
    """Return the mutable CLI state."""
    if ctx.obj is None:
        ctx.obj = {}
    return cast(CLIState, ctx.obj)


@click.group(cls=AethymeCLIGroup)
@click.option(
    "--tenant-id",
    envvar="AETHYME_TENANT_ID",
    help="Tenant ID for multi-tenant isolation",
)
@click.option(
    "--json",
    "output_json_flag",
    is_flag=True,
    help="Output in JSON format",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Verbose output",
)
@click.option(
    "--engine-transport",
    type=str,
    envvar="AETHYME_ENGINE_TRANSPORT",
    help=(
        "Engine transport backend. Built-ins: auto, subprocess, pyo3. "
        "Custom registered transport names are also accepted."
    ),
)
@click.pass_context
def cli(
    ctx: click.Context,
    tenant_id: str | None,
    output_json_flag: bool,
    verbose: bool,
    engine_transport: str | None,
) -> None:
    """Aethyme - Graph-based code intelligence system.

    A powerful CLI for code indexing, querying, and AI-readiness analysis.

    Examples:
        aethyme index --repo /path/to/repo
        aethyme query search UserService
        aethyme ai-ready --apply
        aethyme autofix --dry-run
    """
    state = get_state(ctx)
    state["tenant_id"] = tenant_id
    state["json"] = output_json_flag
    state["verbose"] = verbose
    if engine_transport:
        normalized_transport = engine_transport.strip().lower()
        if not normalized_transport:
            raise click.BadParameter(
                "Engine transport cannot be empty.", param_hint="--engine-transport"
            )
        os.environ["AETHYME_ENGINE_TRANSPORT"] = normalized_transport
        state["engine_transport"] = normalized_transport




@cli.group()
def repo() -> None:
    """Local repository intake and inspection workflows."""


@repo.command("deploy-skills")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--force", is_flag=True, help="Overwrite existing skills")
@click.option(
    "--remove", "do_remove", is_flag=True, help="Remove deployed skills instead"
)
def repo_deploy_skills(repo_path: Path, force: bool, do_remove: bool) -> None:
    """Compatibility path for static runtime skill deployment.

    Prefer `aethyme enhance deploy --repo <path>` for the full discoverability
    and generated onboarding path.
    """
    from src.indexing.skills import deploy_skills, remove_skills

    if do_remove:
        removed = remove_skills(repo_path)
        if removed:
            click.echo(f"Removed skills: {', '.join(removed)}")
        else:
            click.echo("No Aethyme skills found to remove.")
        return

    deployed = deploy_skills(repo_path, force=force)
    if deployed:
        click.echo(f"Deployed skills: {', '.join(deployed)}")
    else:
        click.echo("All skills already present (use --force to overwrite).")
    click.echo("Note: `repo deploy-skills` is a compatibility path. Prefer `aethyme enhance deploy --repo <path>`.")


@repo.command("compile-skills")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option(
    "--skill",
    "skill_names",
    multiple=True,
    type=click.Choice(["repo-onboarding"]),
    help="Generated skill to compile. Defaults to repo-onboarding.",
)
def repo_compile_skills(repo_path: Path, skill_names: tuple[str, ...]) -> None:
    """Compile deterministic repo-specific skills without the full enhancement wrapper."""
    from src.indexing.experience_telemetry import (
        append_event,
        event_payload_from_generated_artifacts,
    )
    from src.indexing.onboarding import expected_onboarding_files

    selected = skill_names or ("repo-onboarding",)
    written: list[str] = []
    if "repo-onboarding" in selected:
        for relative_path, content in expected_onboarding_files(repo_path).items():
            dest = repo_path / relative_path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(content, encoding="utf-8")
            written.append(relative_path)

    for relative_path in written:
        click.echo(f"  compiled   {relative_path}")
    append_event(
        repo_path,
        "repo.compile-skills",
        {
            "selected_skills": list(selected),
            "written_paths": written,
            **event_payload_from_generated_artifacts(repo_path),
        },
    )
    click.echo(f"Compiled repo skills for: {repo_path}")


@repo.command("init-onboarding-overrides")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--force", is_flag=True, help="Overwrite an existing override file")
def repo_init_onboarding_overrides(repo_path: Path, force: bool) -> None:
    """Write a starter onboarding override file for maintainers."""
    from src.indexing.experience_telemetry import append_event
    from src.indexing.onboarding import ONBOARDING_OVERRIDE_PATH, override_template

    dest = repo_path / ONBOARDING_OVERRIDE_PATH
    if dest.exists() and not force:
        raise click.ClickException(
            f"{ONBOARDING_OVERRIDE_PATH} already exists. Use --force to overwrite."
        )
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps(override_template(), indent=2) + "\n", encoding="utf-8")
    append_event(
        repo_path,
        "repo.init-onboarding-overrides",
        {"force": force, "path": ONBOARDING_OVERRIDE_PATH},
    )
    click.echo(f"Wrote {ONBOARDING_OVERRIDE_PATH}")


@repo.command("validate-onboarding-overrides")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
def repo_validate_onboarding_overrides(repo_path: Path) -> None:
    """Validate the onboarding override file if present."""
    from src.indexing.experience_telemetry import append_event
    from src.indexing.onboarding import validate_overrides

    result = validate_overrides(repo_path)
    append_event(
        repo_path,
        "repo.validate-onboarding-overrides",
        {"ok": result["ok"], "exists": result["exists"], "errors": result["errors"]},
    )
    if result["ok"]:
        if result["exists"]:
            click.echo(f"Valid override file: {result['path']}")
        else:
            click.echo(f"No override file present: {result['path']}")
        return
    click.echo(f"Invalid override file: {result['path']}", err=True)
    for error in result["errors"]:
        click.echo(f"  - {error}", err=True)
    raise SystemExit(1)


@repo.command("init-agents-overrides")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--force", is_flag=True, help="Overwrite an existing override file")
def repo_init_agents_overrides(repo_path: Path, force: bool) -> None:
    """Write a starter agents override file for repo-specific root instructions."""
    from src.enhance import AGENTS_OVERRIDE_PATH, agents_override_template
    from src.indexing.experience_telemetry import append_event

    dest = repo_path / AGENTS_OVERRIDE_PATH
    if dest.exists() and not force:
        raise click.ClickException(
            f"{AGENTS_OVERRIDE_PATH} already exists. Use --force to overwrite."
        )
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(
        json.dumps(agents_override_template(), indent=2) + "\n",
        encoding="utf-8",
    )
    append_event(
        repo_path,
        "repo.init-agents-overrides",
        {"force": force, "path": AGENTS_OVERRIDE_PATH},
    )
    click.echo(f"Wrote {AGENTS_OVERRIDE_PATH}")


@repo.command("validate-agents-overrides")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
def repo_validate_agents_overrides(repo_path: Path) -> None:
    """Validate the agents override file if present."""
    from src.enhance import validate_agents_overrides
    from src.indexing.experience_telemetry import append_event

    result = validate_agents_overrides(repo_path)
    append_event(
        repo_path,
        "repo.validate-agents-overrides",
        {"ok": result["ok"], "exists": result["exists"], "errors": result["errors"]},
    )
    if result["ok"]:
        if result["exists"]:
            click.echo(f"Valid override file: {result['path']}")
        else:
            click.echo(f"No override file present: {result['path']}")
        return
    click.echo(f"Invalid override file: {result['path']}", err=True)
    for error in result["errors"]:
        click.echo(f"  - {error}", err=True)
    raise SystemExit(1)


@repo.command("record-wrapper-invocation", hidden=True)
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--wrapper", "wrapper_name", required=True, help="Wrapper or hook name")
@click.option("--detail", "details", multiple=True, help="Optional key=value detail")
def repo_record_wrapper_invocation(
    repo_path: Path,
    wrapper_name: str,
    details: tuple[str, ...],
) -> None:
    """Internal: record that an Aethyme-provided wrapper or hook was invoked."""
    from src.indexing.experience_telemetry import record_wrapper_invocation

    parsed_details: dict[str, str] = {}
    for item in details:
        if "=" not in item:
            continue
        key, value = item.split("=", 1)
        parsed_details[key] = value
    record_wrapper_invocation(
        repo_path,
        wrapper_name=wrapper_name,
        details=parsed_details or None,
    )


@repo.command("experience-telemetry")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option(
    "--check",
    "check_signals",
    is_flag=True,
    help="Exit non-zero when attention signals require operator action.",
)
def repo_experience_telemetry(repo_path: Path, json_output: bool, check_signals: bool) -> None:
    """Show a stable report over repo-local experience telemetry."""
    from src.indexing.experience_telemetry import detailed_report

    report = detailed_report(repo_path)
    if json_output:
        click.echo(json.dumps(report, indent=2))
        if check_signals and _experience_report_has_attention(report):
            raise SystemExit(1)
        return

    click.echo(f"Path: {report['path']}")
    click.echo(f"Exists: {'yes' if report['exists'] else 'no'}")
    click.echo(f"Events: {report['event_count']}")
    click.echo(f"Last event: {report['last_event_type'] or 'none'}")
    if report["by_type"]:
        click.echo("By type:")
        for event_type, count in sorted(report["by_type"].items()):
            click.echo(f"- {event_type}: {count}")
    if report["wrapper_invocations"]:
        click.echo("Wrapper invocations:")
        for wrapper_name, count in sorted(report["wrapper_invocations"].items()):
            click.echo(f"- {wrapper_name}: {count}")
    if report.get("kpis"):
        kpis = report["kpis"]
        click.echo("KPIs:")
        click.echo(f"- wrapper_total: {kpis['wrapper_total']}")
        click.echo(f"- onboarding_commands: {kpis['onboarding_commands']}")
        click.echo(f"- onboarding_notes: {kpis['onboarding_notes']}")
        click.echo(f"- act_has_fast_test: {kpis['act_has_fast_test']}")
        click.echo(f"- override_regeneration_required: {kpis['override_regeneration_required']}")
        if report.get("freshness", {}).get("override_exists"):
            click.echo(
                f"- stale_targets: {', '.join(kpis['stale_targets']) or 'none'}"
            )
        if kpis["signals"]:
            click.echo("Signals:")
            for signal in kpis["signals"]:
                click.echo(f"- {signal['status']} {signal['code']}: {signal['message']}")
        if kpis["suggestions"]:
            click.echo("Suggestions:")
            for suggestion in kpis["suggestions"]:
                click.echo(f"- {suggestion['code']}: {suggestion['message']}")
    freshness = report.get("freshness") or {}
    if freshness.get("override_exists"):
        click.echo("Override freshness:")
        click.echo(
            f"- regeneration_required: {'yes' if freshness['regeneration_required'] else 'no'}"
        )
        if freshness.get("stale_targets"):
            click.echo(f"- stale_targets: {', '.join(freshness['stale_targets'])}")
    if report["recent_events"]:
        click.echo("Recent events:")
        for event in report["recent_events"][-5:]:
            click.echo(f"- {event['timestamp']} {event['event_type']}")
    if check_signals and _experience_report_has_attention(report):
        raise SystemExit(1)


@repo.command("experience-status")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def repo_experience_status(repo_path: Path, json_output: bool) -> None:
    """Write and show a compact repo experience status artifact."""
    from src.indexing.experience_telemetry import write_status_artifacts

    status = write_status_artifacts(repo_path)
    if json_output:
        click.echo(json.dumps(status, indent=2))
        return

    click.echo("Experience status:")
    click.echo(
        "  Enhancement: "
        f"installed={status['enhancement']['installed']}, "
        f"verified={status['enhancement']['verified']}"
    )
    click.echo(
        "  Artifacts: "
        f"onboarding={status['artifacts']['onboarding_present']}, "
        f"act={status['artifacts']['act_present']}, "
        f"override_exists={status['artifacts']['override_exists']}, "
        f"regeneration_required={status['artifacts']['override_regeneration_required']}"
    )
    click.echo(
        "  Recommended next action: "
        f"{status['recommended_next_action']['command']}"
    )
    click.echo(f"  Reason: {status['recommended_next_action']['reason']}")
    click.echo(
        "  Wrote: "
        ".aethyme/generated/experience-status.json, "
        ".aethyme/generated/experience-status.md"
    )


@repo.command("commit-message-template")
@click.option(
    "--type",
    "commit_type",
    type=click.Choice(
        ["fix", "feat", "refactor", "perf", "test", "docs", "build", "chore", "revert"]
    ),
    default="fix",
    show_default=True,
    help="Commit type for the generated template subject.",
)
@click.option(
    "--scope",
    default="scope",
    show_default=True,
    help="Scope token used in the generated subject line.",
)
def repo_commit_message_template(commit_type: str, scope: str) -> None:
    """Print the typed commit message template used by Aethyme commit hygiene."""
    from src.indexing.commit_hygiene import default_template

    click.echo(default_template(commit_type=commit_type, scope=scope), nl=False)


@repo.command("lint-commit-message")
@click.argument(
    "message_path",
    required=False,
    type=click.Path(exists=True, dir_okay=False, path_type=Path),
)
@click.option("--message", "inline_message", help="Lint an inline commit message string.")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def repo_lint_commit_message(
    message_path: Path | None,
    inline_message: str | None,
    json_output: bool,
) -> None:
    """Lint a commit message against the typed Aethyme hygiene contract."""
    from src.indexing.commit_hygiene import lint_commit_message

    if message_path is not None and inline_message is not None:
        raise click.ClickException("Provide either MESSAGE_PATH or --message, not both.")

    if message_path is not None:
        message = message_path.read_text(encoding="utf-8")
    elif inline_message is not None:
        message = inline_message
    else:
        message = click.get_text_stream("stdin").read()

    result = lint_commit_message(message)
    if json_output:
        click.echo(json.dumps(result, indent=2))
        if not result["ok"]:
            raise SystemExit(1)
        return

    subject = result.get("subject")
    click.echo(f"Valid: {'yes' if result['ok'] else 'no'}")
    if isinstance(subject, dict):
        click.echo(f"Type: {subject['type']}")
        click.echo(f"Scope: {subject['scope'] or 'none'}")
        click.echo(f"Summary: {subject['summary']}")
    click.echo(f"Body required: {'yes' if result['body_required'] else 'no'}")
    recognized = result.get("recognized_sections") or []
    click.echo(
        "Sections: " + (", ".join(cast(list[str], recognized)) if recognized else "none")
    )
    memory_candidates = cast(list[dict[str, str]], result.get("memory_candidates") or [])
    if memory_candidates:
        click.echo("Memory candidates:")
        for candidate in memory_candidates:
            click.echo(f"- {candidate['type']}: {candidate['summary']}")
    warnings = cast(list[str], result.get("warnings") or [])
    if warnings:
        click.echo("Warnings:")
        for warning in warnings:
            click.echo(f"- {warning}")
    errors = cast(list[str], result.get("errors") or [])
    if errors:
        click.echo("Errors:")
        for error in errors:
            click.echo(f"- {error}")
        raise SystemExit(1)


@cli.group()
def analyze() -> None:
    """Deterministic analyzers that answer recurring repository questions."""


@analyze.command("dead-code")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--scope", "scope", required=True, help="Directory scope prefix")
@click.option(
    "--boundary",
    "boundary",
    type=click.Choice(["outside-directory"]),
    default="outside-directory",
    show_default=True,
    help="Caller boundary semantics. outside-directory means callers outside --scope count as external.",
)
@click.option("--roots", "roots", default="", help="Comma-separated search roots")
@click.option(
    "--include-methods",
    "include_methods",
    is_flag=True,
    help="Include public methods as well as top-level functions",
)
@click.option(
    "--format",
    "output_format",
    type=click.Choice(["summary", "full-json", "eval-json"]),
    default="summary",
    show_default=True,
    help="Output format. eval-json emits a task-ready unused_functions answer.",
)
@click.option(
    "--show-observability",
    "show_observability",
    is_flag=True,
    help="Include command observability in JSON formats.",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def analyze_dead_code_command(
    repo_path: Path,
    scope: str,
    boundary: str,
    roots: str,
    include_methods: bool,
    output_format: str,
    show_observability: bool,
    json_output: bool,
) -> None:
    """Analyze likely dead code with evidence and ambiguity markers."""
    roots_list = [item.strip() for item in roots.split(",") if item.strip()]
    try:
        result = analyze_dead_code_answer(
            repo_path,
            scope,
            roots=roots_list,
            include_methods=include_methods,
        )
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output and output_format == "summary":
        output_format = "full-json"

    if output_format == "full-json":
        result = {
            **result,
            "command_observability": _dead_code_command_observability(
                repo_path=repo_path,
                scope=scope,
                roots=roots_list,
                boundary=boundary,
                include_methods=include_methods,
                output_format=output_format,
                result=result,
            ),
        }
        _echo_json_with_output_size(result)
        return

    if output_format == "eval-json":
        payload = _dead_code_eval_json(
            result,
            repo_path=repo_path,
            scope=scope,
            roots=roots_list,
            boundary=boundary,
            include_methods=include_methods,
            show_observability=show_observability,
        )
        _echo_json_with_output_size(payload)
        return

    click.echo(f"Analyzer: {result['analyzer']} v{result['version']}")
    click.echo(f"Scope: {result['query']['scope']}")
    click.echo(
        f"Candidates: {result['summary']['total_candidates']} total, "
        f"{result['summary']['unused']} unused, "
        f"{result['summary']['ambiguous']} ambiguous, "
        f"{result['summary']['used']} used"
    )
    click.echo("Candidates:")
    for candidate in result["candidates"]:
        click.echo(
            f"- {candidate['function']['defined_in']}::{candidate['function']['name']} "
            f"[{candidate['status']}] conf={candidate['confidence']:.2f}"
        )


def _dead_code_eval_json(
    result: dict[str, Any],
    *,
    repo_path: Path,
    scope: str,
    roots: list[str],
    boundary: str,
    include_methods: bool,
    show_observability: bool,
) -> dict[str, Any]:
    answer_items = [
        _dead_code_eval_item(candidate)
        for candidate in result.get("candidates", [])
        if candidate.get("status") in {"Unused", "Ambiguous"}
    ]
    payload: dict[str, Any] = {
        "unused_functions": answer_items,
        "excluded_functions": [
            _dead_code_eval_item(candidate) for candidate in result.get("excluded", [])
        ],
    }
    if show_observability:
        payload["observability"] = _dead_code_command_observability(
            repo_path=repo_path,
            scope=scope,
            roots=roots,
            boundary=boundary,
            include_methods=include_methods,
            output_format="eval-json",
            result=result,
        )
    return payload


def _dead_code_eval_item(candidate: dict[str, Any]) -> dict[str, Any]:
    function = candidate.get("function") or {}
    evidence = candidate.get("evidence") or {}
    return {
        "function_name": function.get("name"),
        "defined_in": function.get("defined_in"),
        "status": candidate.get("status"),
        "external_callers": evidence.get("external_callers", []),
        "internal_callers": evidence.get("internal_callers", []),
        "evidence": {
            "searched_roots": evidence.get("searched_roots", []),
            "external_callers": evidence.get("external_callers", []),
            "internal_callers": evidence.get("internal_callers", []),
            "docs_config_references": evidence.get("docs_config_references", []),
            "ambiguity": candidate.get("ambiguity", []),
        },
        "confidence": candidate.get("confidence"),
        "reason": candidate.get("rationale"),
    }


def _dead_code_command_observability(
    *,
    repo_path: Path,
    scope: str,
    roots: list[str],
    boundary: str,
    include_methods: bool,
    output_format: str,
    result: dict[str, Any],
) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    rust_observability = (
        result.get("observability")
        if isinstance(result.get("observability"), dict)
        else {}
    )
    return {
        "command": "analyze dead-code",
        "repo_path": str(snapshot.repo_path),
        "scope": scope,
        "boundary": boundary,
        "roots": roots,
        "include_methods": include_methods,
        "output_format": output_format,
        "index_freshness": {
            "status": "fresh_for_current_snapshot",
            "repo_dirty": snapshot.dirty,
            "commit": snapshot.commit,
            "snapshot_key": snapshot.cache_key,
            "file_count": snapshot.file_count,
        },
        "engine": engine_runtime_info(),
        "graph_fact_count": {
            "graph": rust_observability.get("graph_counts", {}),
            "facts": rust_observability.get("fact_counts", {}),
        },
        "confidence_summary": rust_observability.get("confidence_summary", {}),
        "degraded_reasons": rust_observability.get("degraded_reasons", []),
        "failure_reason": None,
        "output_size_bytes": None,
    }


def _echo_json_with_output_size(payload: dict[str, Any]) -> None:
    if isinstance(payload.get("command_observability"), dict):
        _set_output_size(payload, payload["command_observability"])
    if isinstance(payload.get("observability"), dict):
        _set_output_size(payload, payload["observability"])
    click.echo(json.dumps(payload, indent=2))


def _set_output_size(payload: dict[str, Any], observability: dict[str, Any]) -> None:
    for _ in range(5):
        output_size = len(json.dumps(payload, indent=2).encode("utf-8"))
        if observability.get("output_size_bytes") == output_size:
            return
        observability["output_size_bytes"] = output_size


@cli.command(name="ai-ready")
@click.option(
    "--repo",
    type=click.Path(exists=True),
    help="Repository path (defaults to current directory)",
)
@click.option(
    "--repo-id",
    help="Repository ID for API mode",
)
@click.option(
    "--format",
    "-f",
    type=click.Choice(["json", "md", "both"]),
    default="md",
    help="Output format",
)
@click.option(
    "--output",
    "-o",
    type=click.Path(),
    help="Output file (defaults to stdout)",
)
@click.option(
    "--detectors",
    help="Comma-separated list of detectors to run (runs all if not specified)",
)
@click.pass_context
def ai_ready(
    ctx: click.Context,
    repo: str | None,
    repo_id: str | None,
    format: str,
    output: str | None,
    detectors: str | None,
) -> None:
    """Run AI-readiness scorecard on a repository."""
    from pathlib import Path

    from src.scorecard.engine import ScorecardEngine
    from src.scorecard.metrics import record_scan_metrics

    # Determine repository path
    if repo:
        repo_path = Path(repo).resolve()
    else:
        repo_path = Path.cwd()

    if not repo_path.exists():
        click.echo(f"Error: Repository path does not exist: {repo_path}", err=True)
        sys.exit(2)

    click.echo(f"Running AI-readiness scorecard on: {repo_path}")
    click.echo()

    # Tenant/org lookups retired with the PostgreSQL lineage (2026-07-13).
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    # Parse detectors list
    detector_list = None
    if detectors:
        detector_list = [d.strip() for d in detectors.split(",")]

    # Run scan
    try:
        engine = ScorecardEngine(
            repo_path=repo_path, repository_id=repo_id, tenant_id=tenant_id
        )

        with click.progressbar(
            length=100, label="Scanning repository", show_eta=True
        ) as bar:
            result = engine.scan(detectors=detector_list)
            bar.update(100)

        # Record metrics
        record_scan_metrics(result.report, tenant_id=tenant_id, repository_id=repo_id)

    except Exception as exc:
        click.echo(f"Error during scan: {exc}", err=True)
        logger.error("Scorecard scan failed", error=str(exc), exc_info=True)
        sys.exit(2)

    click.echo()
    click.echo(f"Scan completed: Score {result.score}/100")
    click.echo(
        f"Findings: {result.report.total_findings} total "
        f"({result.report.blocker_count} blockers, "
        f"{result.report.warning_count} warnings, "
        f"{result.report.info_count} info)"
    )
    click.echo()

    # Generate output
    if format in ["json", "both"]:
        json_output = result.to_json()
        if output and format == "json":
            output_path = Path(output)
            output_path.write_text(json_output)
            click.echo(f"JSON report written to: {output_path}")
        elif format == "both":
            output_base = Path(output) if output else Path("scorecard-report")
            json_path = (
                output_base
                if output_base.suffix == ".json"
                else output_base.with_suffix(".json")
            )
            json_path.write_text(json_output)
            click.echo(f"JSON report written to: {json_path}")
        else:
            click.echo(json_output)

    if format in ["md", "both"]:
        md_output = result.to_markdown()
        if output and format == "md":
            output_path = Path(output)
            output_path.write_text(md_output)
            click.echo(f"Markdown report written to: {output_path}")
        elif format == "both":
            output_base = Path(output) if output else Path("scorecard-report")
            md_path = (
                output_base
                if output_base.suffix == ".md"
                else output_base.with_suffix(".md")
            )
            md_path.write_text(md_output)
            click.echo(f"Markdown report written to: {md_path}")
        else:
            click.echo(md_output)

    # Exit code based on findings
    # 0 = ready (score >= 90)
    # 1 = warnings (score >= 50)
    # 2 = blockers (score < 50)
    if result.report.blocker_count > 0 or result.score < 50:
        sys.exit(2)
    elif result.report.warning_count > 0 or result.score < 90:
        sys.exit(1)
    else:
        sys.exit(0)


@cli.command()
@click.argument("repo_path", type=click.Path(exists=True))
@click.option("--dry-run", is_flag=True, help="Show changes without applying")
@click.option("--apply", "do_apply", is_flag=True, help="Apply changes to disk")
@click.option("--pr", is_flag=True, help="Create pull request")
@click.option(
    "--fix-type",
    type=click.Choice(["all", "docs", "links", "selectors", "i18n", "format"]),
    default="all",
    help="Type of fix to apply",
)
@click.option("--skip-approval", is_flag=True, help="Skip approval for risky changes")
@click.pass_context
def autofix(
    ctx: click.Context,
    repo_path: str,
    dry_run: bool,
    do_apply: bool,
    pr: bool,
    fix_type: str,
    skip_approval: bool,
) -> None:
    """Apply automated fixes to codebase issues."""
    from pathlib import Path

    from src.autofixers.fixers import (
        DocsRegenerator,
        FormatFixer,
        I18nScaffolder,
        LinkFixer,
        SelectorInserter,
    )
    from src.autofixers.github import GitHubIntegration
    from src.autofixers.patch import PatchGenerator
    from src.autofixers.safety import SafetyEngine

    repository_path = Path(repo_path).resolve()

    click.echo("\nAethyme Autofixer")
    click.echo("=" * 60)
    click.echo(f"Repository: {repository_path}")
    click.echo(f"Fix type: {fix_type}")
    click.echo(
        f"Mode: {'DRY RUN' if dry_run else 'APPLY' if do_apply else 'PR' if pr else 'DRY RUN'}"
    )
    click.echo("")

    # Initialize components
    safety_engine = SafetyEngine()
    patch_gen = PatchGenerator(repository_path, safety_engine)

    # Collect fixes based on type
    all_fixes: list[FixRecord] = []

    if fix_type in ["all", "docs"]:
        click.echo("Scanning for documentation issues...")
        docs_fixer = DocsRegenerator(repository_path)
        docs_fixes = normalize_fixes(docs_fixer.create_folder_docs())
        all_fixes.extend(docs_fixes)
        click.echo(f"  Found {len(docs_fixes)} documentation fixes")

    if fix_type in ["all", "links"]:
        click.echo("Scanning for link issues...")
        link_fixer = LinkFixer(repository_path)
        link_fixes = normalize_fixes(link_fixer.process_directory())
        all_fixes.extend(link_fixes)
        click.echo(f"  Found {len(link_fixes)} link fixes")

    if fix_type in ["all", "selectors"]:
        click.echo("Scanning for missing test selectors...")
        selector_fixer = SelectorInserter(repository_path)
        selector_fixes = normalize_fixes(selector_fixer.process_directory())
        all_fixes.extend(selector_fixes)
        click.echo(f"  Found {len(selector_fixes)} selector fixes")

    if fix_type in ["all", "i18n"]:
        click.echo("Scanning for hardcoded strings...")
        i18n_fixer = I18nScaffolder(repository_path)
        i18n_fixes = normalize_fixes(i18n_fixer.process_directory())
        all_fixes.extend(i18n_fixes)
        click.echo(f"  Found {len(i18n_fixes)} i18n fixes")

    if fix_type in ["all", "format"]:
        click.echo("Scanning for formatting issues...")
        format_fixer = FormatFixer(repository_path)
        format_fixes = normalize_fixes(format_fixer.process_directory())
        all_fixes.extend(format_fixes)
        click.echo(f"  Found {len(format_fixes)} formatting fixes")

    # Add patches
    click.echo("\nGenerating patches...")
    for fix in all_fixes:
        patch_gen.add_patch(
            fix["file_path"],
            fix["original_content"],
            fix["new_content"],
            fix["fix_type"],
        )

    summary = patch_gen.get_summary()

    # Display summary
    click.echo("\n" + "=" * 60)
    click.echo("Summary")
    click.echo("=" * 60)
    click.echo(f"Total files: {summary['total_files']}")
    click.echo(f"Low risk: {summary['total_low_risk']}")
    click.echo(f"Medium risk: {summary['total_medium_risk']}")
    click.echo(f"High risk: {summary['total_high_risk']}")
    click.echo("")

    for fix_type_key, count in summary["by_fix_type"].items():
        click.echo(f"  {fix_type_key}: {count} files")

    if summary["total_files"] == 0:
        click.echo("\nNo fixes needed!")
        return

    # Execute based on mode
    if dry_run or (not do_apply and not pr):
        # Dry run mode
        result = patch_gen.dry_run()
        click.echo("\n" + "=" * 60)
        click.echo("Changes Preview (Dry Run)")
        click.echo("=" * 60)

        diff = result["diff"]
        if diff:
            click.echo(diff)
        else:
            click.echo("No changes to show")

        click.echo("\nRun with --apply to apply these changes")
        click.echo("Run with --pr to create a pull request")

    elif do_apply:
        # Apply mode
        if summary["requires_approval"] > 0 and not skip_approval:
            click.echo(
                f"\nWarning: {summary['requires_approval']} files require approval"
            )
            if not click.confirm("Continue anyway?"):
                return

        click.echo("\nApplying fixes...")
        result = patch_gen.apply(skip_approval=skip_approval)

        if result["status"] == "requires_approval":
            click.echo("\nSome fixes require approval:")
            for patch_info in result["requires_approval"]:
                click.echo(f"  {patch_info['file']} ({patch_info['risk_level']})")
            click.echo("\nUse --skip-approval to apply anyway (not recommended)")
            return

        if result["status"] in ["success", "partial"]:
            click.echo(f"\nApplied {len(result['applied'])} files")
            if result.get("failed"):
                click.echo(f"Failed {len(result['failed'])} files:")
                for f in result["failed"]:
                    click.echo(f"  {f}")
        else:
            click.echo(f"\nFailed to apply fixes: {result}")

    elif pr:
        # PR mode
        click.echo("\nCreating pull request...")

        gh_integration = GitHubIntegration(repository_path)

        # Check working tree
        if not gh_integration.is_clean_working_tree():
            click.echo("Error: Working tree has uncommitted changes")
            click.echo("Please commit or stash changes before creating PR")
            return

        # Create PR
        result = gh_integration.create_autofix_pr(patch_gen)

        if result and result.get("url"):
            click.echo(f"\nPull request created: {result['url']}")
            click.echo(f"Branch: {result['branch']}")
            click.echo(f"Commit: {result['commit'][:8]}")
        elif result and result.get("status") == "requires_approval":
            click.echo("\nSome fixes require approval before creating PR")
            click.echo("Use the web UI to approve, or use --apply --skip-approval")
        else:
            click.echo("\nFailed to create pull request")


@cli.group()
def enhance() -> None:
    """Deploy or verify Aethyme discoverability files in a target repository.

    A repo is "Aethyme-enhanced" when an agent landing in its working
    directory finds AGENTS.md/CLAUDE.md (root-level announcement) plus
    .claude/skills/aethyme/SKILL.md and .codex/skills/aethyme/SKILL.md
    (per-product short runbooks with optional references), plus generated
    repo-onboarding artifacts
    under `.aethyme/generated/` and per-product `repo-onboarding` skills.
    Static tool runbooks are derived from canonical templates; onboarding is
    generated deterministically from repository facts.
    """


@enhance.command("deploy")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Target repository to enhance",
)
@click.option(
    "--force",
    is_flag=True,
    help="Rewrite files whose content already matches",
)
def enhance_deploy_command(repo_path: Path, force: bool) -> None:
    """Write discoverability files plus generated onboarding into the target repo."""
    from src.enhance import deploy

    actions = deploy(repo_path, force=force)
    for a in actions:
        click.echo(f"  {a.action:9}  {a.target.relative_path}")
    click.echo(f"Enhanced: {repo_path}")


@enhance.command("verify")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Target repository to verify",
)
def enhance_verify_command(repo_path: Path) -> None:
    """Check that discoverability and onboarding files are present and usable."""
    from src.enhance import is_ok, refresh_status, summarize, verify
    from src.indexing.experience_telemetry import append_event

    results = verify(repo_path)
    for r in results:
        ok = r.exists and not r.placeholder_present
        strict_direct_edit = (
            r.target.relative_path in {"AGENTS.md", "CLAUDE.md"} and r.exists and not r.matches_canonical
        )
        marker = "FAIL" if strict_direct_edit else ("OK" if ok else "FAIL")
        notes: list[str] = []
        if not r.exists:
            notes.append("missing")
        elif r.placeholder_present:
            notes.append("placeholder not substituted")
        if r.exists and not r.matches_canonical:
            if r.target.relative_path in {"AGENTS.md", "CLAUDE.md"}:
                notes.append(
                    "direct edits unsupported; use .aethyme/overrides/agents.json"
                )
            else:
                notes.append("content drift (allowed)")
        suffix = f"  ({', '.join(notes)})" if notes else ""
        click.echo(f"  [{marker:4}] {r.target.relative_path}{suffix}")
    if not is_ok(results):
        click.echo("Verification failed.", err=True)
        sys.exit(1)
    summary = summarize(repo_path)
    append_event(
        repo_path,
        "enhance.verify",
        {
            "ok": True,
            "recommended_skill": summary["recommended_skill"],
            "recommended_mode": summary["recommended_mode"],
            "experience_telemetry_before_write": summary["experience_telemetry"],
        },
    )
    click.echo("Enhancement summary:")
    click.echo(
        "  Recommendation: "
        f"load `{summary['recommended_skill']}` then run `{summary['recommended_mode']}`"
    )
    click.echo(f"  Reason: {summary['reason']}")
    click.echo(
        "  Onboarding: "
        f"commands={summary['onboarding']['commands']}, "
        f"areas={summary['onboarding']['areas']}, "
        f"entrypoints={summary['onboarding']['entrypoints']}, "
        f"notes={summary['onboarding']['notes']}, "
        f"overrides={summary['onboarding']['overrides_applied']}"
    )
    click.echo(
        "  Act starter: "
        f"fast_test={summary['act']['has_fast_test']}, "
        f"entrypoints={summary['act']['entrypoints']}, "
        f"caution_zones={summary['act']['caution_zones']}"
    )
    if summary["freshness"]["override_exists"]:
        click.echo(
            "  Override freshness: "
            f"regeneration_required={summary['freshness']['regeneration_required']}, "
            f"stale_targets={','.join(summary['freshness']['stale_targets']) or 'none'}"
        )
    click.echo(
        "  Experience telemetry: "
        f"events={summary['experience_telemetry']['event_count']}, "
        f"last={summary['experience_telemetry']['last_event_type']}"
    )
    status = refresh_status(repo_path)
    click.echo(
        "  Experience status: "
        f"next=`{status['recommended_next_action']['command']}`, "
        "artifacts=.aethyme/generated/experience-status.json,.aethyme/generated/experience-status.md"
    )
    click.echo("All discoverability files present and substituted.")


def _experience_report_has_attention(report: dict[str, Any]) -> bool:
    kpis = report.get("kpis") or {}
    signals = kpis.get("signals") or []
    return any(signal.get("status") == "attention" for signal in signals)


def main() -> None:
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()
