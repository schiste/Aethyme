"""CLI for the Aethyme indexer."""

import click
import uuid
from pathlib import Path
import structlog
import sys
import os

from .graph_builder import GraphBuilder, GraphBuilderStats
from .scip_wrapper import SCIPIndexer
from .fallback_indexer import FallbackIndexer
from ..config import settings
from ..graph.connection_pool import db_pool
from ..graph.store import GraphStore

logger = structlog.get_logger(__name__)

LANGUAGE_EXTENSIONS = {
    "python": (".py",),
    "typescript": (".ts", ".tsx"),
    "javascript": (".js", ".jsx", ".mjs", ".cjs"),
}


def _has_language_files(path: Path, language: str, exclude_dirs: list[str]) -> bool:
    extensions = LANGUAGE_EXTENSIONS.get(language.lower())
    if not extensions:
        return True

    excluded = set(exclude_dirs)
    for root, dirnames, filenames in os.walk(path):
        dirnames[:] = [d for d in dirnames if d not in excluded]
        for filename in filenames:
            for ext in extensions:
                if filename.endswith(ext):
                    return True
    return False


@click.group()
def cli():
    """Aethyme Indexer CLI."""
    pass


@cli.command()
@click.argument('path', type=click.Path(exists=True))
@click.option('--tenant-id', default=None, help='Tenant ID (defaults to system tenant)')
@click.option('--repo-name', default=None, help='Repository name')
@click.option('--languages', '-l', multiple=True, default=['python', 'typescript'],
              help='Languages to index')
@click.option('--exclude', '-e', multiple=True, default=['node_modules', '__pycache__', '.git'],
              help='Directories to exclude')
@click.option('--batch-size', default=1000, help='Batch size for database operations')
@click.option('--scip-only', is_flag=True, help='Require SCIP indexer and fail if unavailable')
def index(path, tenant_id, repo_name, languages, exclude, batch_size, scip_only):
    """Index a repository into the graph."""

    path_obj = Path(path)

    if not repo_name:
        repo_name = path_obj.name

    # Use default tenant if not specified
    if not tenant_id:
        # Get or create default tenant
        query = """
            INSERT INTO aethyme.tenants (id, name, description)
            VALUES (%s, %s, %s)
            ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
            RETURNING id
        """
        default_tenant_id = str(uuid.uuid4())
        result = db_pool.execute(
            query,
            (default_tenant_id, 'aeptus', 'Default tenant for Aeptus'),
            fetch=True
        )
        if result:
            tenant_id = str(result[0]['id'])
        else:
            # Try to get existing tenant
            query = "SELECT id FROM aethyme.tenants WHERE name = %s"
            result = db_pool.execute(query, ('aeptus',))
            if result:
                tenant_id = str(result[0]['id'])
            else:
                click.echo("Failed to create or find tenant", err=True)
                sys.exit(1)

    click.echo(f"Indexing repository: {repo_name}")
    click.echo(f"Path: {path}")
    click.echo(f"Tenant ID: {tenant_id}")
    click.echo(f"Languages: {', '.join(languages)}")

    try:
        # Create GraphStore
        store = GraphStore(tenant_id=tenant_id)

        # Create GraphBuilder
        builder = GraphBuilder(
            store=store,
            batch_size=batch_size
        )

        # Create repository record
        repo_id = store.create_repository(
            name=repo_name,
            path=str(path_obj.resolve()),
            languages=list(languages)
        )

        click.echo(f"Repository ID: {repo_id['id']}")

        # Index the repository
        stats = GraphBuilderStats()

        for language in languages:
            click.echo(f"\nIndexing {language} files...")

            if scip_only and not _has_language_files(path_obj, language, list(exclude)):
                click.echo(f"  Skipping {language} (no matching files found)")
                continue

            # Try SCIP first
            scip_output = None
            try:
                scip_indexer = SCIPIndexer(language)
                if not scip_indexer.is_available():
                    if scip_only:
                        raise RuntimeError(f"SCIP indexer not available for {language}")
                else:
                    click.echo(f"  Trying SCIP indexer for {language}...")
                    scip_output = scip_indexer.index(
                        path_obj,
                        project_name=repo_name
                    )
            except Exception as e:
                if scip_only:
                    raise
                click.echo(f"  SCIP indexing failed: {e}")

            builder_stats = None

            if scip_output and scip_output.get("documents"):
                click.echo(f"  Using SCIP indexer for {language}")
                try:
                    builder_stats = builder.build_from_scip(
                        scip_output,
                        repo_id['id'],
                        language
                    )
                except Exception as e:
                    click.echo(f"  Error building from SCIP: {e}")
                    stats.errors.append(str(e))
            else:
                if scip_only:
                    raise RuntimeError(f"SCIP indexing produced no documents for {language}")
                # Fallback to regex indexer
                click.echo(f"  Using fallback indexer for {language}")
                try:
                    fallback_indexer = FallbackIndexer(language)
                    nodes, edges = fallback_indexer.index(path_obj, exclude_dirs=list(exclude))

                    builder_stats = builder.build_from_fallback(
                        nodes,
                        edges,
                        repo_id['id']
                    )
                except Exception as e:
                    click.echo(f"  Error with fallback indexer: {e}")
                    stats.errors.append(str(e))

            # Accumulate stats if we got results
            if builder_stats:
                stats.nodes_processed += builder_stats.nodes_processed
                stats.edges_created += builder_stats.edges_created
                stats.files_processed += builder_stats.files_processed
                if builder_stats.errors:
                    stats.errors.extend(builder_stats.errors)

        click.echo(f"\nIndexing complete!")
        click.echo(f"Files processed: {stats.files_processed}")
        click.echo(f"Nodes created: {stats.nodes_processed}")
        click.echo(f"Edges created: {stats.edges_created}")
        click.echo(f"Errors: {len(stats.errors)}")

    except Exception as e:
        logger.error("Indexing failed", error=str(e))
        click.echo(f"Error: {e}", err=True)
        sys.exit(1)


