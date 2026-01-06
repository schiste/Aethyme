"""Command-line interface for RepoGraph."""

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
from src.indexer.scip_wrapper import SCIPIndexer
from src.indexer.fallback_indexer import FallbackIndexer
from src.indexer.graph_builder import GraphBuilder

try:
    from rich.console import Console
    from rich.table import Table
    from rich.progress import Progress, SpinnerColumn, TextColumn, BarColumn
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


@click.group()
@click.option(
    "--tenant-id",
    envvar="REPOGRAPH_TENANT_ID",
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
def cli(ctx, tenant_id, output_json_flag, verbose):
    """RepoGraph - Graph-based code intelligence system.

    A powerful CLI for code indexing, querying, and AI-readiness analysis.

    Examples:
        repograph index --repo /path/to/repo
        repograph query search UserService
        repograph ai-ready --apply
        repograph autofix --dry-run
    """
    ctx.ensure_object(dict)
    ctx.obj["tenant_id"] = tenant_id
    ctx.obj["json"] = output_json_flag
    ctx.obj["verbose"] = verbose


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
def index(ctx, repo_path, name, languages, use_fallback):
    """Index a repository and build the code graph."""
    repo_path = Path(repo_path).resolve()
    repo_name = name or repo_path.name
    languages_list = [lang.strip() for lang in languages.split(",")]

    # Get or create tenant
    tenant_id = ctx.obj.get("tenant_id")
    if not tenant_id:
        # Use default tenant
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
            # Create default tenant
            tenant_id = str(uuid.uuid4())
            db_pool.execute(
                """
                INSERT INTO repograph.tenants (id, name, description)
                VALUES (%s, 'aeptus', 'Default tenant')
                """,
                (tenant_id,),
                fetch=False,
            )

    logger.info(
        "Starting repository indexing",
        repo_path=str(repo_path),
        repo_name=repo_name,
        languages=languages_list,
        tenant_id=tenant_id,
    )

    # Initialize store
    store = GraphStore(tenant_id=tenant_id)

    # Create or update repository
    repo = store.create_repository(
        name=repo_name,
        path=str(repo_path),
        languages=languages_list,
    )
    repository_id = str(repo["id"])

    logger.info("Repository registered", repository_id=repository_id)

    # Clear existing data for re-indexing
    if click.confirm("Clear existing graph data for this repository?", default=True):
        stats = store.clear_repository(repository_id)
        logger.info(
            "Cleared existing data",
            nodes_deleted=stats["nodes_deleted"],
            edges_deleted=stats["edges_deleted"],
        )

    # Initialize graph builder
    def progress_callback(current, total, message):
        click.echo(f"Progress: {current}/{total} - {message}")

    builder = GraphBuilder(
        store=store,
        batch_size=1000,
        progress_callback=progress_callback,
    )

    # Index each language
    total_stats = {
        "nodes": 0,
        "edges": 0,
        "files": 0,
    }

    for language in languages_list:
        logger.info(f"Indexing {language} files...")

        try:
            # Try SCIP first unless fallback is forced
            if not use_fallback:
                indexer = SCIPIndexer(language)
                if indexer.is_available():
                    logger.info(f"Using SCIP indexer for {language}")
                    scip_data = indexer.index(repo_path, repo_name)
                    stats = builder.build_from_scip(scip_data, repository_id, language)

                    total_stats["nodes"] += stats.nodes_processed
                    total_stats["edges"] += stats.edges_created
                    total_stats["files"] += stats.files_processed

                    logger.info(
                        f"Indexed {language} with SCIP",
                        nodes=stats.nodes_processed,
                        edges=stats.edges_created,
                        files=stats.files_processed,
                    )
                    continue

            # Fall back to regex-based indexer
            logger.info(f"Using fallback indexer for {language}")
            fallback = FallbackIndexer(language)
            # Default exclusions for cleaner indexing
            exclude_dirs = [
                'node_modules', '__pycache__', '.git', 'venv', '.venv',
                'dist', 'build', '.pytest_cache', '.mypy_cache',
                'site-packages', 'proc', 'sys', 'dev', 'run', 'tmp'
            ]
            nodes, edges = fallback.index(repo_path, exclude_dirs=exclude_dirs)
            stats = builder.build_from_fallback(nodes, edges, repository_id)

            total_stats["nodes"] += stats.nodes_processed
            total_stats["edges"] += stats.edges_created
            total_stats["files"] += stats.files_processed

            logger.info(
                f"Indexed {language} with fallback",
                nodes=stats.nodes_processed,
                edges=stats.edges_created,
            )

        except Exception as e:
            logger.error(f"Failed to index {language}", error=str(e))
            if not click.confirm(f"Continue with other languages?", default=True):
                sys.exit(1)

    # Update repository status
    db_pool.execute(
        """
        UPDATE repograph.repositories
        SET index_status = 'completed',
            last_indexed_at = CURRENT_TIMESTAMP
        WHERE id = %s AND tenant_id = %s
        """,
        (repository_id, tenant_id),
        fetch=False,
    )

    # Get final statistics
    graph_stats = store.get_statistics()

    # Print summary
    click.echo("\n" + "=" * 60)
    click.echo("Indexing Complete!")
    click.echo("=" * 60)
    click.echo(f"Repository: {repo_name}")
    click.echo(f"Path: {repo_path}")
    click.echo(f"Languages: {', '.join(languages_list)}")
    click.echo("-" * 60)
    click.echo(f"Total Nodes: {graph_stats.get('total_nodes', 0):,}")
    click.echo(f"Total Edges: {graph_stats.get('total_edges', 0):,}")
    click.echo(f"Total Files: {graph_stats.get('total_files', 0):,}")
    click.echo("-" * 60)

    # Analyze graph
    if click.confirm("Analyze graph structure?", default=True):
        analysis = builder.analyze_graph()
        click.echo("\nGraph Analysis:")
        click.echo(f"  Orphan nodes: {analysis['orphan_nodes']}")
        click.echo(f"  Node types: {dict(analysis['node_types'])}")
        click.echo(f"  Edge types: {dict(analysis['edge_types'])}")

        if analysis["top_symbols"]:
            click.echo("\nTop 10 Most Referenced Symbols:")
            for i, symbol_info in enumerate(analysis["top_symbols"], 1):
                click.echo(
                    f"  {i}. {symbol_info['symbol']} ({symbol_info['references']} references)"
                )


@cli.command()
@click.pass_context
def stats(ctx):
    """Show graph statistics."""
    tenant_id = ctx.obj.get("tenant_id")

    if not tenant_id:
        # Get default tenant
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
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


@cli.command()
@click.argument("symbol")
@click.option("--depth", "-d", default=2, help="Depth for ego graph traversal")
@click.pass_context
def ego(ctx, symbol, depth):
    """Get ego graph for a symbol."""
    tenant_id = ctx.obj.get("tenant_id")

    if not tenant_id:
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
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
        click.echo(f"\nDefinition:")
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
def impact(ctx, symbol, max_depth):
    """Analyze impact of changes to a symbol."""
    tenant_id = ctx.obj.get("tenant_id")

    if not tenant_id:
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
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
def search(ctx, query, limit, type):
    """Search for symbols in the graph."""
    tenant_id = ctx.obj.get("tenant_id")

    if not tenant_id:
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        )
        if result:
            tenant_id = str(result[0]["id"])
        else:
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
def ai_ready(ctx, repo, org, repo_id, format, output, detectors):
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
    tenant_id = ctx.obj.get("tenant_id")
    if org and not tenant_id:
        # Look up tenant by org name
        result = db_pool.execute(
            "SELECT id FROM repograph.tenants WHERE name = %s LIMIT 1",
            (org,)
        )
        if result:
            tenant_id = str(result[0]["id"])

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

    except Exception as e:
        click.echo(f"Error during scan: {e}", err=True)
        logger.error("Scorecard scan failed", error=str(e), exc_info=True)
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
            json_path = Path(output or "scorecard-report.json")
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
            md_path = Path(output or "scorecard-report.md")
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
def autofix(ctx, repo_path, dry_run, do_apply, pr, fix_type, skip_approval):
    """Apply automated fixes to codebase issues."""
    from pathlib import Path
    from src.autofixers.safety import SafetyEngine
    from src.autofixers.patch import PatchGenerator
    from src.autofixers.github import GitHubIntegration
    from src.autofixers.fixers import (
        DocsRegenerator,
        LinkFixer,
        SelectorInserter,
        I18nScaffolder,
        FormatFixer,
    )

    repo_path = Path(repo_path).resolve()

    click.echo(f"\nRepoGraph Autofixer")
    click.echo("=" * 60)
    click.echo(f"Repository: {repo_path}")
    click.echo(f"Fix type: {fix_type}")
    click.echo(f"Mode: {'DRY RUN' if dry_run else 'APPLY' if do_apply else 'PR' if pr else 'DRY RUN'}")
    click.echo("")

    # Initialize components
    safety_engine = SafetyEngine()
    patch_gen = PatchGenerator(repo_path, safety_engine)

    # Collect fixes based on type
    all_fixes = []

    if fix_type in ["all", "docs"]:
        click.echo("Scanning for documentation issues...")
        docs_fixer = DocsRegenerator(repo_path)
        docs_fixes = docs_fixer.create_folder_docs()
        all_fixes.extend(docs_fixes)
        click.echo(f"  Found {len(docs_fixes)} documentation fixes")

    if fix_type in ["all", "links"]:
        click.echo("Scanning for link issues...")
        link_fixer = LinkFixer(repo_path)
        link_fixes = link_fixer.process_directory()
        all_fixes.extend(link_fixes)
        click.echo(f"  Found {len(link_fixes)} link fixes")

    if fix_type in ["all", "selectors"]:
        click.echo("Scanning for missing test selectors...")
        selector_fixer = SelectorInserter(repo_path)
        selector_fixes = selector_fixer.process_directory()
        all_fixes.extend(selector_fixes)
        click.echo(f"  Found {len(selector_fixes)} selector fixes")

    if fix_type in ["all", "i18n"]:
        click.echo("Scanning for hardcoded strings...")
        i18n_fixer = I18nScaffolder(repo_path)
        i18n_fixes = i18n_fixer.process_directory()
        all_fixes.extend(i18n_fixes)
        click.echo(f"  Found {len(i18n_fixes)} i18n fixes")

    if fix_type in ["all", "format"]:
        click.echo("Scanning for formatting issues...")
        format_fixer = FormatFixer(repo_path)
        format_fixes = format_fixer.process_directory()
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

        gh_integration = GitHubIntegration(repo_path)

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


def main():
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()