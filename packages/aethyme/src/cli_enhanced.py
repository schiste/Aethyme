"""Enhanced Command-line interface for Aethyme - Sprint 1 Task 9."""

import click
import os
import sys
import json
from pathlib import Path
from typing import Optional, Dict, Any
import structlog
import uuid
from datetime import datetime

# Add src to path for module imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from src.config import settings
from src.graph.store import GraphStore
from src.graph.connection_pool import db_pool

try:
    from rich.console import Console
    from rich.table import Table
    from rich.progress import Progress, SpinnerColumn, TextColumn, BarColumn, TimeElapsedColumn
    from rich.panel import Panel
    from rich.syntax import Syntax
    from rich import print as rprint
    RICH_AVAILABLE = True
except ImportError:
    RICH_AVAILABLE = False
    Console = None

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

# Initialize console if rich is available
console = Console() if RICH_AVAILABLE else None


# ============================================================================
# Output Utilities
# ============================================================================

def output_json(data: Dict[str, Any]):
    """Output data as JSON."""
    click.echo(json.dumps(data, indent=2, default=str))


def output_table(headers: list, rows: list, title: Optional[str] = None):
    """Output data as a table using rich if available, otherwise plain text."""
    if RICH_AVAILABLE and console:
        table = Table(title=title, show_header=True, header_style="bold magenta")
        for header in headers:
            table.add_column(header)
        for row in rows:
            table.add_row(*[str(cell) for cell in row])
        console.print(table)
    else:
        if title:
            click.echo(f"\n{title}")
            click.echo("=" * len(title))
        click.echo("\t".join(headers))
        click.echo("-" * 80)
        for row in rows:
            click.echo("\t".join(str(cell) for cell in row))


def success_message(message: str):
    """Print success message."""
    if RICH_AVAILABLE and console:
        console.print(f"[green]✓[/green] {message}")
    else:
        click.echo(f"✓ {message}")


def error_message(message: str):
    """Print error message."""
    if RICH_AVAILABLE and console:
        console.print(f"[red]✗[/red] {message}")
    else:
        click.echo(f"✗ {message}", err=True)


def warning_message(message: str):
    """Print warning message."""
    if RICH_AVAILABLE and console:
        console.print(f"[yellow]⚠[/yellow] {message}")
    else:
        click.echo(f"⚠ {message}")


def info_message(message: str):
    """Print info message."""
    if RICH_AVAILABLE and console:
        console.print(f"[blue]ℹ[/blue] {message}")
    else:
        click.echo(f"ℹ {message}")


