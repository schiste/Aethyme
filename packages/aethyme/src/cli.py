import json
import sys
from pathlib import Path
from typing import Any, TypeAlias, TypedDict, cast

import click
import structlog

# Add src to path for module imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from src.eval.explain_repo import (
    DEFAULT_TASK,
    command_runner,
    run_explain_repo_evaluation,
)
from src.eval.bug_fix import (
    DEFAULT_CROSS_PACKAGE_TASK,
    DEFAULT_TASK as DEFAULT_BUG_FIX_TASK,
)
from src.eval.bug_fix import (
    command_runner as bug_fix_command_runner,
    prepare_bug_fix_benchmark,
    prepare_cross_package_benchmark,
    run_bug_fix_evaluation,
    setup_bug_fix,
    verify_bug_fix_setup,
    reset_bug_fix,
)
from src.eval.repos import create_condition_repos, load_condition_repos
from src.eval.navigation_ctf import (
    DEFAULT_TASK as DEFAULT_NAVIGATION_TASK,
)
from src.eval.navigation_ctf import (
    command_runner as navigation_command_runner,
)
from src.eval.navigation_ctf import (
    run_navigation_ctf_evaluation,
)
from src.graph.connection_pool import db_pool
from src.graph.store import GraphStore
from src.indexing.engine import (
    EngineError,
    build_task_context,
    build_task_pack,
    clear_repository_cache,
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
    task_anchors,
    task_expand,
    task_next,
    task_scope,
)
from src.indexing.engine import (
    dependency_frontier as rust_dependency_frontier,
)
from src.indexing.engine import (
    impact_frontier as rust_impact_frontier,
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
        structlog.dev.ConsoleRenderer()
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
    "--verbose", "-v",
    is_flag=True,
    help="Verbose output",
)
@click.pass_context
def cli(
    ctx: click.Context,
    tenant_id: str | None,
    output_json_flag: bool,
    verbose: bool,
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


@cli.group()
def repo() -> None:
    """Local repository intake and inspection workflows."""


@repo.command("ingest")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
def repo_ingest(repo_path: Path) -> None:
    """Capture local repository metadata for a local-first workflow."""
    snapshot = capture_snapshot(repo_path)
    click.echo(f"Repository: {snapshot.repo_name}")
    click.echo(f"Path: {snapshot.repo_path}")
    click.echo(f"Commit: {snapshot.commit or 'working-tree'}")
    click.echo(f"Files: {snapshot.file_count}")
    click.echo(f"Snapshot key: {snapshot.cache_key}")


@repo.command("inspect")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option("--mode", "mode", type=click.Choice(["brief", "structure", "full"]), default="full", help="Inspect depth: brief (areas+signals), structure (adds files/configs/docs), full (everything)")
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
            click.echo(f"- {name.replace('_', ' ')}: {signal['score']} ({signal['level']})")


@repo.command("clear-cache")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
def repo_clear_cache(repo_path: Path) -> None:
    """Clear cached local engine artifacts for the current repository snapshot."""
    clear_repository_cache(repo_path)
    click.echo(f"Cleared cache for {repo_path}")


@repo.command("warm")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
def repo_warm(repo_path: Path) -> None:
    """Pre-build the repository map cache for fast subsequent commands."""
    from src.indexing.engine import warm_repository
    warm_repository(repo_path)
    click.echo(f"Map cache warmed for {repo_path}")


@repo.command("deploy-skills")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--force", is_flag=True, help="Overwrite existing skills")
@click.option("--remove", "do_remove", is_flag=True, help="Remove deployed skills instead")
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


@cli.group()
def query() -> None:
    """Query the local Rust-backed navigation engine."""


@cli.group()
def graph() -> None:
    """Navigate graph entities and relations directly."""


@query.command("symbol")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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


def _render_relation(result: dict[str, Any], json_output: bool) -> None:
    if json_output:
        click.echo(json.dumps(result, indent=2))
        return
    click.echo(f"Target: {result['target']}")
    click.echo(f"Relation: {result['relation']}")
    for item in result["items"]:
        click.echo(f"- {item['display']} ({item['kind']}, {item['relation']}, conf={item['confidence']})")


@graph.command("children")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_children_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show structural children of a graph node."""
    try:
        _render_relation(graph_children(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("parents")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_parents_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show structural parents of a graph node."""
    try:
        _render_relation(graph_parents(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("callers")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_callers_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show callers for a function node."""
    try:
        _render_relation(graph_callers(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("callees")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_callees_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show callees for a function node."""
    try:
        _render_relation(graph_callees(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("docs")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_docs_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show documentation related to a graph node."""
    try:
        _render_relation(graph_docs(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("configs")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.argument("target")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def graph_configs_command(repo_path: Path, target: str, json_output: bool) -> None:
    """Show config and entrypoint links related to a graph node."""
    try:
        _render_relation(graph_configs(repo_path, target), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@graph.command("expand")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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
            click.echo(f"- {item['display']} ({item['kind']}, {item['relation']}, conf={item['confidence']})")
    if result.get("risks"):
        click.echo("Risks:")
        for risk in result["risks"]:
            click.echo(f"- {risk}")


@graph.command("overview")
@click.argument("repo_path", type=click.Path(exists=True, file_okay=False, path_type=Path))
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
            click.echo(f"- {name.replace('_', ' ')}: {signal['score']} ({signal['level']})")
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


@cli.group()
def task() -> None:
    """Task-context workflows over the local repository engine."""


@task.command("pack")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
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
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--content-budget", "content_budget", default=80000, type=int, help="Max bytes of file content")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_context_command(repo_path: Path, task_text: str, content_budget: int, json_output: bool) -> None:
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
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
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
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
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
            click.echo(f"- {item}")
    if result["in_scope_areas"]:
        click.echo("In-scope areas:")
        for item in result["in_scope_areas"]:
            click.echo(f"- {item}")
    if result["out_of_scope"]:
        click.echo("Out of scope:")
        for item in result["out_of_scope"]:
            click.echo(f"- {item}")
    if result["risks"]:
        click.echo("Risks:")
        for item in result["risks"]:
            click.echo(f"- {item}")


@task.command("next")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", required=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def task_next_command(repo_path: Path, task_text: str, json_output: bool) -> None:
    """Show the next recommended navigation steps for a task."""
    try:
        _render_relation(task_next(repo_path, task_text), json_output)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc


@task.command("expand")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
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


@task.command("explain")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", default=DEFAULT_TASK, show_default=True, help="Task description")
def task_explain(repo_path: Path, task_text: str) -> None:
    """Explain the repository using the deterministic Rust engine."""
    try:
        inspect = inspect_repository(repo_path)
        pack = build_task_pack(repo_path, task_text)
        explanation = render_explain_repo_text(inspect, pack)
    except EngineError as exc:
        raise click.ClickException(str(exc)) from exc
    click.echo(explanation)


@cli.group()
def eval() -> None:
    """Local evaluation harnesses for Aethyme Core."""


@eval.command("run")
@click.option(
    "--eval-type", required=True,
    type=click.Choice(["bug-fix", "bug-fix-1", "explain-repo", "navigation-ctf", "impact-analysis", "feature-localization", "config-audit", "dead-code", "migration"]),
    help="Type of evaluation to run",
)
@click.option(
    "--target", required=True,
    type=click.Choice(["grc", "mediawiki"]),
    help="Target playground repository",
)
@click.option(
    "--model", required=True,
    type=click.Choice(["haiku", "sonnet", "opus", "gpt-5.4"]),
    help="Agent model to use",
)
@click.option("--scenario", type=click.Choice(["implication-share", "cross-package"]), default=None, help="Bug-fix scenario")
@click.option("--reasoning", type=click.Choice(["default", "high", "low"]), default="default", help="Reasoning effort level")
@click.option("--dest", "dest_dir", type=click.Path(path_type=Path), default=None, help="Override dest dir for bug-fix clones")
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
@click.option("--source", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path), help="Source repo to clone")
@click.option("--dest", required=True, type=click.Path(path_type=Path), help="Destination directory for 4 condition clones")
def eval_setup_repos(source: Path, dest: Path) -> None:
    """Create 4 isolated repo clones for benchmark conditions."""
    repos = create_condition_repos(source, dest)
    for cond, path in repos.items():
        click.echo(f"{cond}: {path}")


@eval.command("explain-repo")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", default=DEFAULT_TASK, show_default=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option("--control-cmd", help="Command for the control run (no context, no tools)")
@click.option("--explore-cmd", help="Command for the explore run (tools in prompt, no context)")
@click.option("--leverage-cmd", help="Command for the leverage run (pre-computed context + tools)")
@click.option("--baseline-cmd", hidden=True, help="[Deprecated] Alias for --control-cmd")
@click.option("--aethyme-cmd", hidden=True, help="[Deprecated] Alias for --leverage-cmd")
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
    # Map legacy flags
    if control_cmd is None and baseline_cmd is not None:
        control_cmd = baseline_cmd
    if leverage_cmd is None and aethyme_cmd is not None:
        leverage_cmd = aethyme_cmd

    try:
        control_runner = command_runner(control_cmd, working_directory=repo_path) if control_cmd else None
        explore_runner = command_runner(explore_cmd, working_directory=repo_path) if explore_cmd else None
        leverage_runner = command_runner(leverage_cmd, working_directory=repo_path) if leverage_cmd else None
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
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", default=DEFAULT_NAVIGATION_TASK, show_default=True, help="Task description")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
@click.option("--control-cmd", help="Command for the control run (no context, no tools)")
@click.option("--explore-cmd", help="Command for the explore run (tools in prompt, no context)")
@click.option("--leverage-cmd", help="Command for the leverage run (pre-computed context + tools)")
@click.option("--baseline-cmd", hidden=True, help="[Deprecated] Alias for --control-cmd")
@click.option("--aethyme-cmd", hidden=True, help="[Deprecated] Alias for --leverage-cmd")
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
    # Map legacy flags
    if control_cmd is None and baseline_cmd is not None:
        control_cmd = baseline_cmd
    if leverage_cmd is None and aethyme_cmd is not None:
        leverage_cmd = aethyme_cmd

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
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
def eval_bug_fix_setup(repo_path: Path) -> None:
    """Plant the bug and create the test file in a Playground repo."""
    result = setup_bug_fix(repo_path)
    click.echo(json.dumps(result, indent=2))


@eval_bug_fix_group.command("verify")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
def eval_bug_fix_verify(repo_path: Path) -> None:
    """Verify the bug is planted: planted test should fail, existing tests should pass."""
    result = verify_bug_fix_setup(repo_path)
    click.echo(json.dumps(result, indent=2, default=str))


@eval_bug_fix_group.command("reset")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
def eval_bug_fix_reset(repo_path: Path) -> None:
    """Restore rbac-canonical.ts to its committed (buggy) state."""
    reset_bug_fix(repo_path)
    click.echo("Reset complete.")


@eval_bug_fix_group.command("generate")
@click.option("--repo", "repo_path", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path))
@click.option("--task", "task_text", default=DEFAULT_BUG_FIX_TASK, show_default=True, help="Task description")
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
    click.echo(f"\nTest commands:")
    for name, cmd in result["test_commands"].items():
        click.echo(f"  {name}: {cmd}")
    click.echo(f"\nReference:")
    click.echo(json.dumps(result["reference"], indent=2))


@eval_bug_fix_group.command("prepare")
@click.option("--source", required=True, type=click.Path(exists=True, file_okay=False, path_type=Path), help="Source repo to clone")
@click.option("--dest", required=True, type=click.Path(path_type=Path), help="Destination directory for 4 condition clones")
@click.option("--task", "task_text", default=None, help="Task description (auto-set per scenario)")
@click.option("--scenario", type=click.Choice(["implication-share", "cross-package"]), default="implication-share", show_default=True, help="Bug scenario to plant")
@click.option("--json-output", "json_output", is_flag=True, help="Emit raw JSON")
def eval_bug_fix_prepare(source: Path, dest: Path, task_text: str | None, scenario: str, json_output: bool) -> None:
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
        click.echo(json.dumps(result, indent=2))
        return

    click.echo(f"Scenario: {scenario}")
    click.echo("Repos created:")
    for cond, path in result["repos"].items():
        click.echo(f"  {cond}: {path}")
    click.echo(f"\nArtifacts written:")
    for name, path in result["artifacts"].items():
        click.echo(f"  {name}: {path}")
    click.echo(f"\nTest commands:")
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
            f"{i:2}. {result['symbol']} "
            f"({result['kind']}) "
            f"[score: {score:.3f}]"
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
            "SELECT id FROM aethyme.tenants WHERE slug = %s LIMIT 1",
            (org,)
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
            repo_path=repo_path,
            repository_id=repo_id,
            tenant_id=tenant_id
        )

        with click.progressbar(
            length=100,
            label="Scanning repository",
            show_eta=True
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
    click.echo(f"Findings: {result.report.total_findings} total "
               f"({result.report.blocker_count} blockers, "
               f"{result.report.warning_count} warnings, "
               f"{result.report.info_count} info)")
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
            json_path = output_base if output_base.suffix == ".json" else output_base.with_suffix(".json")
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
            md_path = output_base if output_base.suffix == ".md" else output_base.with_suffix(".md")
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
    click.echo(f"Mode: {'DRY RUN' if dry_run else 'APPLY' if do_apply else 'PR' if pr else 'DRY RUN'}")
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


def main() -> None:
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()
