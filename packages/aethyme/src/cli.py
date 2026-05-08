import json
import os
import shlex
import sys
from pathlib import Path
from typing import Any, TypeAlias, TypedDict, cast

import click
import structlog

# Add src to path for module imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from src.eval.bug_fix import (
    DEFAULT_CROSS_PACKAGE_TASK,
    prepare_bug_fix_benchmark,
    prepare_cross_package_benchmark,
    reset_bug_fix,
    run_bug_fix_evaluation,
    setup_bug_fix,
    verify_bug_fix_setup,
)
from src.eval.bug_fix import (
    DEFAULT_TASK as DEFAULT_BUG_FIX_TASK,
)
from src.eval.explain_repo import (
    DEFAULT_TASK,
    command_runner,
    run_explain_repo_evaluation,
)
from src.eval.navigation_ctf import (
    DEFAULT_TASK as DEFAULT_NAVIGATION_TASK,
)
from src.eval.navigation_ctf import (
    command_runner as navigation_command_runner,
)
from src.eval.navigation_ctf import (
    run_navigation_ctf_evaluation,
)
from src.eval.repos import create_condition_repos
from src.graph.connection_pool import db_pool
from src.graph.store import GraphStore
from src.indexing.engine import (
    EngineError,
    build_task_context,
    build_task_pack,
    clear_repository_cache,
    derived_function_usage,
    derived_public_functions,
    engine_runtime_info,
    graph_callees,
    graph_callers,
    graph_children,
    graph_configs,
    graph_docs,
    graph_expand,
    graph_node,
    graph_overview,
    graph_parents,
    inspect_repository,
    inspect_repository_brief,
    inspect_repository_structure,
    search_symbol,
    search_symbols,
    task_anchors,
    task_expand,
    task_localize,
    task_next,
    task_scope,
)
from src.indexing.engine import (
    analyze_dead_code as analyze_dead_code_answer,
)
from src.indexing.engine import (
    dependency_frontier as rust_dependency_frontier,
)
from src.indexing.engine import (
    impact_frontier as rust_impact_frontier,
)
from src.indexing.engine import (
    usage_boundary_query as usage_boundary_query_answer,
)
from src.indexing.repository_snapshot import capture_snapshot
from src.indexing.service import RepositoryIndexRequest, run_indexing
from src.rendering.context_pack import render_explain_repo_text, render_pack_summary

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


def default_tenant_id() -> str | None:
    """Resolve the default tenant for local CLI workflows."""
    result = db_pool.execute(
        "SELECT id FROM aethyme.tenants WHERE slug = 'default' LIMIT 1"
    )
    if not result:
        return None
    return str(result[0]["id"])


@click.group()
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


@cli.command()
@click.argument("repo_path", type=click.Path(exists=True))
@click.option(
    "--name",
    "-n",
    help="Repository name (defaults to directory name)",
)
@click.option(
    "--languages",
    "-l",
    default="python,typescript",
    help="Languages to index (comma-separated)",
)
@click.option(
    "--use-fallback",
    is_flag=True,
    help="Force use of fallback indexer",
)
@click.pass_context
def index(
    ctx: click.Context,
    repo_path: str,
    name: str | None,
    languages: str,
    use_fallback: bool,
) -> None:
    """Index a repository and build the code graph."""
    request = RepositoryIndexRequest(
        repo_path=Path(repo_path),
        repo_name=name,
        languages=[lang.strip() for lang in languages.split(",")],
        tenant_id=cast(str | None, get_state(ctx).get("tenant_id")),
        use_fallback=use_fallback,
        clear_existing=True,
    )

    # Initialize graph builder
    def progress_callback(current: int, total: int, message: str) -> None:
        click.echo(f"Progress: {current}/{total} - {message}")

    result = run_indexing(request, progress_callback=progress_callback)

    # Print summary
    click.echo("\n" + "=" * 60)
    click.echo("Indexing Complete!")
    click.echo("=" * 60)
    click.echo(f"Repository: {result.repository_name}")
    click.echo(f"Path: {result.repository_path}")
    click.echo(f"Languages: {', '.join(result.languages)}")
    click.echo("-" * 60)
    click.echo(f"Total Nodes: {result.graph_statistics.get('total_nodes', 0):,}")
    click.echo(f"Total Edges: {result.graph_statistics.get('total_edges', 0):,}")
    click.echo(f"Total Files: {result.graph_statistics.get('total_files', 0):,}")
    click.echo("-" * 60)
    for language_result in result.language_results:
        click.echo(
            f"{language_result.language}: {language_result.engine} "
            f"({language_result.nodes} nodes, {language_result.edges} edges, {language_result.files} files)"
        )


@cli.command()
@click.pass_context
def stats(ctx: click.Context) -> None:
    """Show graph statistics."""
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    if not tenant_id:
        tenant_id = default_tenant_id()
        if tenant_id is None:
            click.echo("No tenant found. Please index a repository first.")
            return

    store = GraphStore(tenant_id=tenant_id)
    stats = store.get_statistics()

    click.echo("Graph Statistics")
    click.echo("=" * 40)
    click.echo(f"Total Nodes: {stats.get('total_nodes', 0):,}")
    click.echo(f"Total Edges: {stats.get('total_edges', 0):,}")
    click.echo(f"Node Types: {stats.get('node_types', 0)}")
    click.echo(f"Edge Types: {stats.get('edge_types', 0)}")
    click.echo(f"Total Files: {stats.get('total_files', 0):,}")
    click.echo(f"Languages: {stats.get('languages', 0)}")


@cli.command("intents")
@click.option(
    "--request",
    "request_text",
    default="",
    help=(
        "Optional user request to echo in the catalog. Intent selection remains "
        "the caller/LLM's responsibility."
    ),
)
@click.option(
    "--format",
    "output_format",
    type=click.Choice(["compact-json"]),
    default="compact-json",
    show_default=True,
    help="Output format for the intent catalog.",
)
def intents_command(request_text: str, output_format: str) -> None:
    """List high-level Aethyme modes and supported intents."""
    if output_format != "compact-json":
        raise click.ClickException(f"Unsupported intents format: {output_format}")
    catalog = _intent_catalog()
    if request_text:
        catalog["request"] = {"raw": request_text}
        catalog["selection_status"] = "default_available_choose_specialized_when_clear"
    click.echo(json.dumps(catalog, indent=2))


@cli.command("explore")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--intent",
    "intent",
    required=False,
    type=click.Choice(
        ["task_localization_query", "behavior_localization_query", "usage_boundary_query"]
    ),
    help=(
        "Finite Explore intent selected by the caller. If omitted, Aethyme runs "
        "the default general-purpose task localization intent."
    ),
)
@click.option(
    "--request",
    "request_text",
    default="",
    help="Original end-user request. Used for traceability, not free-form routing.",
)
@click.option(
    "--params",
    "params_json",
    default="{}",
    show_default=True,
    help="Intent parameters as a JSON object.",
)
@click.option(
    "--format",
    "output_format",
    type=click.Choice(["answer-json"]),
    default="answer-json",
    show_default=True,
    help="Task-ready answer contract.",
)
@click.option(
    "--detail",
    "output_detail",
    type=click.Choice(["compact", "standard", "full"]),
    default="compact",
    show_default=True,
    help=(
        "Explore output budget. compact is answer-first with short evidence; "
        "standard includes broader evidence summaries; full preserves verbose "
        "debug evidence."
    ),
)
@click.option(
    "--show-observability",
    "show_observability",
    is_flag=True,
    help="Include full command observability. Compact observability is always emitted.",
)
def explore_command(
    repo_path: Path,
    intent: str | None,
    request_text: str,
    params_json: str,
    output_format: str,
    output_detail: str,
    show_observability: bool,
) -> None:
    """Run a high-level Explore intent and return a task-ready answer."""
    # Soft retirement (task #48, 2026-05-07): the canonical entry point for
    # explore is now the native Rust binary `aethyme explore` (which routes to
    # `aethyme-engine-cli explore`). The Python implementation here is kept as
    # a fallback for the next eval cycle so we have a baseline if the Rust
    # path regresses on a real repo. After validation it will be deleted.
    # Set AETHYME_EXPLORE_KEEP_PYTHON=1 to silence the warning.
    if not os.environ.get("AETHYME_EXPLORE_KEEP_PYTHON"):
        click.echo(
            "DEPRECATED: 'python -m src.cli explore' is retiring. Use "
            "'aethyme explore' (native Rust). Set "
            "AETHYME_EXPLORE_KEEP_PYTHON=1 to silence this warning.",
            err=True,
        )
    if output_format != "answer-json":
        raise click.ClickException(f"Unsupported explore format: {output_format}")
    params = _parse_json_object(params_json, option_name="--params")

    selected_intent = intent or "task_localization_query"

    if selected_intent == "task_localization_query":
        payload = _explore_task_localization_query(
            repo_path=repo_path,
            request_text=request_text,
            params=params,
            show_observability=show_observability,
            output_detail=output_detail,
            intent_source="explicit" if intent else "default",
            intent_name="task_localization_query",
        )
        _echo_json_with_output_size(payload)
        return

    if selected_intent == "behavior_localization_query":
        behavior_params = dict(params)
        behavior_params.setdefault("skip_symbols_after_graph_timeout", False)
        behavior_params.setdefault("max_text_files", 10)
        behavior_params.setdefault("max_callsite_symbols", 8)
        behavior_params.setdefault("max_callsite_results", 8)
        payload = _explore_task_localization_query(
            repo_path=repo_path,
            request_text=request_text,
            params=behavior_params,
            show_observability=show_observability,
            output_detail=output_detail,
            intent_source="explicit",
            intent_name="behavior_localization_query",
        )
        _echo_json_with_output_size(payload)
        return

    if selected_intent == "usage_boundary_query":
        payload = _explore_usage_boundary_query(
            repo_path=repo_path,
            request_text=request_text,
            params=params,
            show_observability=show_observability,
        )
        _echo_json_with_output_size(payload)
        return

    raise click.ClickException(f"Unsupported Explore intent: {selected_intent}")


@cli.group()
def repo() -> None:
    """Local repository intake and inspection workflows."""


@repo.command("ingest")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
def repo_ingest(repo_path: Path) -> None:
    """Capture local repository metadata for a local-first workflow."""
    snapshot = capture_snapshot(repo_path)
    click.echo(f"Repository: {snapshot.repo_name}")
    click.echo(f"Path: {snapshot.repo_path}")
    click.echo(f"Commit: {snapshot.commit or 'working-tree'}")
    click.echo(f"Files: {snapshot.file_count}")
    click.echo(f"Snapshot key: {snapshot.cache_key}")


@repo.command("inspect")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option(
    "--mode",
    "mode",
    type=click.Choice(["brief", "structure", "full"]),
    default="full",
    help="Inspect depth: brief (areas+signals), structure (adds files/configs/docs), full (everything)",
)
def repo_inspect(repo_path: Path, json_output: bool, mode: str) -> None:
    """Inspect the local repository map produced by the Rust engine."""
    try:
        if mode == "brief":
            result = inspect_repository_brief(repo_path)
        elif mode == "structure":
            result = inspect_repository_structure(repo_path)
        else:
            result = inspect_repository(repo_path)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    snapshot = result["snapshot"]
    click.echo(f"Root: {snapshot['root']}")
    click.echo(f"Languages: {', '.join(snapshot['languages'])}")
    click.echo(f"Top-level directories: {', '.join(snapshot['top_level_dirs'])}")
    file_count = snapshot.get("file_count") or len(snapshot.get("files", []))
    click.echo(f"Files: {file_count}")
    if mode == "full":
        click.echo(f"Symbols: {len(result.get('symbols', []))}")
        click.echo(f"Edges: {len(result.get('edges', []))}")
    if result.get("entrypoints"):
        click.echo(f"Entrypoints: {', '.join(result['entrypoints'])}")
    if result.get("key_configs"):
        click.echo(f"Key configs: {', '.join(result['key_configs'])}")
    if result.get("signals"):
        click.echo("Signals:")
        for name, signal in result["signals"].items():
            click.echo(
                f"- {name.replace('_', ' ')}: {signal['score']} ({signal['level']})"
            )


@repo.command("clear-cache")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
def repo_clear_cache(repo_path: Path) -> None:
    """Clear cached local engine artifacts for the current repository snapshot."""
    clear_repository_cache(repo_path)
    click.echo(f"Cleared cache for {repo_path}")


@repo.command("warm")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
def repo_warm(repo_path: Path) -> None:
    """Pre-build the repository map cache for fast subsequent commands."""
    from src.indexing.engine import warm_repository

    warm_repository(repo_path)
    click.echo(f"Map cache warmed for {repo_path}")


@repo.command("deploy-skills")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--force", is_flag=True, help="Overwrite existing skills")
@click.option(
    "--remove", "do_remove", is_flag=True, help="Remove deployed skills instead"
)
def repo_deploy_skills(repo_path: Path, force: bool, do_remove: bool) -> None:
    """Deploy Aethyme navigation skills to a target repository."""
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


@repo.command("engine-info")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option(
    "--check",
    "check_ready",
    is_flag=True,
    help="Exit non-zero if selected transport is not ready.",
)
def repo_engine_info(json_output: bool, check_ready: bool) -> None:
    """Show configured engine transport/runtime information."""
    info = engine_runtime_info()
    if json_output:
        click.echo(json.dumps(info, indent=2))
        if check_ready and (
            not info["transport_supported"] or not info["transport_ready"]
        ):
            raise click.ClickException("Engine transport is not ready.")
        return

    click.echo(f"Transport: {info['transport']}")
    if info.get("transport_source"):
        click.echo(f"Transport source: {info['transport_source']}")
    if info.get("resolved_transport") is not None:
        click.echo(f"Resolved transport: {info['resolved_transport']}")
    click.echo(f"Supported: {'yes' if info['transport_supported'] else 'no'}")
    click.echo(f"Ready: {'yes' if info['transport_ready'] else 'no'}")
    click.echo(f"Transport detail: {info['transport_detail']}")
    click.echo(f"Binary path: {info['binary_path']}")
    click.echo(f"Binary exists: {'yes' if info['binary_exists'] else 'no'}")
    click.echo(f"Supported transports: {', '.join(info['supported_transports'])}")
    runnable = info.get("runnable_transports")
    if isinstance(runnable, list) and runnable:
        click.echo(f"Runnable transports: {', '.join(runnable)}")
    if check_ready and (not info["transport_supported"] or not info["transport_ready"]):
        raise click.ClickException("Engine transport is not ready.")


@cli.group()
def query() -> None:
    """Query the local Rust-backed navigation engine."""


@cli.group()
def graph() -> None:
    """Navigate graph entities and relations directly."""


@query.command("symbol")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("symbol_query")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def query_symbol(repo_path: Path, symbol_query: str, json_output: bool) -> None:
    """Look up symbols in the local repository map."""
    try:
        results = search_symbol(repo_path, symbol_query)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(results, indent=2))
        return

    for result in results:
        click.echo(
            f"- {result['name']} ({result['kind']}) at {result['file']}:{result['line']} "
            f"[score={result['score']}]"
        )


@query.command("deps")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
def query_deps(repo_path: Path, target: str) -> None:
    """Show dependency frontier for a target symbol or file."""
    try:
        results = rust_dependency_frontier(repo_path, target)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc
    for result in results:
        click.echo(f"- {result}")


@query.command("impact")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
def query_impact(repo_path: Path, target: str) -> None:
    """Show reverse dependency frontier for a target symbol or file."""
    try:
        results = rust_impact_frontier(repo_path, target)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc
    for result in results:
        click.echo(f"- {result}")