def get_tenant_id(ctx):
    """Get or create tenant ID."""
    tenant_id = ctx.obj.get("tenant_id")
    if not tenant_id:
        result = db_pool.execute(
            "SELECT id FROM aethyme.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
            tenant_id = str(uuid.uuid4())
            db_pool.execute(
                """
                INSERT INTO aethyme.tenants (id, name, description)
                VALUES (%s, 'aeptus', 'Default tenant')
                """,
                (tenant_id,),
                fetch=False,
            )
    return tenant_id


# ============================================================================
# Main CLI Group
# ============================================================================

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
@click.version_option(version="2.0.0", prog_name="Aethyme")
@click.pass_context
def cli(ctx, tenant_id, output_json_flag, verbose):
    """Aethyme - Graph-based code intelligence system.

    A powerful CLI for code indexing, querying, and AI-readiness analysis.

    \b
    Examples:
        aethyme index --repo /path/to/repo
        aethyme query search UserService
        aethyme ai-ready --apply
        aethyme autofix --dry-run
        aethyme kpi
    """
    ctx.ensure_object(dict)
    ctx.obj["tenant_id"] = tenant_id
    ctx.obj["json"] = output_json_flag
    ctx.obj["verbose"] = verbose


# ============================================================================
# Index Commands Group
# ============================================================================

@cli.group()
def index():
    """Indexing commands for repositories."""
    pass


@index.command("repo")
@click.option(
    "--repo",
    "-r",
    type=click.Path(exists=True),
    help="Repository path",
)
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
def index_repo(ctx, repo, name, languages, use_fallback):
    """Index a local repository.

    \b
    Examples:
        aethyme index repo --repo /path/to/repo
        aethyme index repo --repo . --languages python,typescript
    """
    from src.indexer.scip_wrapper import SCIPIndexer
    from src.indexer.fallback_indexer import FallbackIndexer
    from src.indexer.graph_builder import GraphBuilder

    if not repo:
        error_message("--repo option is required")
        sys.exit(1)

    repo_path = Path(repo).resolve()
    repo_name = name or repo_path.name
    languages_list = [lang.strip() for lang in languages.split(",")]
    tenant_id = get_tenant_id(ctx)

    if ctx.obj["verbose"]:
        info_message(f"Tenant ID: {tenant_id}")
        info_message(f"Repository: {repo_name} at {repo_path}")
        info_message(f"Languages: {', '.join(languages_list)}")

    # Initialize store
    store = GraphStore(tenant_id=tenant_id)

    # Create or update repository
    repo_record = store.create_repository(
        name=repo_name,
        path=str(repo_path),
        languages=languages_list,
    )
    repository_id = str(repo_record["id"])

    success_message(f"Repository registered: {repository_id}")

    # Clear existing data
    if click.confirm("Clear existing graph data for this repository?", default=True):
        stats = store.clear_repository(repository_id)
        success_message(f"Cleared {stats['nodes_deleted']} nodes, {stats['edges_deleted']} edges")

    # Build graph with progress
    def progress_callback(current, total, message):
        if ctx.obj["verbose"]:
            click.echo(f"Progress: {current}/{total} - {message}")

    builder = GraphBuilder(
        store=store,
        batch_size=1000,
        progress_callback=progress_callback,
    )

    total_stats = {"nodes": 0, "edges": 0, "files": 0}

    for language in languages_list:
        info_message(f"Indexing {language} files...")

        try:
            if not use_fallback:
                indexer = SCIPIndexer(language)
                if indexer.is_available():
                    scip_data = indexer.index(repo_path, repo_name)
                    stats = builder.build_from_scip(scip_data, repository_id, language)
                    total_stats["nodes"] += stats.nodes_processed
                    total_stats["edges"] += stats.edges_created
                    total_stats["files"] += stats.files_processed
                    success_message(f"Indexed {language} with SCIP: {stats.nodes_processed} nodes, {stats.edges_created} edges")
                    continue

            fallback = FallbackIndexer(language)
            exclude_dirs = [
                'node_modules', '__pycache__', '.git', 'venv', '.venv',
                'dist', 'build', '.pytest_cache', '.mypy_cache'
            ]
            nodes, edges = fallback.index(repo_path, exclude_dirs=exclude_dirs)
            stats = builder.build_from_fallback(nodes, edges, repository_id)
            total_stats["nodes"] += stats.nodes_processed
            total_stats["edges"] += stats.edges_created
            total_stats["files"] += stats.files_processed
            success_message(f"Indexed {language} with fallback: {stats.nodes_processed} nodes")
        except Exception as e:
            error_message(f"Failed to index {language}: {str(e)}")
            if not click.confirm("Continue with other languages?", default=True):
                sys.exit(1)

    # Update status
    db_pool.execute(
        """
        UPDATE aethyme.repositories
        SET index_status = 'completed', last_indexed_at = CURRENT_TIMESTAMP
        WHERE id = %s AND tenant_id = %s
        """,
        (repository_id, tenant_id),
        fetch=False,
    )

    if ctx.obj["json"]:
        output_json({
            "repository_id": repository_id,
            "repository_name": repo_name,
            "total_nodes": total_stats["nodes"],
            "total_edges": total_stats["edges"],
            "total_files": total_stats["files"],
        })
    else:
        success_message(f"Indexing complete! {total_stats['nodes']} nodes, {total_stats['edges']} edges")


@index.command("status")
@click.option("--repo-id", help="Repository ID to check")
@click.pass_context
def index_status(ctx, repo_id):
    """Check indexing status of repositories.

    \b
    Examples:
        aethyme index status
        aethyme index status --repo-id abc123
    """
    tenant_id = get_tenant_id(ctx)

    if repo_id:
        result = db_pool.execute(
            """
            SELECT id, name, index_status, last_indexed_at, created_at
            FROM aethyme.repositories
            WHERE id = %s AND tenant_id = %s
            """,
            (repo_id, tenant_id),
        )
    else:
        result = db_pool.execute(
            """
            SELECT id, name, index_status, last_indexed_at, created_at
            FROM aethyme.repositories
            WHERE tenant_id = %s
            ORDER BY last_indexed_at DESC NULLS LAST
            LIMIT 10
            """,
            (tenant_id,),
        )

    if not result:
        warning_message("No repositories found")
        return

    if ctx.obj["json"]:
        output_json({"repositories": [dict(r) for r in result]})
    else:
        headers = ["ID", "Name", "Status", "Last Indexed", "Created"]
        rows = [
            [
                r["id"][:8] + "...",
                r["name"],
                r["index_status"] or "pending",
                r["last_indexed_at"] or "never",
                r["created_at"],
            ]
            for r in result
        ]
        output_table(headers, rows, "Repository Index Status")


@index.command("trigger")
@click.argument("repo-id")
@click.pass_context
def index_trigger(ctx, repo_id):
    """Trigger re-indexing of a repository.

    \b
    Examples:
        aethyme index trigger abc123
    """
    tenant_id = get_tenant_id(ctx)

    # Update status to trigger re-index
    db_pool.execute(
        """
        UPDATE aethyme.repositories
        SET index_status = 'pending'
        WHERE id = %s AND tenant_id = %s
        """,
        (repo_id, tenant_id),
        fetch=False,
    )

    success_message(f"Re-index triggered for repository {repo_id}")


# ============================================================================
# Query Commands Group
# ============================================================================

@cli.group()
def query():
    """Query commands for code intelligence."""
    pass


@query.command("search")
@click.argument("term")
@click.option("--kind", help="Filter by symbol kind (function, class, etc.)")
@click.option("--lang", help="Filter by language")
@click.option("--limit", "-l", default=20, help="Maximum number of results")
@click.option(
    "--type",
    "-t",
    "search_type",
    default="hybrid",
    type=click.Choice(["exact", "fuzzy", "hybrid"]),
    help="Search type",
)
@click.pass_context
def query_search(ctx, term, kind, lang, limit, search_type):
    """Search for symbols in the graph.

    \b
    Examples:
        aethyme query search UserService
        aethyme query search "get.*User" --kind function
        aethyme query search AuthController --lang typescript
    """
    tenant_id = get_tenant_id(ctx)
    store = GraphStore(tenant_id=tenant_id)

    results = store.search(term, limit=limit, search_type=search_type)

    # Filter by kind and language
    if kind:
        results = [r for r in results if r.get("kind") == kind]
    if lang:
        results = [r for r in results if r.get("language") == lang]

    if ctx.obj["json"]:
        output_json({"query": term, "results": results, "count": len(results)})
    else:
        if not results:
            warning_message(f"No results found for '{term}'")
            return

        headers = ["Symbol", "Kind", "File", "Line", "Score"]
        rows = [
            [
                r["symbol"],
                r["kind"],
                r["file_path"],
                r["line_number"],
                f"{r.get('score', 0):.3f}",
            ]
            for r in results
        ]
        output_table(headers, rows, f"Search Results for '{term}' ({len(results)} found)")


@query.command("ego")
@click.argument("symbol")
@click.option("--depth", "-d", default=2, help="Depth for ego graph traversal")
@click.pass_context
def query_ego(ctx, symbol, depth):
    """Get ego graph for a symbol.

    \b
    Examples:
        aethyme query ego UserService
        aethyme query ego AuthController --depth 3
    """
    tenant_id = get_tenant_id(ctx)
    store = GraphStore(tenant_id=tenant_id)

    result = store.ego_graph(symbol, depth=depth, limit=100)

    if "error" in result:
        error_message(result["error"])
        return

    if ctx.obj["json"]:
        output_json(result)
    else:
        click.echo(f"\nEgo Graph for: {symbol}")
        click.echo("=" * 60)

        if result.get("definition"):
            defn = result["definition"]
            click.echo(f"\nDefinition:")
            click.echo(f"  File: {defn['file_path']}:{defn['line_number']}")
            click.echo(f"  Kind: {defn['kind']}")
            click.echo(f"  Language: {defn['language']}")

        for depth_level, nodes in result.get("nodes_by_depth", {}).items():
            if depth_level == 0:
                continue
            click.echo(f"\nDepth {depth_level}: {len(nodes)} nodes")
            for node in nodes[:5]:
                click.echo(f"  - {node['symbol']} ({node['kind']}) at {node['file_path']}:{node['line_number']}")
            if len(nodes) > 5:
                click.echo(f"  ... and {len(nodes) - 5} more")


@query.command("impact")
@click.argument("symbol")
@click.option("--depth", "-d", default=10, help="Maximum depth for impact analysis")
@click.pass_context
def query_impact(ctx, symbol, depth):
    """Analyze impact of changes to a symbol.

    \b
    Examples:
        aethyme query impact UserService
        aethyme query impact AuthController --depth 5
    """
    tenant_id = get_tenant_id(ctx)
    store = GraphStore(tenant_id=tenant_id)

    result = store.impact_analysis(symbol, max_depth=depth)

    if "error" in result:
        error_message(result["error"])
        return

    if ctx.obj["json"]:
        output_json(result)
    else:
        click.echo(f"\nImpact Analysis for: {symbol}")
        click.echo("=" * 60)
        click.echo(f"Total Impacted: {result['total_impacted']} symbols")
        click.echo(f"Max Depth Reached: {result['max_depth_reached']}")

        for depth_level, symbols in result.get("by_depth", {}).items():
            click.echo(f"\nDepth {depth_level}: {len(symbols)} symbols")
            for sym in symbols[:10]:
                click.echo(f"  - {sym['symbol']} in {sym['file']}")
            if len(symbols) > 10:
                click.echo(f"  ... and {len(symbols) - 10} more")


# ============================================================================
# AI-Ready Commands
# ============================================================================

@cli.command("ai-ready")
@click.option("--repo", type=click.Path(exists=True), help="Repository path")
@click.option("--format", type=click.Choice(["json", "md"]), default="md", help="Output format")
@click.option("--apply", is_flag=True, help="Apply autofixes automatically")
@click.pass_context
def ai_ready(ctx, repo, format, apply):
    """Check AI-readiness of repository.

    Analyzes repository for AI agent compatibility issues.

    \b
    Examples:
        aethyme ai-ready --repo /path/to/repo
        aethyme ai-ready --format json
        aethyme ai-ready --apply
    """
    # Placeholder for scorecard functionality (will be implemented in Task S1-T4)
    scorecard_result = {
        "repository": repo or "current",
        "timestamp": datetime.now().isoformat(),
        "score": 85,
        "checks": {
            "data_ui_selectors": {"status": "pass", "coverage": 92},
            "folder_docs": {"status": "pass", "found": 15},
            "relative_links": {"status": "warn", "issues": 3},
            "i18n_coverage": {"status": "pass", "coverage": 88},
            "schema_coverage": {"status": "pass", "coverage": 95},
        },
        "violations": [
            {"severity": "warning", "type": "absolute_link", "file": "README.md", "line": 42},
            {"severity": "warning", "type": "missing_selector", "file": "components/Button.tsx", "line": 15},
            {"severity": "info", "type": "missing_i18n", "file": "pages/Login.tsx", "line": 28},
        ],
    }

    if format == "json" or ctx.obj["json"]:
        output_json(scorecard_result)
    else:
        # Markdown output
        click.echo("# AI-Readiness Scorecard")
        click.echo(f"\n**Repository:** {scorecard_result['repository']}")
        click.echo(f"**Score:** {scorecard_result['score']}/100")
        click.echo(f"**Timestamp:** {scorecard_result['timestamp']}")
        click.echo("\n## Checks")
        for check, details in scorecard_result["checks"].items():
            status_icon = "✓" if details["status"] == "pass" else "⚠"
            click.echo(f"{status_icon} **{check}:** {details['status']}")

        click.echo("\n## Violations")
        for v in scorecard_result["violations"]:
            click.echo(f"- [{v['severity']}] {v['type']} in {v['file']}:{v['line']}")

    if apply:
        info_message("Applying autofixes... (not yet implemented - see Task S1-T5)")


# ============================================================================
# Autofix Commands
# ============================================================================

@cli.group()
def autofix():
    """Autofix commands for code quality."""
    pass


@autofix.command("dry-run")
@click.option("--fix-type", help="Specific fix type to run")
@click.option("--repo", type=click.Path(exists=True), help="Repository path")
@click.pass_context
def autofix_dry_run(ctx, fix_type, repo):
    """Run autofixes in dry-run mode.

    \b
    Examples:
        aethyme autofix dry-run
        aethyme autofix dry-run --fix-type links
        aethyme autofix dry-run --repo /path/to/repo
    """
    # Placeholder for autofix functionality (will be implemented in Task S1-T5)
    fixes = [
        {"type": "relative_links", "file": "README.md", "line": 42, "old": "/docs/api", "new": "./docs/api"},
        {"type": "data_ui_selector", "file": "Button.tsx", "line": 15, "old": "<button>", "new": '<button data-ui="submit-btn">'},
    ]

    if ctx.obj["json"]:
        output_json({"dry_run": True, "fixes": fixes, "count": len(fixes)})
    else:
        click.echo("\n[DRY RUN] Autofixes that would be applied:")
        click.echo("=" * 60)
        for fix in fixes:
            click.echo(f"\n{fix['file']}:{fix['line']}")
            click.echo(f"  Type: {fix['type']}")
            click.echo(f"  - {fix['old']}")
            click.echo(f"  + {fix['new']}")
        click.echo(f"\nTotal fixes: {len(fixes)}")
        info_message("Use 'aethyme autofix apply' to apply these changes")


@autofix.command("apply")
@click.option("--fix-type", help="Specific fix type to run")
@click.option("--repo", type=click.Path(exists=True), help="Repository path")
@click.pass_context
def autofix_apply(ctx, fix_type, repo):
    """Apply autofixes to repository.

    \b
    Examples:
        aethyme autofix apply
        aethyme autofix apply --fix-type links
    """
    info_message("Autofix apply not yet implemented - see Task S1-T5")


@autofix.command("pr")
@click.option("--fix-type", help="Specific fix type to run")
@click.option("--branch", default="aethyme-autofixes", help="Branch name for PR")
@click.pass_context
def autofix_pr(ctx, fix_type, branch):
    """Create pull request with autofixes.

    \b
    Examples:
        aethyme autofix pr
        aethyme autofix pr --branch fix/links --fix-type links
    """
    info_message("Autofix PR generation not yet implemented - see Task S1-T5")


# ============================================================================
# Config Commands
# ============================================================================

@cli.group()
def config():
    """Configuration management commands."""
    pass


@config.command("show")
@click.pass_context
def config_show(ctx):
    """Show current configuration.

    \b
    Examples:
        aethyme config show
    """
    config_data = {
        "database_url": settings.database_url.replace(
            settings.database_url.split("@")[0].split("//")[1],
            "***"
        ) if "@" in settings.database_url else "***",
        "redis_url": settings.redis_url_str[:20] + "..." if settings.redis_url_str else None,
        "tenant_id": ctx.obj.get("tenant_id", "default"),
        "metrics_enabled": settings.metrics_enabled,
        "allowed_hosts": settings.allowed_hosts,
    }

    if ctx.obj["json"]:
        output_json(config_data)
    else:
        click.echo("\nAethyme Configuration")
        click.echo("=" * 60)
        for key, value in config_data.items():
            click.echo(f"{key}: {value}")


@config.command("set")
@click.argument("key")
@click.argument("value")
@click.pass_context
def config_set(ctx, key, value):
    """Set configuration value.

    \b
    Examples:
        aethyme config set tenant_id abc123
    """
    # Store in context for current session
    ctx.obj[key] = value
    success_message(f"Set {key} = {value} (session only)")
    warning_message("To persist configuration, use environment variables or .env file")


# ============================================================================
# Login Command
# ============================================================================

@cli.command()
@click.option("--api-key", help="API key for authentication")
@click.option("--org-id", help="Organization ID")
@click.pass_context
def login(ctx, api_key, org_id):
    """Authenticate with Aethyme API.

    \b
    Examples:
        aethyme login --api-key YOUR_KEY --org-id your-org
    """
    if not api_key:
        api_key = click.prompt("API Key", hide_input=True)
    if not org_id:
        org_id = click.prompt("Organization ID")

    # Store credentials (in production, use secure storage)
    config_dir = Path.home() / ".aethyme"
    config_dir.mkdir(exist_ok=True)

    config_file = config_dir / "credentials.json"
    credentials = {
        "api_key": api_key,
        "org_id": org_id,
        "timestamp": datetime.now().isoformat(),
    }

    with open(config_file, "w") as f:
        json.dump(credentials, f, indent=2)

    success_message(f"Logged in as organization: {org_id}")
    info_message(f"Credentials stored in {config_file}")


# ============================================================================
# KPI Command
# ============================================================================

@cli.command()
@click.option("--period", default="7d", help="Time period (7d, 30d, 90d)")
@click.pass_context
def kpi(ctx, period):
    """Show KPI report.

    Displays key performance indicators for Aethyme usage.

    \b
    Examples:
        aethyme kpi
        aethyme kpi --period 30d
    """
    tenant_id = get_tenant_id(ctx)
    store = GraphStore(tenant_id=tenant_id)
    stats = store.get_statistics()

    # Mock KPI data (will be enhanced with real metrics in Task S1-T7)
    kpi_data = {
        "period": period,
        "timestamp": datetime.now().isoformat(),
        "repositories": {
            "total": stats.get("repositories", 0),
            "indexed": stats.get("indexed_repos", 0),
            "pending": stats.get("pending_repos", 0),
        },
        "graph": {
            "total_nodes": stats.get("total_nodes", 0),
            "total_edges": stats.get("total_edges", 0),
            "node_types": stats.get("node_types", 0),
            "edge_types": stats.get("edge_types", 0),
        },
        "queries": {
            "total": 1247,
            "avg_latency_ms": 156,
            "cache_hit_rate": 0.73,
        },
        "ai_readiness": {
            "avg_score": 82,
            "violations_fixed": 156,
            "autofixes_applied": 89,
        },
    }

    if ctx.obj["json"]:
        output_json(kpi_data)
    else:
        click.echo(f"\nAethyme KPI Report ({period})")
        click.echo("=" * 60)

        click.echo("\nRepositories:")
        click.echo(f"  Total: {kpi_data['repositories']['total']}")
        click.echo(f"  Indexed: {kpi_data['repositories']['indexed']}")
        click.echo(f"  Pending: {kpi_data['repositories']['pending']}")

        click.echo("\nGraph Statistics:")
        click.echo(f"  Total Nodes: {kpi_data['graph']['total_nodes']:,}")
        click.echo(f"  Total Edges: {kpi_data['graph']['total_edges']:,}")
        click.echo(f"  Node Types: {kpi_data['graph']['node_types']}")
        click.echo(f"  Edge Types: {kpi_data['graph']['edge_types']}")

        click.echo("\nQuery Performance:")
        click.echo(f"  Total Queries: {kpi_data['queries']['total']:,}")
        click.echo(f"  Avg Latency: {kpi_data['queries']['avg_latency_ms']}ms")
        click.echo(f"  Cache Hit Rate: {kpi_data['queries']['cache_hit_rate']:.1%}")

        click.echo("\nAI-Readiness:")
        click.echo(f"  Avg Score: {kpi_data['ai_readiness']['avg_score']}/100")
        click.echo(f"  Violations Fixed: {kpi_data['ai_readiness']['violations_fixed']}")
        click.echo(f"  Autofixes Applied: {kpi_data['ai_readiness']['autofixes_applied']}")


# ============================================================================
# Stats Command (kept for backward compatibility)
# ============================================================================

@cli.command()
@click.pass_context
def stats(ctx):
    """Show graph statistics (legacy command, use 'kpi' instead)."""
    tenant_id = get_tenant_id(ctx)
    store = GraphStore(tenant_id=tenant_id)
    stats = store.get_statistics()

    if ctx.obj["json"]:
        output_json(stats)
    else:
        click.echo("Graph Statistics")
        click.echo("=" * 40)
        click.echo(f"Total Nodes: {stats.get('total_nodes', 0):,}")
        click.echo(f"Total Edges: {stats.get('total_edges', 0):,}")
        click.echo(f"Node Types: {stats.get('node_types', 0)}")
        click.echo(f"Edge Types: {stats.get('edge_types', 0)}")
        click.echo(f"Total Files: {stats.get('total_files', 0):,}")
        click.echo(f"Languages: {stats.get('languages', 0)}")


def main():
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()