@cli.command()
@click.option('--tenant-id', default=None, help='Tenant ID')
def list_repos(tenant_id):
    """List indexed repositories."""

    if not tenant_id:
        # Get default tenant
        query = "SELECT id FROM aethyme.tenants WHERE name = %s"
        result = db_pool.execute(query, ('aeptus',))
        if result:
            tenant_id = str(result[0]['id'])
        else:
            click.echo("No default tenant found", err=True)
            sys.exit(1)

    query = """
        SELECT id, name, path, created_at, updated_at
        FROM aethyme.repositories
        WHERE tenant_id = %s
        ORDER BY created_at DESC
    """

    repos = db_pool.execute(query, (tenant_id,))

    if not repos:
        click.echo("No repositories found")
        return

    click.echo(f"Found {len(repos)} repositories:\n")

    for repo in repos:
        click.echo(f"ID: {repo['id']}")
        click.echo(f"Name: {repo['name']}")
        click.echo(f"Path: {repo['path']}")
        click.echo(f"Created: {repo['created_at']}")
        click.echo(f"Updated: {repo['updated_at']}")
        click.echo("-" * 40)


@cli.command()
@click.argument('repo-id')
@click.option('--tenant-id', default=None, help='Tenant ID')
def stats(repo_id, tenant_id):
    """Get statistics for a repository."""

    if not tenant_id:
        # Get default tenant
        query = "SELECT id FROM aethyme.tenants WHERE name = %s"
        result = db_pool.execute(query, ('aeptus',))
        if result:
            tenant_id = str(result[0]['id'])

    # Get node counts
    node_query = """
        SELECT kind, COUNT(*) as count
        FROM aethyme.nodes
        WHERE tenant_id = %s AND repository_id = %s
        GROUP BY kind
        ORDER BY count DESC
    """

    nodes = db_pool.execute(node_query, (tenant_id, repo_id))

    # Get edge counts
    edge_query = """
        SELECT edge_type, COUNT(*) as count
        FROM aethyme.edges
        WHERE tenant_id = %s AND repository_id = %s
        GROUP BY edge_type
        ORDER BY count DESC
    """

    edges = db_pool.execute(edge_query, (tenant_id, repo_id))

    click.echo("Repository Statistics:")
    click.echo("\nNodes by kind:")
    for node in nodes:
        click.echo(f"  {node['kind']}: {node['count']}")

    click.echo("\nEdges by type:")
    for edge in edges:
        click.echo(f"  {edge['edge_type']}: {edge['count']}")

    # Total counts
    total_nodes = sum(n['count'] for n in nodes)
    total_edges = sum(e['count'] for e in edges)

    click.echo(f"\nTotal nodes: {total_nodes}")
    click.echo(f"Total edges: {total_edges}")


if __name__ == '__main__':
    cli()