@graph.command("node")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_node_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Inspect a graph node directly."""
    try:
        result = graph_node(repo_path, target)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"ID: {result['id']}")
    click.echo(f"Kind: {result['kind']}")
    click.echo(f"Label: {result['label']}")
    if result.get("confidence") is not None:
        click.echo(f"Confidence: {result['confidence']}")
    if result.get("path"):
        click.echo(f"Path: {result['path']}")
    if result.get("area"):
        click.echo(f"Area: {result['area']}")
    if result.get("language"):
        click.echo(f"Language: {result['language']}")
    if result.get("annotations"):
        click.echo("Annotations:")
        for annotation in result["annotations"]:
            click.echo(f"- {annotation}")
    _emit_completeness_signals(result)


def _render_relation(result: dict[str, Any], json_output: bool) -> None:
    if json_output:
        click.echo(json.dumps(result, indent=2))
        return
    click.echo(f"Target: {result['target']}")
    click.echo(f"Relation: {result['relation']}")
    for item in result["items"]:
        click.echo(
            f"- {item['display']} ({item['kind']}, {item['relation']}, conf={item['confidence']})"
        )
    _emit_completeness_signals(result)


def _emit_completeness_signals(payload: dict[str, Any]) -> None:
    """Surface truncation/cap/confidence signals when available."""
    if isinstance(payload.get("truncated"), bool):
        truncated = payload["truncated"]
        click.echo(f"Truncated: {'yes' if truncated else 'no'}")
        if truncated and payload.get("reason"):
            click.echo(f"Truncation reason: {payload['reason']}")

    confidence = payload.get("confidence")
    if isinstance(confidence, (int, float)):
        click.echo(f"Confidence: {confidence}")
    elif isinstance(confidence, dict):
        anchor_conf = confidence.get("anchor_confidence")
        scope_conf = confidence.get("scope_confidence")
        if anchor_conf is not None or scope_conf is not None:
            click.echo(
                "Confidence:"
                f" anchor={anchor_conf if anchor_conf is not None else 'n/a'}"
                f", scope={scope_conf if scope_conf is not None else 'n/a'}"
            )

    caps = payload.get("caps")
    if isinstance(caps, dict) and caps:
        click.echo(f"Caps: {json.dumps(caps)}")


@graph.command("children")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_children_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show structural children of a graph node."""
    try:
        _render_relation(graph_children(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("parents")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_parents_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show structural parents of a graph node."""
    try:
        _render_relation(graph_parents(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("callers")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_callers_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show callers for a function node."""
    try:
        _render_relation(graph_callers(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("callees")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_callees_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show callees for a function node."""
    try:
        _render_relation(graph_callees(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("docs")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_docs_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show documentation related to a graph node."""
    try:
        _render_relation(graph_docs(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("configs")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_configs_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show config and entrypoint links related to a graph node."""
    try:
        _render_relation(graph_configs(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("expand")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_expand_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Return a compact graph slice for iterative navigation."""
    try:
        result = graph_expand(repo_path, target)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Target: {result['target']['label']} ({result['target']['kind']})")
    if result["target"].get("path"):
        click.echo(f"Path: {result['target']['path']}")
    for label in ("parents", "children", "callers", "callees", "docs", "configs"):
        items = result.get(label, [])
        if not items:
            continue
        click.echo(f"{label.capitalize()}:")
        for item in items:
            click.echo(
                f"- {item['display']} ({item['kind']}, {item['relation']}, conf={item['confidence']})"
            )
    if result.get("risks"):
        click.echo("Risks:")
        for risk in result["risks"]:
            click.echo(f"- {risk}")
    _emit_completeness_signals(result)


@graph.command("overview")
@click.argument(
    "repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path)
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_overview_command(repo_path: Path, json_output: bool) -> None:
    """Show a repo-level navigation overview derived from the graph."""
    try:
        result = graph_overview(repo_path)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Repository: {result['repo']}")
    if result.get("signals"):
        click.echo("Signals:")
        for name, signal in result["signals"].items():
            click.echo(
                f"- {name.replace('_', ' ')}: {signal['score']} ({signal['level']})"
            )
    for label in (
        "code_areas",
        "reference_areas",
        "subareas",
        "overview_docs",
        "key_configs",
        "entrypoints",
        "representative_code_files",
        "representative_docs",
    ):
        items = result.get(label, [])
        if not items:
            continue
        click.echo(f"{label.replace('_', ' ').title()}:")
        for item in items:
            click.echo(f"- {item}")
    _emit_completeness_signals(result)


@cli.group()
def task() -> None:
    """Task-context workflows over the local repository engine."""


@cli.group()
def facts() -> None:
    """Derived repository facts built on top of the graph."""


@cli.group()
def analyze() -> None:
    """Deterministic analyzers that answer recurring repository questions."""


@task.command("pack")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_pack(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Build a deterministic task-context pack."""
    try:
        pack = build_task_pack(repo_path, task_text)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(pack, indent=2))
        return

    click.echo(render_pack_summary(pack))


@task.command("context")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--task", "task_text", required=True, help="Task description")
@click.option(
    "--content-budget",
    "content_budget",
    default=80000,
    type=int,
    help="Max bytes of file content",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_context_command(
    repo_path: Path, task_text: str, content_budget: int, json_output: bool
) -> None:
    """Build task context pack with file contents (single-call navigation)."""
    try:
        result = build_task_context(repo_path, task_text, content_budget)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result))
        return

    click.echo(render_pack_summary(result))


@task.command("anchors")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_anchors_command(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Resolve initial graph anchors for a task."""
    try:
        result = task_anchors(repo_path, task_text)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Task: {result['task']}")
    for anchor in result["anchors"]:
        click.echo(f"- {anchor['id']} ({anchor['kind']}): {anchor['reason']}")


@task.command("scope")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_scope_command(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Show in-scope and out-of-scope graph regions for a task."""
    try:
        result = task_scope(repo_path, task_text)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Task: {result['task']}")
    if result["in_scope_files"]:
        click.echo("In-scope files:")
        for item in result["in_scope_files"]:
            if isinstance(item, dict):
                reason = item.get("reason")
                suffix = f" ({reason})" if reason else ""
                click.echo(f"- {item.get('value', item)}{suffix}")
            else:
                click.echo(f"- {item}")
    if result["in_scope_areas"]:
        click.echo("In-scope areas:")
        for item in result["in_scope_areas"]:
            if isinstance(item, dict):
                reason = item.get("reason")
                suffix = f" ({reason})" if reason else ""
                click.echo(f"- {item.get('value', item)}{suffix}")
            else:
                click.echo(f"- {item}")
    if result["out_of_scope"]:
        click.echo("Out of scope:")
        for item in result["out_of_scope"]:
            if isinstance(item, dict):
                reason = item.get("reason")
                suffix = f" ({reason})" if reason else ""
                click.echo(f"- {item.get('value', item)}{suffix}")
            else:
                click.echo(f"- {item}")
    if result["risks"]:
        click.echo("Risks:")
        for item in result["risks"]:
            click.echo(f"- {item}")
    _emit_completeness_signals(result)


@task.command("next")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_next_command(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Show the next recommended navigation steps for a task."""
    try:
        _render_relation(task_next(repo_path, task_text), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@task.command("expand")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--node", "node_target", required=True, help="Node id or display target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_expand_command(repo_path: Path, node_target: str, json_output: bool) -> None:
    """Expand a graph node into related navigation context."""
    try:
        result = task_expand(repo_path, node_target)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Node: {result['node']}")
    for section in ("dependencies", "impact", "docs", "configs", "risks"):
        items = result.get(section, [])
        if not items:
            continue
        click.echo(f"{section.capitalize()}:")
        for item in items:
            click.echo(f"- {item}")
    _emit_completeness_signals(result)


@task.command("explain")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--task",
    "task_text",
    default=DEFAULT_TASK,
    show_default=True,
    help="Task description",
)
def task_explain(repo_path: Path, task_text: str) -> None:
    """Explain the repository using the deterministic Rust engine."""
    try:
        inspect = inspect_repository(repo_path)
        pack = build_task_pack(repo_path, task_text)
        explanation = render_explain_repo_text(inspect, pack)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc
    click.echo(explanation)


@facts.command("public-functions")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option("--scope", "scope", required=True, help="Directory scope prefix")
@click.option(
    "--include-methods",
    "include_methods",
    is_flag=True,
    help="Include public methods as well as top-level functions",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def facts_public_functions_command(
    repo_path: Path, scope: str, include_methods: bool, json_output: bool
) -> None:
    """List derived public/exported function facts for a scope."""
    try:
        result = derived_public_functions(
            repo_path, scope, include_methods=include_methods
        )
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    for item in result:
        click.echo(f"- {item['defined_in']}::{item['name']} [{item['exposure_kind']}]")


@facts.command("function-usage")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--target", "target", required=True, help="Function id, name, or qualified name"
)
@click.option(
    "--boundary",
    "boundary",
    required=True,
    help="Boundary prefix for internal vs external callers",
)
@click.option("--roots", "roots", default="", help="Comma-separated search roots")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def facts_function_usage_command(
    repo_path: Path, target: str, boundary: str, roots: str, json_output: bool
) -> None:
    """Show derived usage facts for one function relative to a boundary."""
    roots_list = [item.strip() for item in roots.split(",") if item.strip()]
    try:
        result = derived_function_usage(
            repo_path, target, boundary=boundary, roots=roots_list
        )
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Function: {result['function']['qualified_name']}")
    click.echo(f"Boundary: {result['boundary']}")
    if result["internal_callers"]:
        click.echo("Internal callers:")
        for item in result["internal_callers"]:
            click.echo(f"- {item}")
    if result["external_callers"]:
        click.echo("External callers:")
        for item in result["external_callers"]:
            click.echo(f"- {item}")


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


def _intent_catalog() -> dict[str, Any]:
    return {
        "schema_version": "aethyme-intents-v1",
        "selection_contract": {
            "who_selects": "caller_or_llm_for_specialized_intents",
            "aethyme_role": (
                "Run the default general-purpose localization intent when no "
                "intent is selected; validate explicit intents and parameters, "
                "then run deterministic repository analysis."
            ),
            "caller_role": (
                "Use the default Explore path for normal requests. Choose a "
                "specialized intent from the catalog only when the user request "
                "clearly matches it, then provide that intent's structured "
                "parameters."
            ),
            "default_intent": "task_localization_query",
            "no_hidden_task_specific_routing": True,
        },
        "canonical_flow": [
            "Run `aethyme explore --repo <repo> --request <task> --format answer-json` first; default detail is compact.",
            "Read `trust_policy` and use `answer[]` only when `safe_to_use_as_answer` is true.",
            "If trust_policy is `needs_verification`, follow `verification_steps[]` before using answer[] as final evidence.",
            "If only `navigation_hints[]` are available, treat them as manual investigation steps, not candidate answers.",
            "For a specialized intent, rerun `aethyme explore --repo <repo> --intent <intent> --request <task> --params '<json>' --format answer-json --show-observability`.",
            "Read answer[] first only after the trust policy, verification_steps[] second, excluded[] third, ambiguous[]/navigation_hints[]/next_actions fourth, observability last.",
        ],
        "modes": [
            {
                "mode": "explore",
                "purpose": "Return repository facts, evidence, and task-ready answers without editing files.",
                "command": "aethyme explore",
                "intents": [
                    {
                        "intent": "task_localization_query",
                        "summary": (
                            "Localize any normal repository question into ranked "
                            "candidate files, symbols, areas, evidence, and next "
                            "navigation steps."
                        ),
                        "best_for": [
                            "bug diagnosis",
                            "feature localization",
                            "impact analysis",
                            "architecture questions",
                            "where should I look first",
                            "general repository exploration",
                        ],
                        "required_params": [],
                        "optional_params": [
                            "max_anchors",
                            "max_files",
                            "max_symbols",
                            "max_areas",
                            "max_next_items",
                            "max_answer_items",
                            "max_expansions",
                            "detail",
                            "include_expansions",
                            "max_symbol_queries",
                            "max_symbol_results",
                            "max_text_files",
                            "max_text_line_refs",
                            "max_callsite_symbols",
                            "max_callsite_results",
                            "symbol_query_timeout_ms",
                            "graph_query_timeout_ms",
                            "skip_symbols_after_graph_timeout",
                        ],
                        "param_defaults": {
                            "detail": "compact",
                            "max_anchors": 3,
                            "max_files": 5,
                            "max_symbols": 5,
                            "max_areas": 3,
                            "max_next_items": 5,
                            "max_answer_items": 12,
                            "max_expansions": 1,
                            "include_expansions": True,
                            "max_symbol_queries": 5,
                            "max_symbol_results": 4,
                            "max_text_files": 5,
                            "max_text_line_refs": 2,
                            "max_callsite_symbols": 4,
                            "max_callsite_results": 4,
                            "symbol_query_timeout_ms": 1000,
                            "graph_query_timeout_ms": 1000,
                            "skip_symbols_after_graph_timeout": False,
                        },
                        "default_for_explore": True,
                        "answer_schema": {
                            "kind": "anchor | in_scope_file | in_scope_symbol | in_scope_area | next_step",
                            "target": "symbol id, file path, area, or display label",
                            "path": "repo-relative path when known",
                            "status": "candidate",
                            "evidence": "object",
                            "confidence": "number",
                            "reason": "string",
                        },
                        "verification_step_schema": {
                            "action": "read_source_window | search_symbol_in_candidate | read_candidate_file",
                            "target": "symbol or path",
                            "path": "repo-relative path when known",
                            "command": "shell command for cheap manual verification",
                            "reason": "why this verification step matters",
                        },
                        "navigation_hint_schema": {
                            "kind": "filesystem_file | graph_next_action | investigation_plan",
                            "status": "navigation_hint",
                            "trust_policy": "navigation_only",
                            "confidence": "low number; filename-only evidence cannot be authoritative",
                        },
                        "trust_contract": {
                            "safe_to_use_as_answer": "boolean",
                            "safe_to_use_as_navigation": "boolean",
                            # Native (Rust) explore emits compound levels that
                            # describe WHICH evidence sources corroborated, e.g.
                            # `graph+symbol+text+callsite` (triple corroborated),
                            # `graph+symbol+callsite` (strong callsite),
                            # `graph+callsite-weak`, `graph+text`, `graph+symbol`,
                            # `graph+symbol-weak`, `graph` (anchors only). Single-
                            # word legacy forms (`graph`/`symbol`/`text`) still
                            # appear at the lowest tiers and on degraded paths.
                            "evidence_level": "compound: graph[+symbol[-weak]][+text[-weak]][+callsite[-weak]] | none",
                            "verification_required": "boolean",
                            "trust_policy": "answer_candidate | needs_verification | navigation_only | failed",
                        },
                        "observability": [
                            "command name",
                            "repo path",
                            "index freshness",
                            "internal analyzers called",
                            "graph/fact count",
                            "output size",
                            "confidence summary",
                            "evidence level",
                            "trust policy",
                            "failure/degraded reason",
                        ],
                    },
                    {
                        "intent": "behavior_localization_query",
                        "summary": (
                            "Localize behavioral bugs and feature flows by combining "
                            "graph/symbol results with source-text evidence and "
                            "call-site expansion."
                        ),
                        "best_for": [
                            "bug reports with observable behavior",
                            "trace this feature or side effect",
                            "what code path updates this state",
                            "which callers of this API need inspection",
                        ],
                        "required_params": [],
                        "optional_params": [
                            "max_text_files",
                            "max_text_line_refs",
                            "max_callsite_symbols",
                            "max_callsite_results",
                            "graph_query_timeout_ms",
                            "symbol_query_timeout_ms",
                        ],
                        "param_defaults": {
                            "max_text_files": 10,
                            "max_text_line_refs": 4,
                            "max_callsite_symbols": 8,
                            "max_callsite_results": 8,
                            "graph_query_timeout_ms": 1000,
                            "symbol_query_timeout_ms": 1000,
                            "skip_symbols_after_graph_timeout": False,
                        },
                        "answer_schema": {
                            "kind": "source_text_file | call_site_file | symbol_search_file | anchor",
                            "path": "repo-relative path",
                            "role": "entrypoint | state_change | caller | docs | source_candidate",
                            "evidence": "line_refs, matched_terms, top_symbols, call-site chains, or callers",
                            "confidence": "number",
                            "reason": "string",
                        },
                        "observability": [
                            "source text candidate count",
                            "callsite candidate count",
                            "degradation guidance",
                            "trust policy",
                        ],
                    },
                    {
                        "intent": "usage_boundary_query",
                        "summary": (
                            "Find public symbols in a scope and classify whether callers "
                            "exist outside a boundary."
                        ),
                        "best_for": [
                            "dead-code checks",
                            "public API usage audits",
                            "is this symbol used outside this package",
                        ],
                        "required_params": ["scope"],
                        "optional_params": [
                            "symbol_kind",
                            "boundary",
                            "search_roots",
                            "include_methods",
                            "budget_ms",
                            "max_evidence_per_symbol",
                        ],
                        "param_defaults": {
                            "symbol_kind": "public_top_level_function",
                            "boundary": {"type": "outside_directory", "path": "<scope>"},
                            "search_roots": [],
                            "include_methods": False,
                            "budget_ms": 10000,
                            "max_evidence_per_symbol": 5,
                        },
                        "answer_schema": {
                            "function_name": "string",
                            "defined_in": "repo-relative path",
                            "status": "Unused | Ambiguous | Used",
                            "external_callers": ["string"],
                            "internal_callers": ["string"],
                            "evidence": "object",
                            "confidence": "number",
                            "reason": "string",
                        },
                        "observability": [
                            "command name",
                            "repo path",
                            "index freshness",
                            "graph/fact count",
                            "output size",
                            "confidence summary",
                            "failure/degraded reason",
                        ],
                    }
                ],
            },
            {
                "mode": "act",
                "purpose": "Reserved for future deterministic change-planning and safe edit workflows.",
                "command": None,
                "intents": [],
            },
            {
                "mode": "learn",
                "purpose": "Reserved for future feedback capture, eval traces, and repository memory updates.",
                "command": None,
                "intents": [],
            },
        ],
    }


def _parse_json_object(raw_value: str, *, option_name: str) -> dict[str, Any]:
    try:
        parsed = json.loads(raw_value)
    except json.JSONDecodeError as exc:
        raise click.BadParameter(
            f"Expected a JSON object: {exc.msg}", param_hint=option_name
        ) from exc
    if not isinstance(parsed, dict):
        raise click.BadParameter("Expected a JSON object.", param_hint=option_name)
    return parsed


def _task_localization_output_detail(
    params: dict[str, Any], output_detail: str
) -> str:
    raw = params.get("detail", output_detail)
    detail = str(raw or "compact").strip().lower()
    if detail not in {"compact", "standard", "full"}:
        raise click.BadParameter(
            "Expected one of: compact, standard, full.", param_hint="--detail"
        )
    return detail


def _trim_explore_response(
    response: dict[str, Any],
    *,
    detail: str,
    show_observability: bool,
) -> dict[str, Any]:
    """Strip large/redundant fields from an explore response based on detail level.

    Measured cost on a real MediaWiki query before this trim was ~6,400 tokens
    of JSON per call; ~28% of that was redundant or debug-only payload that the
    agent can't act on. Trimming defaults bring the same answer down to roughly
    2,000-2,500 tokens at `compact` while still giving the agent everything it
    needs to plan a follow-up.

    The trim is purely additive on top of the existing response builder: nothing
    is computed differently, only fewer fields are emitted. `--detail full` and
    `--show-observability` reinstate the previous shape for back-compat.
    """

    # `--show-observability` is the documented escape hatch for "I'm
    # investigating Aethyme" — it should reinstate every diagnostic field,
    # not just `observability` itself. `--detail full` is the documented
    # back-compat mode for consumers that want the original shape.
    if detail == "full" or show_observability:
        return response

    # Drop the redundant repackaging at compact / standard. `output_adapters`
    # re-emits `answer[]` items in a different shape; the agent already has the
    # canonical form in `answer[]`. Keep at full for back-compat with consumers
    # that read the adapter view.
    response.pop("output_adapters", None)

    # Internal tuning knobs (per-call limits + timeouts). Useful for debugging
    # Aethyme but not actionable by the agent.
    response.pop("resolved_parameters", None)

    # Full command-trace data: per-stage timings, anchor confidence summaries,
    # query counts. Useful when investigating Aethyme behavior; pure noise to
    # the consuming agent.
    response.pop("observability", None)

    # Verification steps come back as a list of 5+ items. Agents typically
    # follow only 1-2 before deciding. Truncate at compact only.
    if detail == "compact":
        steps = response.get("verification_steps") or []
        if isinstance(steps, list) and len(steps) > 2:
            response["verification_steps"] = steps[:2]

    return response


def _task_localization_detail_defaults(detail: str) -> dict[str, Any]:
    if detail == "full":
        return {
            "max_anchors": 5,
            "max_files": 8,
            "max_symbols": 8,
            "max_areas": 5,
            "max_next_items": 10,
            "max_answer_items": 0,
            "max_expansions": 3,
            "max_symbol_queries": 6,
            "max_symbol_results": 5,
            "max_text_files": 8,
            "max_text_line_refs": 4,
            "max_callsite_symbols": 5,
            "max_callsite_results": 6,
            "include_expansions": True,
        }
    if detail == "standard":
        return {
            "max_anchors": 4,
            "max_files": 6,
            "max_symbols": 6,
            "max_areas": 4,
            "max_next_items": 6,
            "max_answer_items": 24,
            "max_expansions": 2,
            "max_symbol_queries": 6,
            "max_symbol_results": 4,
            "max_text_files": 6,
            "max_text_line_refs": 3,
            "max_callsite_symbols": 5,
            "max_callsite_results": 5,
            "include_expansions": True,
        }
    # Compact defaults — agent-first.
    #
    # max_answer_items used to be 12 here, larger than the union of
    # max_files (5) + max_symbols (5) = 10. Lowering to 5 brings the cap
    # into alignment with the sources that fill it and cuts the largest
    # response field (`answer`, ~2.4k tokens at 12 items) roughly in half.
    # Agents typically act on the top 1-3 candidates; readers asking for
    # more should use `--detail standard` or `--detail full`.
    #
    # max_text_files lowered to 3 because filesystem-filename matches are
    # the weakest evidence kind — only useful as `navigation_hints[]`,
    # never authoritative `answer[]` items.
    return {
        "max_anchors": 3,
        "max_files": 5,
        "max_symbols": 5,
        "max_areas": 3,
        "max_next_items": 5,
        "max_answer_items": 5,
        "max_expansions": 1,
        "max_symbol_queries": 5,
        "max_symbol_results": 4,
        "max_text_files": 3,
        "max_text_line_refs": 2,
        "max_callsite_symbols": 4,
        "max_callsite_results": 4,
        "include_expansions": True,
    }


def _explore_task_localization_query(
    *,
    repo_path: Path,
    request_text: str,
    params: dict[str, Any],
    show_observability: bool,
    output_detail: str,
    intent_source: str,
    intent_name: str = "task_localization_query",
) -> dict[str, Any]:
    request = request_text.strip()
    if not request:
        return _explore_error_payload(
            repo_path=repo_path,
            request_text=request_text,
            params=params,
            intent=intent_name,
            errors=["Missing request text. Provide --request for task localization."],
        )

    detail = _task_localization_output_detail(params, output_detail)
    defaults = _task_localization_detail_defaults(detail)
    max_anchors = _positive_int_param(
        params, "max_anchors", default=defaults["max_anchors"], minimum=1
    )
    max_files = _positive_int_param(
        params, "max_files", default=defaults["max_files"], minimum=1
    )
    max_symbols = _positive_int_param(
        params, "max_symbols", default=defaults["max_symbols"], minimum=1
    )
    max_areas = _positive_int_param(
        params, "max_areas", default=defaults["max_areas"], minimum=0
    )
    max_next_items = _positive_int_param(
        params, "max_next_items", default=defaults["max_next_items"], minimum=0
    )
    max_answer_items = _positive_int_param(
        params, "max_answer_items", default=defaults["max_answer_items"], minimum=0
    )
    max_expansions = _positive_int_param(
        params, "max_expansions", default=defaults["max_expansions"], minimum=0
    )
    max_symbol_queries = _positive_int_param(
        params, "max_symbol_queries", default=defaults["max_symbol_queries"], minimum=0
    )
    max_symbol_results = _positive_int_param(
        params, "max_symbol_results", default=defaults["max_symbol_results"], minimum=0
    )
    max_text_files = _positive_int_param(
        params, "max_text_files", default=defaults["max_text_files"], minimum=0
    )
    max_text_line_refs = _positive_int_param(
        params, "max_text_line_refs", default=defaults["max_text_line_refs"], minimum=1
    )
    max_callsite_symbols = _positive_int_param(
        params,
        "max_callsite_symbols",
        default=defaults["max_callsite_symbols"],
        minimum=0,
    )
    max_callsite_results = _positive_int_param(
        params,
        "max_callsite_results",
        default=defaults["max_callsite_results"],
        minimum=0,
    )
    symbol_query_timeout_ms = _positive_int_param(
        params, "symbol_query_timeout_ms", default=1_000, minimum=100
    )
    graph_query_timeout_ms = _positive_int_param(
        params, "graph_query_timeout_ms", default=1_000, minimum=100
    )
    skip_symbols_after_graph_timeout = bool(
        params.get("skip_symbols_after_graph_timeout", False)
    )
    include_expansions = bool(
        params.get("include_expansions", defaults["include_expansions"])
    )
    degraded_reasons: list[str] = []

    graph_timeout_seconds = graph_query_timeout_ms / 1000
    graph_defaults = _task_localization_default_graph_bundle(request)
    graph_bundle = _task_localization_graph_view(
        "task-localize",
        lambda: task_localize(
            repo_path,
            request,
            timeout_seconds=graph_timeout_seconds,
        ),
        default=graph_defaults,
        degraded_reasons=degraded_reasons,
    )
    anchors_result = _dict_or_default(graph_bundle.get("anchors"), graph_defaults["anchors"])
    scope_result = _dict_or_default(graph_bundle.get("scope"), graph_defaults["scope"])
    next_result = _dict_or_default(graph_bundle.get("next"), graph_defaults["next"])

    graph_timed_out = any(
        reason.startswith("task-localize skipped: Rust engine timed out")
        for reason in degraded_reasons
    )
    if graph_timed_out and skip_symbols_after_graph_timeout:
        symbol_matches = []
        degraded_reasons.append(
            "symbol batch search skipped: task-localize exceeded the responsiveness budget"
        )
    else:
        symbol_matches = _task_localization_symbol_matches(
            repo_path=repo_path,
            request=request,
            max_queries=max_symbol_queries,
            max_results=max_symbol_results,
            timeout_seconds=symbol_query_timeout_ms / 1000,
            degraded_reasons=degraded_reasons,
        )
    filesystem_items = _task_localization_filesystem_items(
        repo_path=repo_path,
        request=request,
        max_items=max_files,
    )
    text_items = _task_localization_text_items(
        repo_path=repo_path,
        request=request,
        max_items=max_text_files,
        max_line_refs=max_text_line_refs,
    )
    callsite_items, callsite_expansions = _task_localization_callsite_items(
        repo_path=repo_path,
        text_items=text_items,
        symbol_matches=symbol_matches,
        max_symbols=max_callsite_symbols,
        max_results=max_callsite_results,
    )
    expansions = _task_localization_expansions(
        repo_path=repo_path,
        anchors=anchors_result.get("anchors", []),
        max_expansions=max_expansions if include_expansions else 0,
        timeout_seconds=graph_timeout_seconds,
        degraded_reasons=degraded_reasons,
    )
    answer_items = _task_localization_answer_items(
        symbol_matches=symbol_matches,
        text_items=text_items,
        callsite_items=callsite_items,
        anchors=anchors_result.get("anchors", []),
        scope=scope_result,
        next_view=next_result,
        expansions=expansions,
        max_anchors=max_anchors,
        max_files=max_files,
        max_symbols=max_symbols,
        max_areas=max_areas,
        max_next_items=max_next_items,
    )
    navigation_hints = _task_localization_navigation_hints(
        repo_path=repo_path,
        request=request,
        filesystem_items=filesystem_items,
        raw_next_items=next_result.get("items", []),
        answer_items=answer_items,
        max_items=max_next_items,
    )
    excluded_items = _task_localization_excluded_items(scope_result, max_items=max_areas)
    ambiguous_items = _task_localization_ambiguous_items(
        answer_items=answer_items,
        anchors=anchors_result.get("anchors", []),
        navigation_hints=navigation_hints,
    )
    trust_policy = _task_localization_trust_policy(
        answer_items=answer_items,
        navigation_hints=navigation_hints,
        degraded_reasons=degraded_reasons,
    )
    command_observability = _task_localization_observability(
        repo_path=repo_path,
        anchors=anchors_result.get("anchors", []),
        scope=scope_result,
        next_view=next_result,
        answer_items=answer_items,
        navigation_hints=navigation_hints,
        excluded_items=excluded_items,
        ambiguous_items=ambiguous_items,
        symbol_match_count=len(symbol_matches),
        filesystem_candidate_count=len(filesystem_items),
        text_candidate_count=len(text_items),
        callsite_candidate_count=len(callsite_items),
        expansion_count=len(expansions),
        degraded_reasons=degraded_reasons,
        trust_policy=trust_policy,
        show_observability=show_observability,
    )
    status = "degraded" if degraded_reasons else "complete"
    public_answer_items = _task_localization_public_answer_items(
        answer_items, detail=detail, max_items=max_answer_items
    )
    public_evidence = _task_localization_evidence_payload(
        detail=detail,
        answer_items=answer_items,
        navigation_hints=navigation_hints,
        excluded_items=excluded_items,
        symbol_matches=symbol_matches,
        text_items=text_items,
        callsite_expansions=callsite_expansions,
        filesystem_items=filesystem_items,
        anchors=anchors_result.get("anchors", []),
        scope_result=scope_result,
        next_result=next_result,
        max_symbols=max_symbols,
        max_files=max_files,
        max_anchors=max_anchors,
        max_areas=max_areas,
        max_next_items=max_next_items,
        max_answer_items=max_answer_items,
        expansions=expansions,
    )
    verification_steps = _task_localization_verification_steps(
        answer_items=answer_items,
        navigation_hints=navigation_hints,
        repo_path=repo_path,
        max_items=max_next_items,
    )

    response = {
        "schema_version": "aethyme-explore-v1",
        "mode": "explore",
        "intent": intent_name,
        "intent_source": intent_source,
        "status": status,
        "request": {
            "raw": request_text,
            "parameters": params,
        },
        "resolved_parameters": {
            "detail": detail,
            "max_anchors": max_anchors,
            "max_files": max_files,
            "max_symbols": max_symbols,
            "max_areas": max_areas,
            "max_next_items": max_next_items,
            "max_answer_items": max_answer_items,
            "max_expansions": max_expansions,
            "include_expansions": include_expansions,
            "max_symbol_queries": max_symbol_queries,
            "max_symbol_results": max_symbol_results,
            "max_text_files": max_text_files,
            "max_text_line_refs": max_text_line_refs,
            "max_callsite_symbols": max_callsite_symbols,
            "max_callsite_results": max_callsite_results,
            "symbol_query_timeout_ms": symbol_query_timeout_ms,
            "graph_query_timeout_ms": graph_query_timeout_ms,
            "skip_symbols_after_graph_timeout": skip_symbols_after_graph_timeout,
        },
        "answer": public_answer_items,
        "navigation_hints": navigation_hints,
        "excluded": excluded_items,
        "ambiguous": ambiguous_items,
        "evidence": public_evidence,
        "confidence": {
            "overall": _overall_confidence(answer_items),
            "answer_summary": _confidence_summary_for_items(answer_items),
            "excluded_summary": _confidence_summary_for_items(excluded_items),
            "analyzed_summary": command_observability.get("confidence_summary", {}),
        },
        "output_adapters": {
            "task_localization_json": {
                "candidate_files": [
                    _adapter_candidate_item(item, detail=detail)
                    for item in public_answer_items
                    if item.get("kind")
                    in {
                        "symbol_search_file",
                        "source_text_file",
                        "call_site_file",
                        "filesystem_file",
                        "anchor",
                        "in_scope_file",
                    }
                    and item.get("path")
                ],
                "candidate_symbols": [
                    _adapter_candidate_item(item, detail=detail)
                    for item in public_answer_items
                    if item.get("kind") in {"symbol_search", "in_scope_symbol"}
                    or item.get("evidence", {}).get("anchor_kind") == "symbol"
                ],
                "next_actions": _task_localization_next_action_objects(
                    next_result.get("items", []),
                    repo_path=repo_path,
                    max_items=max_next_items,
                ),
                "verification_steps": verification_steps,
                "navigation_hints": [] if detail == "compact" else navigation_hints,
            }
        },
        "safe_to_use_as_answer": trust_policy["safe_to_use_as_answer"],
        "safe_to_use_as_navigation": trust_policy["safe_to_use_as_navigation"],
        "trust_policy": trust_policy,
        "observability": command_observability,
        "degraded_reasons": degraded_reasons,
        "verification_steps": verification_steps,
        "next_actions": _task_localization_next_actions(
            answer_items=answer_items,
            navigation_hints=navigation_hints,
            ambiguous_items=ambiguous_items,
            degraded_reasons=degraded_reasons,
            trust_policy=trust_policy,
        ),
        "available_specialized_intents": [
            "behavior_localization_query",
            "usage_boundary_query",
        ],
    }
    return _trim_explore_response(
        response, detail=detail, show_observability=show_observability
    )


def _task_localization_expansions(
    *,
    repo_path: Path,
    anchors: Any,
    max_expansions: int,
    timeout_seconds: float,
    degraded_reasons: list[str],
) -> dict[str, dict[str, Any]]:
    if max_expansions <= 0:
        return {}
    expansions: dict[str, dict[str, Any]] = {}
    for anchor in _dict_items(anchors)[:max_expansions]:
        target = _anchor_target(anchor)
        if not target:
            continue
        try:
            expansions[target] = _compact_task_expansion(
                task_expand(repo_path, target, timeout_seconds=timeout_seconds)
            )
        except EngineError as exc:
            degraded_reasons.append(f"task-expand failed for {target}: {exc}")
    return expansions


def _task_localization_default_graph_bundle(request: str) -> dict[str, dict[str, Any]]:
    return {
        "anchors": {"task": request, "anchors": []},
        "scope": {
            "task": request,
            "navigation_order": [],
            "in_scope_files": [],
            "in_scope_symbols": [],
            "in_scope_areas": [],
            "out_of_scope": [],
            "risks": [],
        },
        "next": {"target": request, "relation": "next", "items": []},
    }


def _dict_or_default(value: Any, default: dict[str, Any]) -> dict[str, Any]:
    return value if isinstance(value, dict) else default


def _task_localization_graph_view(
    name: str,
    producer: Any,
    *,
    default: dict[str, Any],
    degraded_reasons: list[str],
) -> dict[str, Any]:
    try:
        return producer()
    except EngineError as exc:
        degraded_reasons.append(f"{name} skipped: {exc}")
        return default


def _task_localization_symbol_matches(
    *,
    repo_path: Path,
    request: str,
    max_queries: int,
    max_results: int,
    timeout_seconds: float,
    degraded_reasons: list[str],
) -> list[dict[str, Any]]:
    if max_queries <= 0 or max_results <= 0:
        return []
    queries = _request_symbol_queries(request)[:max_queries]
    if not queries:
        return []
    try:
        results_by_query = search_symbols(
            repo_path,
            queries,
            limit=max_results,
            timeout_seconds=timeout_seconds,
        )
    except EngineError as exc:
        degraded_reasons.append(f"symbol batch search skipped: {exc}")
        return []

    matches: list[dict[str, Any]] = []
    seen: set[str] = set()
    for query in queries:
        for result in results_by_query.get(query, [])[:max_results]:
            if not isinstance(result, dict):
                continue
            result_id = str(result.get("id") or result.get("name") or "").strip()
            if not result_id or result_id in seen:
                continue
            seen.add(result_id)
            matches.append({"query": query, "result": result})
    return matches


def _task_localization_symbol_file_items(
    symbol_matches: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    by_file: dict[str, dict[str, Any]] = {}
    for match in symbol_matches:
        result = match.get("result")
        if not isinstance(result, dict):
            continue
        file_path = result.get("file")
        if not isinstance(file_path, str) or not file_path.strip():
            continue
        item = by_file.setdefault(
            file_path,
            {
                "queries": set(),
                "symbols": [],
                "score": 0.0,
            },
        )
        query = match.get("query")
        if isinstance(query, str) and query.strip():
            item["queries"].add(query)
        item["symbols"].append(
            {
                "name": result.get("name"),
                "kind": result.get("kind"),
                "line": result.get("line"),
                "score": result.get("score"),
            }
        )
        if isinstance(result.get("score"), (int, float)):
            item["score"] += float(result["score"])

    ranked_files = sorted(
        by_file.items(),
        key=lambda pair: (len(pair[1]["queries"]), pair[1]["score"]),
        reverse=True,
    )
    items: list[dict[str, Any]] = []
    for file_path, summary in ranked_files[:8]:
        matched_queries = sorted(summary["queries"])
        confidence = 0.88 if len(matched_queries) > 1 else 0.76
        items.append(
            {
                "kind": "symbol_search_file",
                "target": file_path,
                "path": file_path,
                "status": "candidate",
                "evidence": {
                    "source": "query-symbol",
                    "matched_queries": matched_queries,
                    "symbols": summary["symbols"][:5],
                    "combined_score": summary["score"],
                },
                "confidence": confidence,
                "reason": "Multiple request terms matched symbols in this file."
                if len(matched_queries) > 1
                else "A request term matched a symbol in this file.",
            }
        )
    return items


def _task_localization_text_items(
    *,
    repo_path: Path,
    request: str,
    max_items: int,
    max_line_refs: int,
) -> list[dict[str, Any]]:
    if max_items <= 0:
        return []
    terms = _request_text_search_terms(request)
    if not terms:
        return []

    skip_dirs = _source_search_skip_dirs()
    allowed_suffixes = _source_search_suffixes(include_docs=True)
    scored: list[dict[str, Any]] = []
    for root, dirs, files in os.walk(repo_path):
        dirs[:] = [
            item
            for item in dirs
            if item not in skip_dirs and not item.startswith(".")
        ]
        for filename in files:
            path = Path(root) / filename
            suffix = path.suffix.lower()
            if suffix and suffix not in allowed_suffixes:
                continue
            try:
                if path.stat().st_size > 750_000:
                    continue
                contents = path.read_text(encoding="utf-8", errors="ignore")
            except OSError:
                continue
            if "\0" in contents:
                continue
            rel_path = path.relative_to(repo_path).as_posix()
            lines = contents.splitlines()
            symbols = _extract_source_symbols(lines, rel_path)
            hit_lines: list[dict[str, Any]] = []
            matched_terms: set[str] = set()
            enclosed_symbols: dict[tuple[str, int], dict[str, Any]] = {}
            clusters: dict[str, dict[str, Any]] = {}
            hit_count = 0
            for line_number, line in enumerate(lines, start=1):
                lowered = line.lower()
                terms_for_line = [
                    term for term in terms if _term_matches_line(term, lowered)
                ]
                if not terms_for_line:
                    continue
                hit_count += 1
                matched_terms.update(terms_for_line)
                symbol = _enclosing_symbol(symbols, line_number)
                if symbol:
                    enclosed_symbols[(symbol["name"], symbol["line"])] = symbol
                line_item = {
                    "line": line_number,
                    "text": line.strip()[:220],
                    "matched_terms": terms_for_line[:6],
                    "enclosing_symbol": _compact_source_symbol(symbol)
                    if symbol
                    else None,
                    "score": _text_line_score(line, terms_for_line),
                }
                hit_lines.append(line_item)
                cluster_key = "__file__"
                if symbol:
                    cluster_key = str(symbol.get("qualified_name") or symbol.get("name"))
                cluster = clusters.setdefault(
                    cluster_key,
                    {
                        "symbol": symbol,
                        "matched_terms": set(),
                        "line_refs": [],
                        "hit_count": 0,
                    },
                )
                cluster["matched_terms"].update(terms_for_line)
                cluster["line_refs"].append(line_item)
                cluster["hit_count"] += 1
            if not matched_terms:
                continue

            hit_lines.sort(
                key=lambda item: (
                    item["score"],
                    len(item["matched_terms"]),
                    -item["line"],
                ),
                reverse=True,
            )
            top_clusters = _rank_source_text_clusters(clusters)
            line_refs = _source_text_cluster_line_refs(
                top_clusters, fallback_lines=hit_lines, max_line_refs=max_line_refs
            )
            top_symbol_score = (
                int(top_clusters[0].get("score") or 0) if top_clusters else 0
            )
            score = _text_candidate_score(
                rel_path=rel_path,
                matched_terms=matched_terms,
                hit_count=hit_count,
                enclosed_symbol_count=len(enclosed_symbols),
                line_refs=line_refs,
                top_symbol_score=top_symbol_score,
            )
            if score <= 0:
                continue
            confidence = _text_candidate_confidence(
                matched_terms=matched_terms,
                hit_count=hit_count,
                enclosed_symbol_count=len(enclosed_symbols),
            )
            scored.append(
                {
                    "kind": "source_text_file",
                    "target": rel_path,
                    "path": rel_path,
                    "status": "candidate",
                    "role": _source_candidate_role(rel_path, matched_terms),
                    "evidence": {
                        "source": "source-text-search",
                        "matched_terms": sorted(matched_terms),
                        "hit_count": hit_count,
                        "line_refs": line_refs,
                        "top_symbols": [
                            _compact_source_text_cluster(cluster)
                            for cluster in top_clusters[:5]
                        ],
                        "score": score,
                    },
                    "confidence": confidence,
                    "reason": (
                        "Source text matched request terms in executable code; "
                        "line refs are evidence, not filename-only hints."
                    ),
                }
            )

    scored.sort(
        key=lambda item: (
            item["evidence"]["score"],
            item["confidence"],
            -len(str(item["path"])),
            str(item["path"]),
        ),
        reverse=True,
    )
    return scored[:max_items]


def _task_localization_callsite_items(
    *,
    repo_path: Path,
    text_items: list[dict[str, Any]],
    symbol_matches: list[dict[str, Any]],
    max_symbols: int,
    max_results: int,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if max_symbols <= 0 or max_results <= 0:
        return ([], {})

    candidates: list[dict[str, Any]] = []
    seen_symbols: set[str] = set()
    for item in text_items:
        evidence = item.get("evidence")
        if not isinstance(evidence, dict):
            continue
        symbol_sources = _dict_items(evidence.get("top_symbols"))
        if not symbol_sources:
            symbol_sources = _dict_items(evidence.get("enclosed_symbols"))
        for symbol in symbol_sources:
            name = str(symbol.get("name") or "").strip()
            if (
                not name
                or name in seen_symbols
                or _is_noisy_symbol_query(name)
                or _is_noisy_callsite_symbol(name)
            ):
                continue
            seen_symbols.add(name)
            candidates.append(
                {
                    "name": name,
                    "kind": symbol.get("kind"),
                    "defined_in": symbol.get("path") or item.get("path"),
                    "line": symbol.get("line"),
                    "source": "source-text-enclosing-symbol",
                }
            )
    for match in _ranked_symbol_matches(symbol_matches):
        result = match.get("result")
        if not isinstance(result, dict):
            continue
        name = str(result.get("name") or "").strip()
        if (
            not name
            or name in seen_symbols
            or _is_noisy_symbol_query(name)
            or _is_noisy_callsite_symbol(name)
        ):
            continue
        seen_symbols.add(name)
        candidates.append(
            {
                "name": name,
                "kind": result.get("kind"),
                "defined_in": result.get("file"),
                "line": result.get("line"),
                "source": "query-symbol",
            }
        )

    expansions: dict[str, Any] = {}
    by_file: dict[str, dict[str, Any]] = {}
    bridge_candidates: list[dict[str, Any]] = []
    seen_bridge_symbols: set[tuple[str, str]] = set()

    def add_callsite_summary(
        *,
        caller: dict[str, Any],
        symbol_name: str,
        score: int,
        chain: dict[str, Any] | None = None,
    ) -> None:
        path = caller.get("path")
        if not isinstance(path, str) or not path:
            return
        entry = by_file.setdefault(
            path,
            {
                "symbols": set(),
                "callers": [],
                "chains": [],
                "score": 0,
            },
        )
        entry["symbols"].add(symbol_name)
        entry["callers"].append(caller)
        if chain:
            entry["chains"].append(chain)
        entry["score"] += score

    for symbol in candidates[:max_symbols]:
        name = symbol["name"]
        callers = _find_source_callers(
            repo_path=repo_path,
            symbol=name,
            definition_path=symbol.get("defined_in"),
            max_results=max_results,
        )
        expansions[name] = {
            "symbol": symbol,
            "callers": callers,
            "caller_count": len(callers),
        }
        for caller in callers:
            add_callsite_summary(caller=caller, symbol_name=name, score=10)
            caller_symbol = _source_enclosing_symbol_at_callsite(
                repo_path=repo_path,
                rel_path=caller.get("path"),
                line_number=caller.get("line"),
            )
            if not caller_symbol:
                continue
            bridge_name = str(caller_symbol.get("name") or "").strip()
            bridge_path = str(caller_symbol.get("path") or "").strip()
            bridge_key = (bridge_path, bridge_name)
            if (
                not bridge_name
                or bridge_name == name
                or bridge_key in seen_bridge_symbols
                or _is_noisy_symbol_query(bridge_name)
                or _is_noisy_callsite_symbol(bridge_name)
            ):
                continue
            seen_bridge_symbols.add(bridge_key)
            bridge_candidates.append(
                {
                    "name": bridge_name,
                    "kind": caller_symbol.get("kind"),
                    "defined_in": bridge_path,
                    "line": caller_symbol.get("line"),
                    "source": "source-callsite-enclosing-symbol",
                    "via_symbol": name,
                    "via_path": symbol.get("defined_in"),
                }
            )
            add_callsite_summary(
                caller=caller,
                symbol_name=bridge_name,
                score=18,
                chain={
                    "source_symbol": name,
                    "bridge_symbol": bridge_name,
                    "bridge_path": bridge_path,
                },
            )

    for bridge in bridge_candidates[:max_symbols]:
        bridge_name = bridge["name"]
        callers = _find_source_callers(
            repo_path=repo_path,
            symbol=bridge_name,
            definition_path=bridge.get("defined_in"),
            max_results=max_results,
        )
        expansion_key = f"{bridge_name} via {bridge.get('via_symbol')}"
        expansions[expansion_key] = {
            "symbol": bridge,
            "callers": callers,
            "caller_count": len(callers),
            "chain": {
                "via_symbol": bridge.get("via_symbol"),
                "via_path": bridge.get("via_path"),
                "bridge_path": bridge.get("defined_in"),
            },
        }
        for caller in callers:
            add_callsite_summary(
                caller=caller,
                symbol_name=bridge_name,
                score=16,
                chain={
                    "source_symbol": bridge.get("via_symbol"),
                    "bridge_symbol": bridge_name,
                    "bridge_path": bridge.get("defined_in"),
                },
            )

    items: list[dict[str, Any]] = []
    for path, summary in sorted(
        by_file.items(),
        key=lambda pair: (pair[1]["score"], len(pair[1]["symbols"]), pair[0]),
        reverse=True,
    )[:max_results]:
        symbols = sorted(summary["symbols"])
        items.append(
            {
                "kind": "call_site_file",
                "target": path,
                "path": path,
                "status": "candidate",
                "role": "caller",
                "evidence": {
                    "source": "source-callsite",
                    "symbols": symbols,
                    "line_refs": summary["callers"][:max_results],
                    "chains": summary["chains"][:max_results],
                    "score": summary["score"],
                },
                "confidence": (
                    0.84
                    if summary["chains"]
                    else 0.82
                    if len(symbols) > 1
                    else 0.76
                ),
                "reason": (
                    "Source-backed call-site expansion found this file calling "
                    f"{', '.join(symbols[:3])}."
                ),
            }
        )
    return (items, expansions)


def _rank_source_text_clusters(
    clusters: dict[str, dict[str, Any]]
) -> list[dict[str, Any]]:
    ranked: list[dict[str, Any]] = []
    for cluster in clusters.values():
        line_refs = [
            item
            for item in cluster.get("line_refs", [])
            if isinstance(item, dict)
        ]
        if not line_refs:
            continue
        line_refs.sort(
            key=lambda item: (
                int(item.get("score") or 0),
                len(item.get("matched_terms") or []),
                -int(item.get("line") or 0),
            ),
            reverse=True,
        )
        matched_terms = {
            term
            for term in cluster.get("matched_terms", set())
            if isinstance(term, str)
        }
        score = sum(int(item.get("score") or 0) for item in line_refs[:5])
        score += _request_term_group_count(matched_terms) * 58
        score += min(int(cluster.get("hit_count") or 0), 12) * 3
        symbol = cluster.get("symbol")
        if isinstance(symbol, dict):
            symbol_text = " ".join(
                str(symbol.get(key) or "")
                for key in ("name", "qualified_name", "signature")
            ).lower()
            if any(term.lower() in symbol_text for term in matched_terms):
                score += 24
            if "::" in str(symbol.get("qualified_name") or ""):
                score += 8
        ranked.append(
            {
                "symbol": symbol if isinstance(symbol, dict) else None,
                "matched_terms": sorted(matched_terms),
                "line_refs": line_refs,
                "hit_count": int(cluster.get("hit_count") or 0),
                "score": score,
            }
        )
    return sorted(
        ranked,
        key=lambda item: (
            int(item.get("score") or 0),
            len(item.get("matched_terms") or []),
        ),
        reverse=True,
    )


def _source_text_cluster_line_refs(
    clusters: list[dict[str, Any]],
    *,
    fallback_lines: list[dict[str, Any]],
    max_line_refs: int,
) -> list[dict[str, Any]]:
    refs: list[dict[str, Any]] = []
    seen_lines: set[int] = set()
    for cluster in clusters:
        for line_ref in cluster.get("line_refs", [])[:max(2, max_line_refs)]:
            line_number = line_ref.get("line")
            if not isinstance(line_number, int) or line_number in seen_lines:
                continue
            refs.append(line_ref)
            seen_lines.add(line_number)
            if len(refs) >= max_line_refs:
                return refs
    for line_ref in fallback_lines:
        line_number = line_ref.get("line")
        if not isinstance(line_number, int) or line_number in seen_lines:
            continue
        refs.append(line_ref)
        seen_lines.add(line_number)
        if len(refs) >= max_line_refs:
            break
    return refs


def _compact_source_text_cluster(cluster: dict[str, Any]) -> dict[str, Any]:
    symbol = cluster.get("symbol") if isinstance(cluster.get("symbol"), dict) else {}
    compact = {
        "name": symbol.get("name"),
        "qualified_name": symbol.get("qualified_name"),
        "kind": symbol.get("kind"),
        "path": symbol.get("path"),
        "line": symbol.get("line"),
        "source": "source-text-enclosing-symbol",
        "matched_terms": cluster.get("matched_terms", []),
        "score": cluster.get("score"),
        "hit_count": cluster.get("hit_count"),
    }
    return {key: value for key, value in compact.items() if value not in (None, [])}


def _compact_source_symbol(symbol: dict[str, Any]) -> dict[str, Any]:
    compact = {
        "name": symbol.get("name"),
        "qualified_name": symbol.get("qualified_name"),
        "kind": symbol.get("kind"),
        "path": symbol.get("path"),
        "line": symbol.get("line"),
    }
    return {key: value for key, value in compact.items() if value is not None}


def _ranked_symbol_matches(
    symbol_matches: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    file_query_counts: dict[str, int] = {}
    file_scores: dict[str, float] = {}
    by_file_queries: dict[str, set[str]] = {}
    for match in symbol_matches:
        result = match.get("result")
        if not isinstance(result, dict):
            continue
        file_path = result.get("file")
        if not isinstance(file_path, str):
            continue
        query = match.get("query")
        by_file_queries.setdefault(file_path, set())
        if isinstance(query, str):
            by_file_queries[file_path].add(query)
        if isinstance(result.get("score"), (int, float)):
            file_scores[file_path] = file_scores.get(file_path, 0.0) + float(
                result["score"]
            )
    for file_path, queries in by_file_queries.items():
        file_query_counts[file_path] = len(queries)

    return sorted(
        symbol_matches,
        key=lambda match: _symbol_match_rank(
            match,
            file_query_counts=file_query_counts,
            file_scores=file_scores,
        ),
        reverse=True,
    )


def _symbol_match_rank(
    match: dict[str, Any],
    *,
    file_query_counts: dict[str, int],
    file_scores: dict[str, float],
) -> tuple[int, float, float]:
    result = match.get("result")
    if not isinstance(result, dict):
        return (0, 0.0, 0.0)
    file_path = result.get("file")
    raw_score = result.get("score")
    score = float(raw_score) if isinstance(raw_score, (int, float)) else 0.0
    if not isinstance(file_path, str):
        return (0, 0.0, score)
    return (file_query_counts.get(file_path, 0), file_scores.get(file_path, 0.0), score)


def _task_localization_filesystem_items(
    *,
    repo_path: Path,
    request: str,
    max_items: int,
) -> list[dict[str, Any]]:
    if max_items <= 0:
        return []
    terms = _request_file_queries(request)
    if not terms:
        return []

    skip_dirs = {
        ".aethyme",
        ".chau7",
        ".claude",
        ".codex",
        ".git",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "target",
        "vendor",
        "venv",
    }
    allowed_suffixes = {
        ".c",
        ".cc",
        ".cpp",
        ".cs",
        ".go",
        ".h",
        ".hpp",
        ".java",
        ".js",
        ".jsx",
        ".kt",
        ".mjs",
        ".php",
        ".py",
        ".rb",
        ".rs",
        ".swift",
        ".ts",
        ".tsx",
        ".vue",
    }
    scored: list[dict[str, Any]] = []
    for root, dirs, files in os.walk(repo_path):
        dirs[:] = [
            item
            for item in dirs
            if item not in skip_dirs and not item.startswith(".")
        ]
        for filename in files:
            path = Path(root) / filename
            if path.suffix and path.suffix.lower() not in allowed_suffixes:
                continue
            rel_path = path.relative_to(repo_path).as_posix()
            score, matched_terms = _filesystem_match_score(rel_path, terms)
            if score <= 0:
                continue
            scored.append(
                {
                    "kind": "filesystem_file",
                    "target": rel_path,
                    "path": rel_path,
                    "status": "navigation_hint",
                    "evidence": {
                        "source": "filesystem-filename",
                        "matched_terms": matched_terms,
                        "score": score,
                    },
                    "confidence": 0.38 if len(matched_terms) > 1 else 0.28,
                    "reason": (
                        "Filename-only match. Use as a search/navigation hint, "
                        "not as primary answer evidence."
                    ),
                    "trust_policy": "navigation_only",
                }
            )

    scored.sort(
        key=lambda item: (
            item["evidence"]["score"],
            -len(str(item["path"])),
            str(item["path"]),
        ),
        reverse=True,
    )
    return scored[:max_items]


def _filesystem_match_score(path: str, terms: list[str]) -> tuple[int, list[str]]:
    lowered_path = path.lower()
    filename = Path(path).name.lower()
    stem = Path(path).stem.lower()
    matched_terms: list[str] = []
    score = 0
    for term in terms:
        lowered = term.lower()
        if lowered == stem:
            score += 20
            matched_terms.append(term)
        elif stem.startswith(lowered):
            score += 12
            matched_terms.append(term)
        elif lowered in stem:
            score += 8
            matched_terms.append(term)
        elif lowered in filename:
            score += 5
            matched_terms.append(term)
        elif lowered in lowered_path:
            score += 2
            matched_terms.append(term)
    if path.startswith("includes/"):
        score += 2
    return score, matched_terms


def _task_localization_answer_items(
    *,
    symbol_matches: list[dict[str, Any]],
    text_items: list[dict[str, Any]],
    callsite_items: list[dict[str, Any]],
    anchors: Any,
    scope: dict[str, Any],
    next_view: dict[str, Any],
    expansions: dict[str, dict[str, Any]],
    max_anchors: int,
    max_files: int,
    max_symbols: int,
    max_areas: int,
    max_next_items: int,
) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()

    def add_item(item: dict[str, Any]) -> None:
        target = str(item.get("target") or item.get("path") or "")
        key = (str(item.get("kind") or ""), target)
        if not target or key in seen:
            return
        seen.add(key)
        items.append(item)

    for item in _task_localization_symbol_file_items(symbol_matches):
        add_item(item)

    primary_text_items = [
        item for item in text_items if not _candidate_looks_reference_only(item)
    ]
    reference_text_items = [
        item for item in text_items if _candidate_looks_reference_only(item)
    ]

    source_first_count = min(max_files, 4)
    for item in primary_text_items[:source_first_count]:
        add_item(item)

    for item in callsite_items[:max_files]:
        add_item(item)

    for item in primary_text_items[source_first_count:max_files]:
        add_item(item)

    for match in _ranked_symbol_matches(symbol_matches)[:max_symbols]:
        result = match.get("result")
        if not isinstance(result, dict):
            continue
        target = str(result.get("id") or result.get("name") or "").strip()
        if not target:
            continue
        add_item(
            {
                "kind": "symbol_search",
                "target": target,
                "path": result.get("file"),
                "line": result.get("line"),
                "status": "candidate",
                "evidence": {
                    "source": "query-symbol",
                    "query": match.get("query"),
                    "symbol_name": result.get("name"),
                    "symbol_kind": result.get("kind"),
                    "score": result.get("score"),
                },
                "confidence": _symbol_search_confidence(result.get("score")),
                "reason": result.get("reason") or "Matched request term in symbol index.",
            }
        )

    for anchor in _dict_items(anchors)[:max_anchors]:
        target = _anchor_target(anchor)
        if not target:
            continue
        anchor_kind = str(anchor.get("kind", "anchor"))
        path = anchor.get("file")
        if not isinstance(path, str) and anchor_kind in {"file", "folder"}:
            path = target
        add_item(
            {
                "kind": "anchor",
                "target": target,
                "path": path,
                "status": "candidate",
                "evidence": {
                    "source": "task-anchors",
                    "anchor_kind": anchor_kind,
                    "expansion": expansions.get(target),
                },
                "confidence": _anchor_confidence(anchor),
                "reason": str(anchor.get("reason") or "Matched task anchor."),
            }
        )

    for scope_item in _scope_values(scope.get("in_scope_files", []))[:max_files]:
        add_item(
            {
                "kind": "in_scope_file",
                "target": scope_item["value"],
                "path": scope_item["value"],
                "status": "candidate",
                "evidence": {"source": "task-scope"},
                "confidence": 0.74,
                "reason": scope_item.get("reason")
                or "Selected by graph task scope as an in-scope file.",
            }
        )

    for scope_item in _scope_values(scope.get("in_scope_symbols", []))[:max_symbols]:
        add_item(
            {
                "kind": "in_scope_symbol",
                "target": scope_item["value"],
                "path": None,
                "status": "candidate",
                "evidence": {"source": "task-scope"},
                "confidence": 0.72,
                "reason": scope_item.get("reason")
                or "Selected by graph task scope as an in-scope symbol.",
            }
        )

    for scope_item in _scope_values(scope.get("in_scope_areas", []))[:max_areas]:
        add_item(
            {
                "kind": "in_scope_area",
                "target": scope_item["value"],
                "path": None,
                "status": "candidate",
                "evidence": {"source": "task-scope"},
                "confidence": 0.62,
                "reason": scope_item.get("reason")
                or "Selected by graph task scope as an in-scope area.",
            }
        )

    for next_item in _dict_items(next_view.get("items", []))[:max_next_items]:
        target = str(next_item.get("id") or next_item.get("display") or "").strip()
        if not target:
            continue
        display = str(next_item.get("display") or target)
        add_item(
            {
                "kind": "next_step",
                "target": target,
                "path": display if _looks_like_repo_path(display) else None,
                "status": "candidate",
                "evidence": {
                    "source": "task-next",
                    "display": display,
                    "relation": next_item.get("relation"),
                    "graph_kind": next_item.get("kind"),
                },
                "confidence": _graph_confidence(next_item.get("confidence"), 0.68),
                "reason": "Recommended by graph task navigation order.",
            }
        )

    for item in reference_text_items[:max_files]:
        add_item(item)

    return items


def _task_localization_navigation_hints(
    *,
    repo_path: Path,
    request: str,
    filesystem_items: list[dict[str, Any]],
    raw_next_items: Any,
    answer_items: list[dict[str, Any]],
    max_items: int,
) -> list[dict[str, Any]]:
    hints: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()

    def add_hint(item: dict[str, Any]) -> None:
        target = str(item.get("target") or item.get("path") or item.get("kind") or "")
        key = (str(item.get("kind") or ""), target)
        if not target or key in seen:
            return
        seen.add(key)
        hints.append(item)

    for item in filesystem_items[:max_items]:
        hint = dict(item)
        hint["status"] = "navigation_hint"
        hint["trust_policy"] = "navigation_only"
        add_hint(hint)

    for next_item in _task_localization_next_action_objects(
        raw_next_items, repo_path=repo_path, max_items=max_items
    ):
        add_hint(
            {
                "kind": "graph_next_action",
                "target": next_item["target"],
                "path": None,
                "status": "navigation_hint",
                "evidence": {
                    "source": "task-next",
                    "action": next_item["action"],
                    "command": next_item["command"],
                },
                "confidence": 0.48,
                "reason": "Graph suggested this as a follow-up inspection step.",
                "trust_policy": "navigation_only",
            }
        )

    search_terms = _request_investigation_terms(request)[:6]
    if search_terms and (not answer_items or filesystem_items):
        add_hint(
            {
                "kind": "investigation_plan",
                "target": "repository_search",
                "path": None,
                "status": "navigation_hint",
                "evidence": {
                    "source": "aethyme-trust-policy",
                    "search_terms": search_terms,
                },
                "confidence": 0.0,
                "reason": (
                    "Use these searches to verify weak or missing localization "
                    "before forming an answer."
                ),
                "commands": [
                    {
                        "label": f"Search {term}",
                        "command": f"rg -n {json.dumps(term)} .",
                    }
                    for term in search_terms
                ],
                "trust_policy": "navigation_only",
            }
        )

    return hints


def _candidate_looks_reference_only(item: dict[str, Any]) -> bool:
    path = item.get("path")
    role = item.get("role")
    return (
        isinstance(path, str)
        and _path_looks_like_reference_only(path)
        or role == "docs"
    )


def _task_localization_excluded_items(
    scope: dict[str, Any], *, max_items: int
) -> list[dict[str, Any]]:
    return [
        {
            "kind": "out_of_scope",
            "target": item["value"],
            "path": item["value"] if _looks_like_repo_path(item["value"]) else None,
            "status": "excluded",
            "evidence": {"source": "task-scope"},
            "confidence": 0.64,
            "reason": item.get("reason")
            or "Aethyme scoped this outside the current request.",
        }
        for item in _scope_values(scope.get("out_of_scope", []))[:max_items]
    ]


def _task_localization_ambiguous_items(
    *,
    answer_items: list[dict[str, Any]],
    anchors: Any,
    navigation_hints: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    ambiguous = [
        item
        for item in answer_items
        if (_confidence_value(item.get("confidence")) or 0.0) < 0.5
    ]
    if not _dict_items(anchors):
        ambiguous.append(
            {
                "kind": "no_anchor",
                "target": "request",
                "path": None,
                "status": "ambiguous",
                "evidence": {"source": "task-anchors", "anchors": []},
                "confidence": 0.0,
                "reason": "No graph anchor matched the request.",
            }
        )
    if not answer_items and navigation_hints:
        ambiguous.append(
            {
                "kind": "no_authoritative_answer",
                "target": "request",
                "path": None,
                "status": "ambiguous",
                "evidence": {
                    "source": "trust-policy",
                    "navigation_hint_count": len(navigation_hints),
                },
                "confidence": 0.0,
                "reason": (
                    "Only navigation hints were available; do not treat them as "
                    "answer candidates without manual verification."
                ),
            }
        )
    return ambiguous


def _task_localization_trust_policy(
    *,
    answer_items: list[dict[str, Any]],
    navigation_hints: list[dict[str, Any]],
    degraded_reasons: list[str],
) -> dict[str, Any]:
    evidence_level = _highest_evidence_level(answer_items, navigation_hints)
    best_answer_confidence = _best_confidence(answer_items)
    degraded = bool(degraded_reasons)
    safe_to_use_as_answer = bool(answer_items) and (
        not degraded
        and (best_answer_confidence is None or best_answer_confidence >= 0.72)
        and evidence_level in {"text", "symbol", "graph", "reference"}
    )
    if not answer_items:
        reason = (
            "No graph-, symbol-, or source-backed answer candidates were available. "
            "Navigation hints may guide manual search only."
        )
    elif degraded_reasons:
        reason = (
            "Answer candidates were found, but at least one analyzer degraded. "
            "Use them as ranked navigation and verify before forming a final answer."
        )
    else:
        reason = "Answer candidates have graph, symbol, or source evidence and no analyzer degraded."
    policy_name = "answer_candidate" if safe_to_use_as_answer else (
        "needs_verification" if answer_items else "navigation_only"
    )
    return {
        "safe_to_use_as_answer": safe_to_use_as_answer,
        "safe_to_use_as_navigation": bool(answer_items or navigation_hints),
        "evidence_level": evidence_level,
        "authoritative_answer_count": len(answer_items),
        "navigation_hint_count": len(navigation_hints),
        "best_answer_confidence": best_answer_confidence,
        "degraded": degraded,
        "verification_required": not safe_to_use_as_answer,
        "trust_policy": policy_name,
        "reason": reason,
    }


def _task_localization_next_actions(
    *,
    answer_items: list[dict[str, Any]],
    navigation_hints: list[dict[str, Any]],
    ambiguous_items: list[dict[str, Any]],
    degraded_reasons: list[str],
    trust_policy: dict[str, Any],
) -> list[str]:
    if not answer_items:
        actions = [
            "No authoritative graph/symbol/source candidates were found; do not use navigation_hints as the final answer.",
        ]
        if navigation_hints:
            actions.append(
                "Use navigation_hints only as a manual investigation plan, then verify with repository search."
            )
        else:
            actions.append(
                "Use repository search and then rerun Explore with more specific request terms."
            )
        if degraded_reasons:
            actions.append("Inspect degraded_reasons before retrying or widening budgets.")
        return actions
    actions = [
        "Use answer[] as the ranked starting set; inspect candidates in order before broad repository search.",
        "If the top candidate is a symbol, inspect its defining file and callers/callees before editing or concluding.",
    ]
    if not trust_policy.get("safe_to_use_as_answer"):
        actions.append("Follow verification_steps before using answer[] as final evidence.")
    if ambiguous_items:
        actions.append("Review ambiguous[] before relying on low-confidence candidates.")
    if degraded_reasons:
        actions.append("Inspect degraded_reasons before treating the result as complete.")
    return actions


def _task_localization_next_action_objects(
    raw_items: Any, *, repo_path: Path, max_items: int
) -> list[dict[str, str]]:
    actions: list[dict[str, str]] = []
    for item in _dict_items(raw_items)[:max_items]:
        target = str(item.get("id") or item.get("display") or "").strip()
        if not target:
            continue
        actions.append(
            {
                "action": "expand",
                "target": target,
                "command": (
                    "aethyme graph expand --repo "
                    f"{json.dumps(str(repo_path))} {json.dumps(target)} --json-output"
                ),
            }
        )
    return actions


def _task_localization_observability(
    *,
    repo_path: Path,
    anchors: Any,
    scope: dict[str, Any],
    next_view: dict[str, Any],
    answer_items: list[dict[str, Any]],
    navigation_hints: list[dict[str, Any]],
    excluded_items: list[dict[str, Any]],
    ambiguous_items: list[dict[str, Any]],
    symbol_match_count: int,
    filesystem_candidate_count: int,
    text_candidate_count: int,
    callsite_candidate_count: int,
    expansion_count: int,
    degraded_reasons: list[str],
    trust_policy: dict[str, Any],
    show_observability: bool,
) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    internal_analyzers = [
        "filesystem-filename",
        "search-symbol",
        "source-text-search",
        "source-callsite",
        "task-localize",
    ]
    if expansion_count:
        internal_analyzers.append("task-expand")
    engine_info = engine_runtime_info()
    compact_engine = {
        "resolved_transport": engine_info.get("resolved_transport"),
        "transport_ready": engine_info.get("transport_ready"),
        "transport_detail": engine_info.get("transport_detail"),
    }
    return {
        "command": "explore",
        "mode": "explore",
        "intent": "task_localization_query",
        "repo_path": str(snapshot.repo_path),
        "index_freshness": {
            "status": "fresh_for_current_snapshot",
            "repo_dirty": snapshot.dirty,
            "commit": snapshot.commit,
            "snapshot_key": snapshot.cache_key,
            "file_count": snapshot.file_count,
        },
        "engine": engine_info if show_observability else compact_engine,
        "internal_analyzers": internal_analyzers,
        "graph_fact_count": {
            "graph": {
                "anchors": len(_dict_items(anchors)),
                "symbol_matches": symbol_match_count,
                "filesystem_candidates": filesystem_candidate_count,
                "source_text_candidates": text_candidate_count,
                "callsite_candidates": callsite_candidate_count,
                "in_scope_files": len(_scope_values(scope.get("in_scope_files", []))),
                "in_scope_symbols": len(
                    _scope_values(scope.get("in_scope_symbols", []))
                ),
                "in_scope_areas": len(_scope_values(scope.get("in_scope_areas", []))),
                "out_of_scope": len(_scope_values(scope.get("out_of_scope", []))),
                "next_items": len(_dict_items(next_view.get("items", []))),
                "expansions": expansion_count,
            },
            "facts": {
                "candidate_items": len(answer_items),
                "navigation_hint_items": len(navigation_hints),
                "excluded_items": len(excluded_items),
                "ambiguous_items": len(ambiguous_items),
            },
        },
        "confidence_summary": _confidence_summary_for_items(
            [*answer_items, *excluded_items]
        ),
        "navigation_confidence_summary": _confidence_summary_for_items(
            navigation_hints
        ),
        "evidence_level": trust_policy.get("evidence_level"),
        "trust_policy": trust_policy,
        "degraded_reasons": degraded_reasons,
        "degradation_guidance": _task_localization_degradation_guidance(
            answer_items=answer_items,
            text_candidate_count=text_candidate_count,
            callsite_candidate_count=callsite_candidate_count,
            degraded_reasons=degraded_reasons,
        ),
        "failure_reason": None,
        "output_size_bytes": None,
    }


def _compact_task_expansion(expansion: dict[str, Any]) -> dict[str, Any]:
    return {
        "dependencies": _string_items(expansion.get("dependencies", []))[:5],
        "impact": _string_items(expansion.get("impact", []))[:5],
        "docs": _string_items(expansion.get("docs", []))[:5],
        "configs": _string_items(expansion.get("configs", []))[:5],
        "risks": _string_items(expansion.get("risks", []))[:5],
    }


def _task_localization_public_answer_items(
    items: list[dict[str, Any]], *, detail: str, max_items: int
) -> list[dict[str, Any]]:
    if detail == "full":
        return items
    visible_items = items[:max_items] if max_items > 0 else items
    max_line_refs = 3 if detail == "standard" else 2
    max_top_symbols = 3 if detail == "standard" else 2
    return [
        _compact_candidate_item(
            item,
            max_line_refs=max_line_refs,
            max_top_symbols=max_top_symbols,
            include_chains=True,
        )
        for item in visible_items
    ]


def _task_localization_evidence_payload(
    *,
    detail: str,
    answer_items: list[dict[str, Any]],
    navigation_hints: list[dict[str, Any]],
    excluded_items: list[dict[str, Any]],
    symbol_matches: list[dict[str, Any]],
    text_items: list[dict[str, Any]],
    callsite_expansions: dict[str, Any],
    filesystem_items: list[dict[str, Any]],
    anchors: Any,
    scope_result: dict[str, Any],
    next_result: dict[str, Any],
    max_symbols: int,
    max_files: int,
    max_anchors: int,
    max_areas: int,
    max_next_items: int,
    max_answer_items: int,
    expansions: dict[str, Any],
) -> dict[str, Any]:
    visible_answer_items = answer_items[:max_answer_items] if max_answer_items > 0 else answer_items
    base: dict[str, Any] = {
        "detail": detail,
        "answer_count": len(answer_items),
        "visible_answer_count": len(visible_answer_items),
        "navigation_hint_count": len(navigation_hints),
        "excluded_count": len(excluded_items),
        "top_answer_targets": [
            {
                "kind": item.get("kind"),
                "target": item.get("target"),
                "path": item.get("path"),
                "confidence": item.get("confidence"),
                "evidence_source": (
                    item.get("evidence", {}).get("source")
                    if isinstance(item.get("evidence"), dict)
                    else None
                ),
            }
            for item in visible_answer_items[:max_files]
        ],
        "candidate_source_counts": {
            "symbol_matches": len(symbol_matches),
            "source_text_candidates": len(text_items),
            "callsite_expansions": len(callsite_expansions),
            "filesystem_candidates": len(filesystem_items),
            "anchors": len(_dict_items(anchors)),
        },
    }
    if detail == "compact":
        return base

    base.update(
        {
            "symbol_matches": _ranked_symbol_matches(symbol_matches)[:max_symbols],
            "source_text_candidates": [
                _compact_candidate_item(item, max_line_refs=2, max_top_symbols=2)
                for item in text_items[:max_files]
            ],
            "filesystem_candidates": filesystem_items[:max_files],
            "anchors": _dict_items(anchors)[:max_anchors],
            "scope": {
                "navigation_order": _string_items(
                    scope_result.get("navigation_order", [])
                ),
                "in_scope_files": _scope_values(
                    scope_result.get("in_scope_files", [])
                )[:max_files],
                "in_scope_symbols": _scope_values(
                    scope_result.get("in_scope_symbols", [])
                )[:max_symbols],
                "in_scope_areas": _scope_values(
                    scope_result.get("in_scope_areas", [])
                )[:max_areas],
                "risks": _string_items(scope_result.get("risks", [])),
            },
            "next_items": _dict_items(next_result.get("items", []))[:max_next_items],
            "expansions": expansions,
        }
    )
    if detail == "full":
        base["callsite_expansions"] = callsite_expansions
    else:
        base["callsite_expansion_summary"] = {
            key: {
                "caller_count": value.get("caller_count"),
                "symbol": value.get("symbol"),
            }
            for key, value in list(callsite_expansions.items())[:max_symbols]
            if isinstance(value, dict)
        }
    return base


def _task_localization_verification_steps(
    *,
    answer_items: list[dict[str, Any]],
    navigation_hints: list[dict[str, Any]],
    repo_path: Path,
    max_items: int,
) -> list[dict[str, Any]]:
    steps: list[dict[str, Any]] = []
    seen: set[tuple[str, str, int | None]] = set()

    def add_step(step: dict[str, Any]) -> None:
        target = str(step.get("target") or step.get("path") or "").strip()
        line = step.get("line") if isinstance(step.get("line"), int) else None
        key = (str(step.get("action") or ""), target, line)
        if not target or key in seen or len(steps) >= max_items:
            return
        seen.add(key)
        steps.append(step)

    for item in [*answer_items, *navigation_hints]:
        if len(steps) >= max_items:
            break
        path = item.get("path")
        if not isinstance(path, str) or not path.strip():
            continue
        evidence = item.get("evidence") if isinstance(item.get("evidence"), dict) else {}
        line_refs = _dict_items(evidence.get("line_refs"))
        if line_refs:
            line = line_refs[0].get("line")
            if isinstance(line, int):
                start = max(1, line - 30)
                end = line + 30
                add_step(
                    {
                        "action": "read_source_window",
                        "target": path,
                        "path": path,
                        "line": line,
                        "command": (
                            f"cd {shlex.quote(str(repo_path))} && "
                            f"sed -n '{start},{end}p' {shlex.quote(path)}"
                        ),
                        "reason": "Verify the strongest source line before trusting this candidate.",
                    }
                )
                continue
        symbols = evidence.get("symbols")
        if isinstance(symbols, list) and symbols:
            symbol = str(symbols[0]).strip()
            if symbol:
                add_step(
                    {
                        "action": "search_symbol_in_candidate",
                        "target": symbol,
                        "path": path,
                        "command": (
                            f"cd {shlex.quote(str(repo_path))} && "
                            f"rg -n {shlex.quote(symbol)} {shlex.quote(path)}"
                        ),
                        "reason": "Verify that the candidate participates in the suspected call chain.",
                    }
                )
                continue
        add_step(
            {
                "action": "read_candidate_file",
                "target": path,
                "path": path,
                "command": (
                    f"cd {shlex.quote(str(repo_path))} && "
                    f"sed -n '1,220p' {shlex.quote(path)}"
                ),
                "reason": "Inspect this ranked candidate before broad repository search.",
            }
        )
    return steps


def _compact_candidate_item(
    item: dict[str, Any],
    *,
    max_line_refs: int = 2,
    max_top_symbols: int = 2,
    include_chains: bool = False,
) -> dict[str, Any]:
    evidence = item.get("evidence") if isinstance(item.get("evidence"), dict) else {}
    compact: dict[str, Any] = {
        "kind": item.get("kind"),
        "target": item.get("target"),
        "path": item.get("path"),
        "status": item.get("status"),
        "confidence": item.get("confidence"),
        "reason": item.get("reason"),
    }
    if item.get("role"):
        compact["role"] = item.get("role")
    if evidence:
        compact_evidence: dict[str, Any] = {}
        passthrough_keys = (
            "source",
            "matched_terms",
            "hit_count",
            "symbols",
            "score",
            "query",
            "symbol_name",
            "symbol_kind",
            "anchor_kind",
            "expansion",
            "display",
            "relation",
            "graph_kind",
        )
        for key in passthrough_keys:
            if key in evidence:
                compact_evidence[key] = evidence.get(key)
        if "line_refs" in evidence:
            compact_evidence["line_refs"] = _compact_line_refs(
                evidence.get("line_refs"), max_items=max_line_refs
            )
        if "top_symbols" in evidence:
            compact_evidence["top_symbols"] = _dict_items(
                evidence.get("top_symbols")
            )[:max_top_symbols]
        if include_chains and "chains" in evidence:
            compact_evidence["chains"] = _dict_items(evidence.get("chains"))[:2]
        compact["evidence"] = {
            key: value
            for key, value in compact_evidence.items()
            if value not in (None, [], {})
        }
    return compact


def _adapter_candidate_item(item: dict[str, Any], *, detail: str) -> dict[str, Any]:
    if detail != "compact":
        return _compact_candidate_item(item)
    compact = {
        "kind": item.get("kind"),
        "target": item.get("target"),
        "path": item.get("path"),
        "status": item.get("status"),
        "confidence": item.get("confidence"),
        "reason": item.get("reason"),
    }
    return {
        key: value
        for key, value in compact.items()
        if value not in (None, [], {})
    }


def _compact_line_refs(raw_refs: Any, *, max_items: int) -> list[dict[str, Any]]:
    refs: list[dict[str, Any]] = []
    for item in _dict_items(raw_refs)[:max_items]:
        compact = {
            "line": item.get("line"),
            "text": item.get("text"),
            "matched_terms": item.get("matched_terms"),
            "enclosing_symbol": item.get("enclosing_symbol"),
            "score": item.get("score"),
        }
        refs.append(
            {
                key: value
                for key, value in compact.items()
                if value not in (None, [], {})
            }
        )
    return refs


def _anchor_target(anchor: dict[str, Any]) -> str:
    return str(anchor.get("id") or anchor.get("file") or "").strip()


def _anchor_confidence(anchor: dict[str, Any]) -> float:
    anchor_kind = str(anchor.get("kind", ""))
    if anchor_kind in {"symbol", "file"}:
        return 0.84
    if anchor_kind == "folder":
        return 0.68
    return 0.6


def _symbol_search_confidence(raw_score: Any) -> float:
    if isinstance(raw_score, bool):
        return 0.7
    if isinstance(raw_score, (int, float)):
        score = float(raw_score)
        if score >= 200:
            return 0.92
        if score >= 100:
            return 0.82
        if score >= 50:
            return 0.72
    return 0.66


def _graph_confidence(raw_value: Any, default: float) -> float:
    if isinstance(raw_value, bool):
        return default
    if isinstance(raw_value, (int, float)):
        value = float(raw_value)
        if value > 1:
            value /= 1000.0
        return round(max(0.0, min(1.0, value)), 4)
    return default


def _highest_evidence_level(
    answer_items: list[dict[str, Any]], navigation_hints: list[dict[str, Any]]
) -> str:
    order = {
        "none": 0,
        "filename": 1,
        "text": 2,
        "reference": 3,
        "symbol": 4,
        "graph": 5,
    }
    best = "none"
    for item in [*answer_items, *navigation_hints]:
        level = _item_evidence_level(item)
        if order[level] > order[best]:
            best = level
    return best


def _item_evidence_level(item: dict[str, Any]) -> str:
    evidence = item.get("evidence")
    source = evidence.get("source") if isinstance(evidence, dict) else None
    if source in {"task-anchors", "task-scope", "task-next"}:
        return "graph"
    if source == "query-symbol":
        return "symbol"
    if source in {"source-text-search", "source-callsite"}:
        return "text"
    if source == "filesystem-filename":
        return "filename"
    return "none"


def _task_localization_degradation_guidance(
    *,
    answer_items: list[dict[str, Any]],
    text_candidate_count: int,
    callsite_candidate_count: int,
    degraded_reasons: list[str],
) -> dict[str, Any]:
    if not degraded_reasons:
        return {
            "status": "not_degraded",
            "message": "All configured analyzers completed.",
            "useful_next_retry": None,
        }
    if answer_items:
        return {
            "status": "recovered",
            "message": (
                "One or more analyzers degraded, but source-backed fallback "
                "produced answer candidates. Verify before finalizing."
            ),
            "source_text_candidates": text_candidate_count,
            "callsite_candidates": callsite_candidate_count,
            "useful_next_retry": (
                "Increase graph_query_timeout_ms if graph-backed ranking is needed; "
                "otherwise inspect answer[].evidence.line_refs and callsite_expansions."
            ),
        }
    return {
        "status": "not_recovered",
        "message": (
            "Configured analyzers degraded and no source-backed answer candidate "
            "was recovered."
        ),
        "source_text_candidates": text_candidate_count,
        "callsite_candidates": callsite_candidate_count,
        "useful_next_retry": (
            "Retry with more concrete request terms, a larger graph_query_timeout_ms, "
            "or an explicit behavior_localization_query intent."
        ),
    }


def _dict_items(raw_items: Any) -> list[dict[str, Any]]:
    if not isinstance(raw_items, list):
        return []
    return [item for item in raw_items if isinstance(item, dict)]


def _scope_values(raw_items: Any) -> list[dict[str, str]]:
    values: list[dict[str, str]] = []
    if not isinstance(raw_items, list):
        return values
    for item in raw_items:
        if isinstance(item, dict):
            value = item.get("value")
            if isinstance(value, str) and value.strip():
                normalized = {"value": value.strip()}
                reason = item.get("reason")
                if isinstance(reason, str) and reason.strip():
                    normalized["reason"] = reason.strip()
                values.append(normalized)
        elif isinstance(item, str) and item.strip():
            values.append({"value": item.strip()})
    return values


def _string_items(raw_items: Any) -> list[str]:
    if not isinstance(raw_items, list):
        return []
    return [item.strip() for item in raw_items if isinstance(item, str) and item.strip()]


def _looks_like_repo_path(value: str) -> bool:
    return "/" in value or "\\" in value or "." in Path(value).name


def _source_search_skip_dirs() -> set[str]:
    return {
        ".aethyme",
        ".chau7",
        ".claude",
        ".codex",
        ".git",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".venv",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "target",
        "vendor",
        "venv",
    }


def _source_search_suffixes(*, include_docs: bool) -> set[str]:
    suffixes = {
        ".c",
        ".cc",
        ".cpp",
        ".cs",
        ".go",
        ".h",
        ".hpp",
        ".java",
        ".js",
        ".jsx",
        ".kt",
        ".mjs",
        ".php",
        ".py",
        ".rb",
        ".rs",
        ".swift",
        ".ts",
        ".tsx",
        ".vue",
    }
    if include_docs:
        suffixes.update({".md", ".rst", ".txt"})
    return suffixes


def _extract_source_symbols(lines: list[str], rel_path: str) -> list[dict[str, Any]]:
    symbols: list[dict[str, Any]] = []
    current_class: str | None = None
    for index, line in enumerate(lines, start=1):
        stripped = line.strip()
        class_name = _extract_class_name(stripped)
        if class_name:
            current_class = class_name
            symbols.append(
                {
                    "name": class_name,
                    "qualified_name": class_name,
                    "kind": "class",
                    "path": rel_path,
                    "line": index,
                    "signature": stripped[:220],
                }
            )
            continue
        function_name = _extract_function_name(stripped)
        if function_name:
            qualified = (
                f"{current_class}::{function_name}" if current_class else function_name
            )
            symbols.append(
                {
                    "name": function_name,
                    "qualified_name": qualified,
                    "kind": "function",
                    "path": rel_path,
                    "line": index,
                    "signature": stripped[:220],
                }
            )
    return symbols


def _extract_class_name(stripped: str) -> str | None:
    prefixes = ("class ", "interface ", "trait ", "enum ")
    for prefix in prefixes:
        if stripped.startswith(prefix):
            rest = stripped[len(prefix) :].strip()
            return _identifier_prefix(rest)
    return None


def _extract_function_name(stripped: str) -> str | None:
    lowered = stripped.lower()
    prefixes = (
        "public static function ",
        "protected static function ",
        "private static function ",
        "public function ",
        "protected function ",
        "private function ",
        "static function ",
        "async function ",
        "export function ",
        "function ",
        "def ",
        "func ",
        "fn ",
    )
    for prefix in prefixes:
        if not lowered.startswith(prefix):
            continue
        rest = stripped[len(prefix) :].strip()
        name = _identifier_prefix(rest)
        if name and name not in {"if", "for", "while", "switch"}:
            return name
    return None


def _identifier_prefix(value: str) -> str | None:
    chars: list[str] = []
    for char in value:
        if char.isalnum() or char == "_":
            chars.append(char)
            continue
        break
    if not chars:
        return None
    name = "".join(chars)
    if not (name[0].isalpha() or name[0] == "_"):
        return None
    return name


def _enclosing_symbol(
    symbols: list[dict[str, Any]], line_number: int
) -> dict[str, Any] | None:
    current: dict[str, Any] | None = None
    for symbol in symbols:
        line = symbol.get("line")
        if isinstance(line, int) and line <= line_number:
            current = symbol
        elif isinstance(line, int) and line > line_number:
            break
    return current


def _term_matches_line(term: str, lowered_line: str) -> bool:
    lowered = term.lower()
    if not lowered:
        return False
    if " " in lowered:
        return lowered in lowered_line
    if lowered in {"oldid", "diffonly"}:
        return lowered in lowered_line
    if len(lowered) <= 4 and lowered not in {"diff", "view"}:
        separators = " \t()[]{}.,;:'\"/\\<>|+-=*"
        padded = lowered_line
        for separator in separators:
            padded = padded.replace(separator, " ")
        return lowered in padded.split()
    return lowered in lowered_line


def _text_candidate_score(
    *,
    rel_path: str,
    matched_terms: set[str],
    hit_count: int,
    enclosed_symbol_count: int,
    line_refs: list[dict[str, Any]],
    top_symbol_score: int,
) -> int:
    top_line_score = sum(int(item.get("score") or 0) for item in line_refs[:3])
    top_line_terms = {
        term
        for item in line_refs
        for term in item.get("matched_terms", [])
        if isinstance(term, str)
    }
    score = max(top_line_score, top_symbol_score)
    score += _request_term_group_count(top_line_terms) * 52
    score += _request_term_group_count(matched_terms) * 10
    score += min(hit_count, 12)
    score += min(enclosed_symbol_count, 5) * 6
    if rel_path.startswith(("src/", "lib/", "app/", "packages/", "includes/")):
        score += 8
    if _path_looks_like_test(rel_path):
        score -= 120
    if _path_looks_like_reference_only(rel_path):
        score -= 180
        if {"doc", "docs", "documentation", "readme", "release", "deprecation"} & {
            term.lower() for term in matched_terms
        }:
            score += 80
    if hit_count > 200:
        score -= min(80, (hit_count - 200) // 8)
    if rel_path in {"autoload.php", "includes/MainConfigSchema.php"}:
        score -= 80
    if rel_path.endswith("/HookRunner.php") or rel_path.endswith("/Resources.php"):
        score -= 60
    return score


def _text_line_score(line: str, terms: list[str]) -> int:
    score = sum(min(len(term), 14) for term in terms)
    score += _request_term_group_count(set(terms)) * 24
    stripped = line.strip()
    if "->" in stripped or "::" in stripped:
        score += 32
    if "(" in stripped and ")" in stripped:
        score += 12
    if "function " in stripped:
        score += 18
    if "deprecat" in stripped.lower():
        score += 12
    return score


def _text_candidate_confidence(
    *,
    matched_terms: set[str],
    hit_count: int,
    enclosed_symbol_count: int,
) -> float:
    if len(matched_terms) >= 4 and enclosed_symbol_count:
        return 0.84
    if len(matched_terms) >= 3:
        return 0.78
    if len(matched_terms) >= 2 and hit_count >= 2:
        return 0.68
    return 0.56


def _source_candidate_role(rel_path: str, matched_terms: set[str]) -> str:
    lowered_path = rel_path.lower()
    lowered_terms = {term.lower() for term in matched_terms}
    if (
        "release" in lowered_path
        or "deprecat" in lowered_terms
        or _path_looks_like_reference_only(rel_path)
    ):
        return "docs"
    if {"diff", "difference", "view", "viewing"} & lowered_terms:
        return "entrypoint"
    if {"watchlist", "watched", "notification", "notify"} & lowered_terms:
        return "state_change"
    return "source_candidate"


def _source_enclosing_symbol_at_callsite(
    *,
    repo_path: Path,
    rel_path: Any,
    line_number: Any,
) -> dict[str, Any] | None:
    if not isinstance(rel_path, str) or not rel_path:
        return None
    if not isinstance(line_number, int):
        return None
    if _path_looks_generated_or_vendor(rel_path):
        return None
    path = repo_path / rel_path
    try:
        if path.stat().st_size > 750_000:
            return None
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except OSError:
        return None
    symbols = _extract_source_symbols(lines, rel_path)
    return _enclosing_symbol(symbols, line_number)


def _request_term_group_count(matched_terms: set[str]) -> int:
    lowered = {term.lower() for term in matched_terms}
    groups = [
        {"diff", "difference", "diffonly"},
        {"revision", "revisions", "oldid"},
        {"watchlist", "watchlisted", "watched", "notification", "notify"},
        {"seen", "timestamp"},
        {"view", "viewed", "viewing"},
        {"deprecated", "deprecation"},
    ]
    return sum(1 for group in groups if lowered & group)


def _find_source_callers(
    *,
    repo_path: Path,
    symbol: str,
    definition_path: Any,
    max_results: int,
) -> list[dict[str, Any]]:
    if not symbol or max_results <= 0:
        return []
    skip_dirs = _source_search_skip_dirs()
    allowed_suffixes = _source_search_suffixes(include_docs=False)
    definition_suffix = ""
    if isinstance(definition_path, str) and definition_path:
        definition_suffix = Path(definition_path).suffix.lower()
    production_callers: list[dict[str, Any]] = []
    test_callers: list[dict[str, Any]] = []
    for root, dirs, files in os.walk(repo_path):
        dirs[:] = [
            item
            for item in dirs
            if item not in skip_dirs and not item.startswith(".")
        ]
        for filename in files:
            path = Path(root) / filename
            suffix = path.suffix.lower()
            if suffix and suffix not in allowed_suffixes:
                continue
            if definition_suffix and suffix and suffix != definition_suffix:
                continue
            rel_path = path.relative_to(repo_path).as_posix()
            if _path_looks_generated_or_vendor(rel_path):
                continue
            try:
                if path.stat().st_size > 750_000:
                    continue
                lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
            except OSError:
                continue
            for line_number, line in enumerate(lines, start=1):
                if not _line_looks_like_call(line, symbol):
                    continue
                if _line_looks_like_definition(line, symbol):
                    continue
                caller = {
                    "path": rel_path,
                    "line": line_number,
                    "symbol": symbol,
                    "text": line.strip()[:220],
                    "definition_path": definition_path,
                }
                if _path_looks_like_test(rel_path):
                    test_callers.append(caller)
                else:
                    production_callers.append(caller)
                    if len(production_callers) >= max_results:
                        return production_callers
    if production_callers:
        return production_callers[:max_results]
    return test_callers[:max_results]


def _path_looks_like_test(rel_path: str) -> bool:
    lowered = rel_path.lower()
    return (
        lowered.startswith(("test/", "tests/"))
        or "/test/" in lowered
        or "/tests/" in lowered
        or "test" in Path(lowered).name
    )


def _path_looks_like_reference_only(rel_path: str) -> bool:
    lowered = rel_path.lower()
    suffix = Path(lowered).suffix
    return (
        suffix in {".md", ".rst", ".txt"}
        or lowered.startswith(("doc/", "docs/", "documentation/"))
        or "/doc/" in lowered
        or "/docs/" in lowered
        or "/documentation/" in lowered
    )


def _is_noisy_callsite_symbol(symbol: str) -> bool:
    lowered = symbol.lower()
    return lowered in {
        "__construct",
        "add",
        "build",
        "cache",
        "create",
        "delete",
        "execute",
        "format",
        "get",
        "getcached",
        "handle",
        "init",
        "load",
        "make",
        "new",
        "parse",
        "process",
        "remove",
        "render",
        "run",
        "save",
        "set",
        "show",
        "update",
    }


def _path_looks_generated_or_vendor(rel_path: str) -> bool:
    lowered = rel_path.lower()
    noisy_parts = (
        "/dist/",
        "/generated/",
        "/lib/vendor/",
        "/resources/lib/",
        "/third_party/",
        "/vendor/",
    )
    if any(part in f"/{lowered}" for part in noisy_parts):
        return True
    return lowered.endswith((".bundle.js", ".min.js"))


def _line_looks_like_call(line: str, symbol: str) -> bool:
    stripped = line.strip()
    if not stripped or stripped.startswith(("//", "#", "*")):
        return False
    call_needles = (
        f"->{symbol}(",
        f"::{symbol}(",
        f".{symbol}(",
        f" {symbol}(",
        f"\t{symbol}(",
    )
    compact = stripped.replace(" ", "")
    if f"->{symbol}(" in compact or f"::{symbol}(" in compact:
        return True
    if stripped.startswith(f"{symbol}("):
        return True
    return any(needle in stripped for needle in call_needles)


def _line_looks_like_definition(line: str, symbol: str) -> bool:
    stripped = line.strip()
    definition_needles = (
        f"function {symbol}(",
        f"def {symbol}(",
        f"func {symbol}(",
        f"fn {symbol}(",
    )
    lowered = stripped.lower()
    return any(needle.lower() in lowered for needle in definition_needles)


def _request_symbol_queries(request: str) -> list[str]:
    stop_words = {
        "about",
        "after",
        "against",
        "also",
        "and",
        "before",
        "being",
        "between",
        "bug",
        "code",
        "command",
        "could",
        "defined",
        "does",
        "done",
        "file",
        "files",
        "find",
        "fix",
        "for",
        "from",
        "have",
        "here",
        "how",
        "implement",
        "implemented",
        "implementation",
        "into",
        "issue",
        "json",
        "located",
        "make",
        "marked",
        "marks",
        "need",
        "object",
        "not",
        "only",
        "output",
        "path",
        "prose",
        "question",
        "relative",
        "report",
        "repo",
        "repository",
        "request",
        "rules",
        "shape",
        "the",
        "should",
        "specific",
        "that",
        "their",
        "there",
        "this",
        "ticket",
        "seen",
        "viewed",
        "viewing",
        "what",
        "when",
        "where",
        "which",
        "who",
        "why",
        "with",
        "would",
        "you",
    }
    raw_terms = []
    normalized = request.replace("`", " ")
    for token in normalized.replace("/", " ").replace("-", " ").split():
        term = "".join(char for char in token if char.isalnum() or char == "_")
        if len(term) < 3:
            continue
        lowered = term.lower()
        if lowered in stop_words or _is_noisy_symbol_query(term):
            continue
        raw_terms.append(term)

    queries: list[str] = []
    seen: set[str] = set()
    for term in raw_terms:
        variants = [term]
        if "_" in term:
            variants.append(term.replace("_", ""))
        for variant in variants:
            lowered = variant.lower()
            if lowered not in seen:
                seen.add(lowered)
                queries.append(variant)
    return queries


def _request_file_queries(request: str) -> list[str]:
    terms = _request_symbol_queries(request)
    extra_terms: list[str] = []
    lowered_request = request.lower()
    if "watchlist" in lowered_request or "watchlisted" in lowered_request:
        extra_terms.extend(["watchlist", "watched"])
    if "diff" in lowered_request:
        extra_terms.extend(["diff", "difference"])
    if "revision" in lowered_request or "oldid" in lowered_request:
        extra_terms.extend(["revision", "rev"])
    if "page" in lowered_request:
        extra_terms.append("page")

    queries: list[str] = []
    seen: set[str] = set()
    for term in [*terms, *extra_terms]:
        lowered = term.lower()
        if lowered and lowered not in seen:
            seen.add(lowered)
            queries.append(term)
    return queries


def _request_text_search_terms(request: str) -> list[str]:
    """Return terms for source-text evidence.

    This intentionally keeps some behavioral words that are too noisy for
    symbol search. Text search can require line-level evidence and term
    diversity, so words like "view" and "seen" are useful there.
    """
    base_terms = _request_file_queries(request)
    lowered_request = request.lower()
    extra_terms: list[str] = []
    if "watchlist" in lowered_request or "watchlisted" in lowered_request:
        extra_terms.extend(["watchlist", "watchlisted", "watched", "notification"])
    if "seen" in lowered_request:
        extra_terms.extend(["seen", "notification", "timestamp"])
    if "view" in lowered_request:
        extra_terms.extend(["view", "viewed", "viewing"])
    if "diff" in lowered_request:
        extra_terms.extend(["diff", "difference", "diffonly"])
    if "revision" in lowered_request or "oldid" in lowered_request:
        extra_terms.extend(["revision", "revisions", "oldid"])
    if "deprecat" in lowered_request:
        extra_terms.extend(["deprecated", "deprecation"])

    noisy = {
        "all",
        "answer",
        "bug",
        "change",
        "code",
        "exactly",
        "file",
        "files",
        "include",
        "includes",
        "instead",
        "json",
        "object",
        "one",
        "only",
        "output",
        "page",
        "relative",
        "report",
        "rev",
        "shape",
        "specific",
    }
    terms: list[str] = []
    seen: set[str] = set()
    for term in [*base_terms, *extra_terms]:
        cleaned = "".join(char for char in term if char.isalnum() or char in "_-")
        lowered = cleaned.lower()
        if len(lowered) < 3 or lowered in noisy or _is_noisy_symbol_query(cleaned):
            continue
        if lowered not in seen:
            seen.add(lowered)
            terms.append(cleaned)
    return terms


def _request_investigation_terms(request: str) -> list[str]:
    terms = _request_file_queries(request)
    ranked: list[tuple[int, str]] = []
    generic_terms = {
        "all",
        "diff",
        "file",
        "files",
        "include",
        "includes",
        "page",
        "revision",
        "revisions",
        "rev",
    }
    for term in terms:
        lowered = term.lower()
        score = 0
        if lowered not in generic_terms:
            score += 10
        if any(char.isupper() for char in term[1:]):
            score += 4
        if "_" in term:
            score += 3
        score += min(len(term), 12)
        ranked.append((score, term))
    ranked.sort(key=lambda item: (item[0], item[1].lower()), reverse=True)
    result: list[str] = []
    seen: set[str] = set()
    for _, term in ranked:
        lowered = term.lower()
        if lowered not in seen:
            seen.add(lowered)
            result.append(term)
    return result


def _is_noisy_symbol_query(term: str) -> bool:
    """Return true for request metadata terms that are bad symbol-search keys."""
    stripped = term.strip()
    if not stripped:
        return True
    if stripped.isdigit():
        return True
    if len(stripped) >= 4 and stripped[0].upper() == "T" and stripped[1:].isdigit():
        return True
    digit_count = sum(1 for char in stripped if char.isdigit())
    alpha_count = sum(1 for char in stripped if char.isalpha())
    if digit_count >= 3 and alpha_count <= 2:
        return True
    return False


def _explore_usage_boundary_query(
    *,
    repo_path: Path,
    request_text: str,
    params: dict[str, Any],
    show_observability: bool,
) -> dict[str, Any]:
    validation_errors = _usage_boundary_validation_errors(params)
    if validation_errors:
        return _explore_error_payload(
            repo_path=repo_path,
            request_text=request_text,
            params=params,
            intent="usage_boundary_query",
            errors=validation_errors,
        )

    scope = str(params["scope"]).strip()
    roots = _usage_boundary_roots(params)
    include_methods = _usage_boundary_include_methods(params)
    boundary = _usage_boundary_boundary(params, scope)
    symbol_kind = str(params.get("symbol_kind", "public_top_level_function"))
    budget_ms = _positive_int_param(params, "budget_ms", default=10_000, minimum=0)
    max_evidence_per_symbol = _positive_int_param(
        params, "max_evidence_per_symbol", default=5, minimum=1
    )

    try:
        result = usage_boundary_query_answer(
            repo_path,
            scope,
            roots=roots,
            include_methods=include_methods,
            budget_ms=budget_ms,
            max_evidence_per_symbol=max_evidence_per_symbol,
        )
    except EngineError as exc:
        return _explore_error_payload(
            repo_path=repo_path,
            request_text=request_text,
            params=params,
            intent="usage_boundary_query",
            errors=[str(exc)],
            failure_reason=str(exc),
        )

    answer_items = [
        _dead_code_eval_item(candidate)
        for candidate in result.get("candidates", [])
        if candidate.get("status") in {"Unused", "Ambiguous"}
    ]
    excluded_items = [
        _dead_code_eval_item(candidate) for candidate in result.get("excluded", [])
    ]
    command_observability = _dead_code_command_observability(
        repo_path=repo_path,
        scope=scope,
        roots=roots,
        boundary="outside-directory",
        include_methods=include_methods,
        output_format="answer-json",
        result=result,
    )
    command_observability["command"] = "explore"
    command_observability["mode"] = "explore"
    command_observability["intent"] = "usage_boundary_query"
    command_observability["internal_analyzer"] = "analyze usage-boundary"
    command_observability["budget_ms"] = budget_ms
    command_observability["max_evidence_per_symbol"] = max_evidence_per_symbol
    command_observability["full_observability_requested"] = show_observability

    response = {
        "schema_version": "aethyme-explore-v1",
        "mode": "explore",
        "intent": "usage_boundary_query",
        "status": "complete",
        "request": {
            "raw": request_text,
            "parameters": params,
        },
        "resolved_parameters": {
            "scope": scope,
            "boundary": boundary,
            "symbol_kind": symbol_kind,
            "search_roots": roots,
            "include_methods": include_methods,
            "budget_ms": budget_ms,
            "max_evidence_per_symbol": max_evidence_per_symbol,
        },
        "answer": answer_items,
        "excluded": excluded_items,
        "ambiguous": [
            item
            for item in answer_items
            if item.get("status") == "Ambiguous"
            or item.get("evidence", {}).get("ambiguity")
        ],
        "evidence": {
            "answer_count": len(answer_items),
            "excluded_count": len(excluded_items),
            "searched_roots": roots,
            "boundary": boundary,
            "engine_summary": result.get("summary", {}),
        },
        "confidence": {
            "overall": _overall_confidence(answer_items),
            "answer_summary": _confidence_summary_for_items(answer_items),
            "excluded_summary": _confidence_summary_for_items(excluded_items),
            "analyzed_summary": command_observability.get("confidence_summary", {}),
        },
        "output_adapters": {
            "dead_code_eval_json": {
                "unused_functions": answer_items,
            }
        },
        "observability": command_observability,
        "degraded_reasons": command_observability.get("degraded_reasons", []),
        "next_actions": _usage_boundary_next_actions(
            answer_items=answer_items,
            degraded_reasons=command_observability.get("degraded_reasons", []),
        ),
    }
    return _trim_explore_response(
        response, detail="compact", show_observability=show_observability
    )


def _explore_selection_required_payload(
    *,
    repo_path: Path,
    request_text: str,
    params: dict[str, Any],
) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    catalog = _intent_catalog()
    available_intents = []
    for mode in catalog.get("modes", []):
        if not isinstance(mode, dict) or mode.get("mode") != "explore":
            continue
        available_intents.extend(mode.get("intents", []))

    return {
        "schema_version": "aethyme-explore-v1",
        "mode": "explore",
        "status": "intent_selection_required",
        "request": {
            "raw": request_text,
            "parameters": params,
        },
        "selection_contract": catalog["selection_contract"],
        "available_intents": available_intents,
        "answer": [],
        "excluded": [],
        "ambiguous": [],
        "next_actions": [
            "Choose one available intent from available_intents.",
            "Fill that intent's required params from the user request.",
            "Rerun explore with --intent and --params to get answer-shaped evidence.",
        ],
        "observability": {
            "command": "explore",
            "mode": "explore",
            "repo_path": str(snapshot.repo_path),
            "status": "intent_selection_required",
            "index_freshness": {
                "status": "fresh_for_current_snapshot",
                "repo_dirty": snapshot.dirty,
                "commit": snapshot.commit,
                "snapshot_key": snapshot.cache_key,
                "file_count": snapshot.file_count,
            },
            "failure_reason": None,
            "degraded_reasons": [],
            "output_size_bytes": None,
        },
    }


def _usage_boundary_validation_errors(params: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    scope = params.get("scope")
    if not isinstance(scope, str) or not scope.strip():
        errors.append("Missing required parameter: scope.")

    symbol_kind = str(params.get("symbol_kind", "public_top_level_function"))
    supported_symbol_kinds = {
        "public_top_level_function",
        "public_function_or_method",
        "public_method",
    }
    if symbol_kind not in supported_symbol_kinds:
        errors.append(
            "Unsupported symbol_kind. Expected one of: "
            + ", ".join(sorted(supported_symbol_kinds))
            + "."
        )

    boundary = params.get("boundary")
    if isinstance(boundary, dict):
        boundary_type = boundary.get("type")
        if boundary_type not in {None, "outside_directory"}:
            errors.append("Unsupported boundary.type. Expected outside_directory.")
    elif boundary is not None and not isinstance(boundary, str):
        errors.append("boundary must be a string or object.")

    roots = params.get("search_roots", params.get("roots", []))
    if isinstance(roots, list) and not all(isinstance(item, str) for item in roots):
        errors.append("search_roots must contain strings only.")
    elif not isinstance(roots, (str, list)):
        errors.append("search_roots must be a string or list of strings.")

    return errors


def _usage_boundary_roots(params: dict[str, Any]) -> list[str]:
    raw_roots = params.get("search_roots", params.get("roots", []))
    if isinstance(raw_roots, str):
        return [item.strip() for item in raw_roots.split(",") if item.strip()]
    if isinstance(raw_roots, list):
        return [
            item.strip()
            for item in raw_roots
            if isinstance(item, str) and item.strip()
        ]
    return []


def _usage_boundary_include_methods(params: dict[str, Any]) -> bool:
    if "include_methods" in params:
        return bool(params["include_methods"])
    symbol_kind = str(params.get("symbol_kind", "public_top_level_function"))
    return symbol_kind in {"public_function_or_method", "public_method"}


def _positive_int_param(
    params: dict[str, Any],
    key: str,
    *,
    default: int,
    minimum: int,
) -> int:
    raw_value = params.get(key, default)
    try:
        value = int(raw_value)
    except (TypeError, ValueError):
        return default
    return max(minimum, value)


def _usage_boundary_boundary(params: dict[str, Any], scope: str) -> dict[str, str]:
    boundary = params.get("boundary")
    if isinstance(boundary, dict):
        return {
            "type": str(boundary.get("type", "outside_directory")),
            "path": str(boundary.get("path", scope)),
        }
    if isinstance(boundary, str) and boundary.strip():
        return {"type": "outside_directory", "path": boundary.strip()}
    return {"type": "outside_directory", "path": scope}


def _usage_boundary_next_actions(
    *,
    answer_items: list[dict[str, Any]],
    degraded_reasons: list[Any],
) -> list[str]:
    actions = ["Use answer[] as the primary task result."]
    if any(item.get("status") == "Ambiguous" for item in answer_items):
        actions.append("Review Ambiguous results before treating them as removable dead code.")
    if degraded_reasons:
        actions.append("Inspect degraded_reasons before relying on confidence values.")
    return actions


def _explore_error_payload(
    *,
    repo_path: Path,
    request_text: str,
    params: dict[str, Any],
    intent: str,
    errors: list[str],
    failure_reason: str | None = None,
) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    failure = failure_reason or "; ".join(errors)
    response = {
        "schema_version": "aethyme-explore-v1",
        "mode": "explore",
        "intent": intent,
        "request": {
            "raw": request_text,
            "parameters": params,
        },
        "resolved_parameters": {},
        "answer": [],
        "navigation_hints": [],
        "excluded": [],
        "evidence": {
            "answer_count": 0,
            "navigation_hint_count": 0,
            "excluded_count": 0,
        },
        "confidence": {
            "overall": None,
            "answer_summary": _confidence_summary_for_items([]),
            "excluded_summary": _confidence_summary_for_items([]),
            "analyzed_summary": {},
        },
        "output_adapters": {"dead_code_eval_json": {"unused_functions": []}},
        "safe_to_use_as_answer": False,
        "safe_to_use_as_navigation": False,
        "trust_policy": {
            "safe_to_use_as_answer": False,
            "safe_to_use_as_navigation": False,
            "evidence_level": "none",
            "authoritative_answer_count": 0,
            "navigation_hint_count": 0,
            "degraded": True,
            "trust_policy": "failed",
            "reason": failure,
        },
        "observability": {
            "command": "explore",
            "mode": "explore",
            "intent": intent,
            "repo_path": str(snapshot.repo_path),
            "index_freshness": {
                "status": "not_analyzed",
                "repo_dirty": snapshot.dirty,
                "commit": snapshot.commit,
                "snapshot_key": snapshot.cache_key,
                "file_count": snapshot.file_count,
            },
            "engine": engine_runtime_info(),
            "graph_fact_count": {"graph": {}, "facts": {}},
            "confidence_summary": {},
            "navigation_confidence_summary": {},
            "evidence_level": "none",
            "trust_policy": {
                "safe_to_use_as_answer": False,
                "safe_to_use_as_navigation": False,
                "evidence_level": "none",
                "authoritative_answer_count": 0,
                "navigation_hint_count": 0,
                "degraded": True,
                "trust_policy": "failed",
                "reason": failure,
            },
            "degraded_reasons": errors,
            "failure_reason": failure,
            "output_size_bytes": None,
        },
        "degraded_reasons": errors,
        "next_actions": ["Provide required parameters and rerun the same intent."],
    }
    return _trim_explore_response(
        response, detail="compact", show_observability=False
    )


def _overall_confidence(items: list[dict[str, Any]]) -> float | None:
    values = [
        confidence
        for item in items
        if (confidence := _confidence_value(item.get("confidence"))) is not None
    ]
    if not values:
        return None
    return round(min(values), 4)


def _best_confidence(items: list[dict[str, Any]]) -> float | None:
    values = [
        confidence
        for item in items
        if (confidence := _confidence_value(item.get("confidence"))) is not None
    ]
    if not values:
        return None
    return round(max(values), 4)


def _confidence_summary_for_items(items: list[dict[str, Any]]) -> dict[str, Any]:
    values = [
        confidence
        for item in items
        if (confidence := _confidence_value(item.get("confidence"))) is not None
    ]
    if not values:
        return {"high": 0, "medium": 0, "low": 0, "min": None, "max": None}
    return {
        "high": sum(1 for value in values if value >= 0.8),
        "medium": sum(1 for value in values if 0.5 <= value < 0.8),
        "low": sum(1 for value in values if value < 0.5),
        "min": round(min(values), 4),
        "max": round(max(values), 4),
    }


def _confidence_value(raw_value: Any) -> float | None:
    if isinstance(raw_value, bool):
        return None
    if isinstance(raw_value, (int, float)):
        return float(raw_value)
    return None


@cli.group()
def eval() -> None:
    """Local evaluation harnesses for Aethyme Core."""


@eval.command("run")
@click.option(
    "--eval-type",
    required=True,
    type=click.Choice(
        [
            "bug-fix",
            "bug-fix-1",
            "explain-repo",
            "navigation-ctf",
            "impact-analysis",
            "feature-localization",
            "config-audit",
            "dead-code",
            "migration",
        ]
    ),
    help="Type of evaluation to run",
)
@click.option(
    "--target",
    required=True,
    type=click.Choice(["grc", "mediawiki"]),
    help="Target playground repository",
)
@click.option(
    "--model",
    required=True,
    type=click.Choice(["haiku", "sonnet", "opus", "gpt-5.4"]),
    help="Agent model to use",
)
@click.option(
    "--scenario",
    type=click.Choice(["implication-share", "cross-package"]),
    default=None,
    help="Bug-fix scenario",
)
@click.option(
    "--reasoning",
    type=click.Choice(["default", "high", "low"]),
    default="default",
    help="Reasoning effort level",
)
@click.option(
    "--dest",
    "dest_dir",
    type=click.Path(path_type=Path),
    default=None,
    help="Override dest dir for bug-fix clones",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON plan")
def eval_run(
    eval_type: str,
    target: str,
    model: str,
    scenario: str | None,
    reasoning: str,
    dest_dir: Path | None,
    json_output: bool,
) -> None:
    """Generate a complete eval run plan for Chau7 MCP execution."""
    from src.eval.orchestrator import generate_run_plan

    plan = generate_run_plan(
        eval_type=eval_type,
        target=target,
        model=model,
        scenario=scenario,
        reasoning=reasoning,
        dest_dir=str(dest_dir) if dest_dir else None,
    )

    if json_output:
        click.echo(json.dumps(plan, indent=2))
        return

    meta = plan["meta"]
    click.echo(f"Eval Type:  {meta['eval_type']}")
    click.echo(f"Target:     {meta['target_display']}")
    click.echo(f"Model:      {meta['model']['name']} ({meta['model']['provider']})")
    click.echo(f"Backend:    {meta['model']['backend']}")
    click.echo(f"Conditions: {', '.join(meta['conditions'])}")
    if meta.get("scenario"):
        click.echo(f"Scenario:   {meta['scenario']}")
    click.echo(f"Commit:     {meta['aethyme_commit'][:8]}")
    click.echo()
    click.echo(f"Phases ({len(plan['phases'])}):")
    for i, phase in enumerate(plan["phases"]):
        click.echo(f"  {i}. {phase['name']}: {phase['description']}")
    click.echo()
    click.echo("Run with --json-output for the full plan dict.")


@eval.command("targets")
def eval_targets() -> None:
    """List available eval targets and their validation status."""
    from src.eval.targets import list_targets

    for tgt in list_targets():
        errors = tgt.validate()
        status = "OK" if not errors else f"{len(errors)} ERRORS"
        click.echo(f"{tgt.name}: {tgt.display_name} [{status}]")
        click.echo(f"  Control: {tgt.control_path}")
        click.echo(f"  Aethyme: {tgt.aethyme_path}")
        if errors:
            for err in errors:
                click.echo(f"  ERROR: {err}")


@eval.command("setup-repos")
@click.option(
    "--source",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Source repo to clone",
)
@click.option(
    "--dest",
    required=True,
    type=click.Path(path_type=Path),
    help="Destination directory for 4 condition clones",
)
def eval_setup_repos(source: Path, dest: Path) -> None:
    """Create 4 isolated repo clones for benchmark conditions."""
    repos = create_condition_repos(source, dest)
    for cond, path in repos.items():
        click.echo(f"{cond}: {path}")


def _condition_cmd_options(func):
    """Add the five condition-command click options shared by `eval explain-repo`
    and `eval navigation-ctf`. Includes the deprecated `--baseline-cmd` /
    `--aethyme-cmd` aliases so old scripts keep working — pair with
    :func:`_resolve_legacy_cmd_aliases` inside the command body to map them.
    """
    func = click.option(
        "--aethyme-cmd", hidden=True, help="[Deprecated] Alias for --leverage-cmd"
    )(func)
    func = click.option(
        "--baseline-cmd", hidden=True, help="[Deprecated] Alias for --control-cmd"
    )(func)
    func = click.option(
        "--leverage-cmd",
        help="Command for the leverage run (pre-computed context + tools)",
    )(func)
    func = click.option(
        "--explore-cmd",
        help="Command for the explore run (tools in prompt, no context)",
    )(func)
    func = click.option(
        "--control-cmd", help="Command for the control run (no context, no tools)"
    )(func)
    return func


def _resolve_legacy_cmd_aliases(
    control_cmd: str | None,
    leverage_cmd: str | None,
    baseline_cmd: str | None,
    aethyme_cmd: str | None,
) -> tuple[str | None, str | None]:
    """Map the deprecated `--baseline-cmd` → `--control-cmd` and
    `--aethyme-cmd` → `--leverage-cmd`. Returns the resolved (control, leverage)
    pair. Modern flags win when both are passed.
    """
    if control_cmd is None and baseline_cmd is not None:
        control_cmd = baseline_cmd
    if leverage_cmd is None and aethyme_cmd is not None:
        leverage_cmd = aethyme_cmd
    return control_cmd, leverage_cmd


@eval.command("explain-repo")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--task",
    "task_text",
    default=DEFAULT_TASK,
    show_default=True,
    help="Task description",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@_condition_cmd_options
def eval_explain_repo(
    repo_path: Path,
    task_text: str,
    json_output: bool,
    control_cmd: str | None,
    explore_cmd: str | None,
    leverage_cmd: str | None,
    baseline_cmd: str | None,
    aethyme_cmd: str | None,
) -> None:
    """Build the control artifacts for a local explain-repo benchmark."""
    control_cmd, leverage_cmd = _resolve_legacy_cmd_aliases(
        control_cmd, leverage_cmd, baseline_cmd, aethyme_cmd
    )

    try:
        control_runner = (
            command_runner(control_cmd, working_directory=repo_path)
            if control_cmd
            else None
        )
        explore_runner = (
            command_runner(explore_cmd, working_directory=repo_path)
            if explore_cmd
            else None
        )
        leverage_runner = (
            command_runner(leverage_cmd, working_directory=repo_path)
            if leverage_cmd
            else None
        )
        result = run_explain_repo_evaluation(
            repo_path,
            task_text,
            control_runner=control_runner,
            explore_runner=explore_runner,
            leverage_runner=leverage_runner,
        )
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    report = result["report"]
    report_path = result["report_path"]
    condition_chars = report.get("condition_prompt_chars", {})
    condition_runs = report.get("condition_runs", {})
    click.echo(f"Task: {result['task']}")
    for cond in ("control", "explore", "leverage"):
        if cond in condition_chars:
            click.echo(f"{cond.title()} prompt chars: {condition_chars[cond]}")
    click.echo(f"Navigation items: {report['navigation_items']}")
    click.echo(f"Risk items: {report['risk_items']}")
    for cond in ("control", "explore", "leverage"):
        run = condition_runs.get(cond)
        if run:
            click.echo(f"{cond.title()} duration: {run['duration_seconds']:.3f}s")
    click.echo(f"Markdown report: {report_path}")
    click.echo("\nExplanation:\n")
    click.echo(result["explanation"])


@eval.command("navigation-ctf")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--task",
    "task_text",
    default=DEFAULT_NAVIGATION_TASK,
    show_default=True,
    help="Task description",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@_condition_cmd_options
def eval_navigation_ctf(
    repo_path: Path,
    task_text: str,
    json_output: bool,
    control_cmd: str | None,
    explore_cmd: str | None,
    leverage_cmd: str | None,
    baseline_cmd: str | None,
    aethyme_cmd: str | None,
) -> None:
    """Build a directed repository navigation benchmark from real graph relations."""
    control_cmd, leverage_cmd = _resolve_legacy_cmd_aliases(
        control_cmd, leverage_cmd, baseline_cmd, aethyme_cmd
    )

    try:
        control_runner = (
            navigation_command_runner(control_cmd, working_directory=repo_path)
            if control_cmd
            else None
        )
        explore_runner = (
            navigation_command_runner(explore_cmd, working_directory=repo_path)
            if explore_cmd
            else None
        )
        leverage_runner = (
            navigation_command_runner(leverage_cmd, working_directory=repo_path)
            if leverage_cmd
            else None
        )
        result = run_navigation_ctf_evaluation(
            repo_path,
            task_text,
            control_runner=control_runner,
            explore_runner=explore_runner,
            leverage_runner=leverage_runner,
        )
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    report = result["report"]
    condition_chars = report.get("condition_prompt_chars", {})
    condition_runs = report.get("condition_runs", {})
    click.echo(f"Task: {result['task']}")
    for cond in ("control", "explore", "leverage"):
        if cond in condition_chars:
            click.echo(f"{cond.title()} prompt chars: {condition_chars[cond]}")
    click.echo(f"Navigation items: {report['navigation_items']}")
    for cond in ("control", "explore", "leverage"):
        run = condition_runs.get(cond)
        if run:
            click.echo(f"{cond.title()} duration: {run['duration_seconds']:.3f}s")
    click.echo(f"Markdown report: {result['report_path']}")
    click.echo("\nReference output:\n")
    click.echo(json.dumps(result["reference_output"], indent=2))


@eval.group("bug-fix")
def eval_bug_fix_group() -> None:
    """Bug-fix evaluation: plant a deterministic bug, let agents fix it."""


@eval_bug_fix_group.command("setup")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
def eval_bug_fix_setup(repo_path: Path) -> None:
    """Plant the bug and create the test file in a Playground repo."""
    result = setup_bug_fix(repo_path)
    click.echo(json.dumps(result, indent=2))


@eval_bug_fix_group.command("verify")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
def eval_bug_fix_verify(repo_path: Path) -> None:
    """Verify the bug is planted: planted test should fail, existing tests should pass."""
    result = verify_bug_fix_setup(repo_path)
    click.echo(json.dumps(result, indent=2, default=str))


@eval_bug_fix_group.command("reset")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
def eval_bug_fix_reset(repo_path: Path) -> None:
    """Restore rbac-canonical.ts to its committed (buggy) state."""
    reset_bug_fix(repo_path)
    click.echo("Reset complete.")


@eval_bug_fix_group.command("generate")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
@click.option(
    "--task",
    "task_text",
    default=DEFAULT_BUG_FIX_TASK,
    show_default=True,
    help="Task description",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def eval_bug_fix_generate(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Generate all eval artifacts (prompts, schema, reference, nav context)."""
    try:
        result = run_bug_fix_evaluation(repo_path, task_text)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Task: {result['task']}")
    click.echo(f"Report: {result['report_path']}")
    click.echo("\nTest commands:")
    for name, cmd in result["test_commands"].items():
        click.echo(f"  {name}: {cmd}")
    click.echo("\nReference:")
    click.echo(json.dumps(result["reference"], indent=2))


@eval_bug_fix_group.command("prepare")
@click.option(
    "--source",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Source repo to clone",
)
@click.option(
    "--dest",
    required=True,
    type=click.Path(path_type=Path),
    help="Destination directory for 4 condition clones",
)
@click.option(
    "--task", "task_text", default=None, help="Task description (auto-set per scenario)"
)
@click.option(
    "--scenario",
    type=click.Choice(["implication-share", "cross-package"]),
    default="implication-share",
    show_default=True,
    help="Bug scenario to plant",
)
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def eval_bug_fix_prepare(
    source: Path, dest: Path, task_text: str | None, scenario: str, json_output: bool
) -> None:
    """One-step: clone 4 repos, plant bug, generate all artifacts."""
    try:
        if scenario == "cross-package":
            task = task_text or DEFAULT_CROSS_PACKAGE_TASK
            result = prepare_cross_package_benchmark(source, dest, task=task)
        else:
            task = task_text or DEFAULT_BUG_FIX_TASK
            result = prepare_bug_fix_benchmark(source, dest, task=task)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc

    if json_output:
        # `cleanup_timer` is a threading.Timer the orchestrator uses to
        # auto-delete the benchmark dir after 5 min. It's not part of
        # the JSON contract — strip it before serializing.
        printable = {k: v for k, v in result.items() if k != "cleanup_timer"}
        click.echo(json.dumps(printable, indent=2))
        return

    click.echo(f"Scenario: {scenario}")
    click.echo("Repos created:")
    for cond, path in result["repos"].items():
        click.echo(f"  {cond}: {path}")
    click.echo("\nArtifacts written:")
    for name, path in result["artifacts"].items():
        click.echo(f"  {name}: {path}")
    click.echo("\nTest commands:")
    for name, cmd in result["test_commands"].items():
        click.echo(f"  {name}: {cmd}")


@cli.command()
@click.argument("symbol")
@click.option("--depth", "-d", default=2, help="Depth for ego graph traversal")
@click.pass_context
def ego(ctx: click.Context, symbol: str, depth: int) -> None:
    """Get ego graph for a symbol."""
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    if not tenant_id:
        tenant_id = default_tenant_id()
        if tenant_id is None:
            click.echo("No tenant found. Please index a repository first.")
            return

    store = GraphStore(tenant_id=tenant_id)
    result = store.ego_graph(symbol, depth=depth, limit=100)

    if "error" in result:
        click.echo(f"Error: {result['error']}")
        return

    click.echo(f"\nEgo Graph for: {symbol}")
    click.echo("=" * 60)

    if result.get("definition"):
        defn = result["definition"]
        click.echo("\nDefinition:")
        click.echo(f"  File: {defn['file_path']}:{defn['line_number']}")
        click.echo(f"  Kind: {defn['kind']}")
        click.echo(f"  Language: {defn['language']}")

    for depth_level, nodes in result.get("nodes_by_depth", {}).items():
        if depth_level == 0:
            continue  # Skip definition

        click.echo(f"\nDepth {depth_level}: {len(nodes)} nodes")
        for node in nodes[:5]:  # Show first 5
            click.echo(
                f"  - {node['symbol']} ({node['kind']}) at {node['file_path']}:{node['line_number']}"
            )
        if len(nodes) > 5:
            click.echo(f"  ... and {len(nodes) - 5} more")


@cli.command()
@click.argument("symbol")
@click.option("--max-depth", "-d", default=10, help="Maximum depth for impact analysis")
@click.pass_context
def impact(ctx: click.Context, symbol: str, max_depth: int) -> None:
    """Analyze impact of changes to a symbol."""
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    if not tenant_id:
        tenant_id = default_tenant_id()
        if tenant_id is None:
            click.echo("No tenant found. Please index a repository first.")
            return

    store = GraphStore(tenant_id=tenant_id)
    result = store.impact_analysis(symbol, max_depth=max_depth)

    if "error" in result:
        click.echo(f"Error: {result['error']}")
        return

    click.echo(f"\nImpact Analysis for: {symbol}")
    click.echo("=" * 60)
    click.echo(f"Total Impacted: {result['total_impacted']} symbols")
    click.echo(f"Max Depth Reached: {result['max_depth_reached']}")

    for depth, symbols in result.get("by_depth", {}).items():
        click.echo(f"\nDepth {depth}: {len(symbols)} symbols")
        for sym in symbols[:10]:  # Show first 10
            click.echo(f"  - {sym['symbol']} in {sym['file']}")
        if len(symbols) > 10:
            click.echo(f"  ... and {len(symbols) - 10} more")


@cli.command()
@click.argument("query")
@click.option("--limit", "-l", default=20, help="Maximum number of results")
@click.option(
    "--type",
    "-t",
    default="hybrid",
    type=click.Choice(["exact", "fuzzy", "hybrid"]),
    help="Search type",
)
@click.pass_context
def search(ctx: click.Context, query: str, limit: int, type: str) -> None:
    """Search for symbols in the graph."""
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    if not tenant_id:
        tenant_id = default_tenant_id()
        if tenant_id is None:
            click.echo("No tenant found. Please index a repository first.")
            return

    store = GraphStore(tenant_id=tenant_id)
    results = store.search(query, limit=limit, search_type=type)

    click.echo(f"\nSearch Results for: '{query}'")
    click.echo("=" * 60)
    click.echo(f"Found {len(results)} results (search type: {type})")
    click.echo()

    for i, result in enumerate(results, 1):
        score = result.get("score", 0)
        click.echo(
            f"{i:2}. {result['symbol']} " f"({result['kind']}) " f"[score: {score:.3f}]"
        )
        click.echo(f"    {result['file_path']}:{result['line_number']}")
        if result.get("documentation"):
            doc = result["documentation"][:100]
            click.echo(f"    {doc}...")


@cli.command(name="ai-ready")
@click.option(
    "--repo",
    type=click.Path(exists=True),
    help="Repository path (defaults to current directory)",
)
@click.option(
    "--org",
    help="Organization ID for API mode",
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
    org: str | None,
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

    # Get tenant_id if using API mode
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))
    if org and not tenant_id:
        # Look up tenant by org name
        tenant_lookup = db_pool.execute(
            "SELECT id FROM aethyme.tenants WHERE slug = %s LIMIT 1", (org,)
        )
        if tenant_lookup:
            tenant_id = str(tenant_lookup[0]["id"])

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
    (per-product detailed runbooks). All four are derived from the canonical
    templates in packages/aethyme/skills/aethyme/.
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
    """Write all four discoverability files into the target repo."""
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
    """Check that the four discoverability files are present and substituted."""
    from src.enhance import is_ok, verify

    results = verify(repo_path)
    for r in results:
        ok = r.exists and not r.placeholder_present
        marker = "OK" if ok else "FAIL"
        notes: list[str] = []
        if not r.exists:
            notes.append("missing")
        elif r.placeholder_present:
            notes.append("placeholder not substituted")
        if r.exists and not r.matches_canonical:
            notes.append("content drift (allowed)")
        suffix = f"  ({', '.join(notes)})" if notes else ""
        click.echo(f"  [{marker:4}] {r.target.relative_path}{suffix}")
    if not is_ok(results):
        click.echo("Verification failed.", err=True)
        sys.exit(1)
    click.echo("All discoverability files present and substituted.")


def main() -> None:
    """Main entry point for CLI."""
    # Lazy import + registration of the daemon group: defining it inline
    # here would force the daemon module's signal/socket imports onto every
    # one-shot CLI invocation, defeating the daemon's startup-cost story.
    from src.daemon import daemon as _daemon_group

    if "daemon" not in cli.commands:
        cli.add_command(_daemon_group)
    cli(obj={})


if __name__ == "__main__":
    main()
