"""Export graph data from PostgreSQL to JSON."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path

import click
import structlog

from ..graph.connection_pool import db_pool
from ..graph.store import GraphStore

logger = structlog.get_logger(__name__)


def _ensure_default_scope() -> tuple[str, str]:
    org_result = db_pool.execute(
        "SELECT id FROM aethyme.orgs WHERE slug = %s",
        ("default",),
    )
    if org_result:
        org_id = str(org_result[0]["id"])
    else:
        insert_result = db_pool.execute(
            """
            INSERT INTO aethyme.orgs (id, name, slug, description)
            VALUES (%s, %s, %s, %s)
            RETURNING id
            """,
            ("00000000-0000-0000-0000-000000000001", "Default Org", "default", "Default bootstrap org"),
        )
        if not insert_result:
            raise RuntimeError("Failed to create default org")
        org_id = str(insert_result[0]["id"])

    tenant_result = db_pool.execute(
        "SELECT id FROM aethyme.tenants WHERE org_id = %s AND slug = %s",
        (org_id, "default"),
    )
    if tenant_result:
        return org_id, str(tenant_result[0]["id"])

    insert_result = db_pool.execute(
        """
        INSERT INTO aethyme.tenants (id, org_id, name, slug, description)
        VALUES (%s, %s, %s, %s, %s)
        RETURNING id
        """,
        (
            "00000000-0000-0000-0000-000000000001",
            org_id,
            "Default Tenant",
            "default",
            "Default bootstrap tenant",
        ),
    )
    if not insert_result:
        raise RuntimeError("Failed to create default tenant")
    return org_id, str(insert_result[0]["id"])


def _resolve_tenant_id(tenant_id: str | None) -> tuple[str | None, str]:
    if tenant_id:
        result = db_pool.execute(
            "SELECT org_id FROM aethyme.tenants WHERE id = %s",
            (tenant_id,),
        )
        org_id = str(result[0]["org_id"]) if result and result[0].get("org_id") else None
        return org_id, tenant_id

    return _ensure_default_scope()


@click.command()
@click.option("--tenant-id", default=None, help="Tenant ID (defaults to system tenant)")
@click.option("--repo-name", required=True, help="Repository name")
@click.option("--output", required=True, type=click.Path(), help="Output JSON path")
def main(tenant_id: str | None, repo_name: str, output: str) -> None:
    """Export nodes and edges for a repository to JSON."""
    resolved_org_id, resolved_tenant_id = _resolve_tenant_id(tenant_id)
    store = GraphStore(tenant_id=resolved_tenant_id, org_id=resolved_org_id)

    repo = store.get_repository(repo_name)
    if not repo:
        raise click.ClickException(f"Repository not found: {repo_name}")

    output_path = Path(output)
    output_path.parent.mkdir(parents=True, exist_ok=True)

    with store.db_pool.get_connection() as conn:
        with conn.cursor() as cur:
            store.apply_session(cur)

            cur.execute(
                """
                SELECT id, display_id, symbol, file_path, line_number, column_number,
                       kind, language, signature, documentation, metadata
                FROM aethyme.nodes
                WHERE tenant_id = %s AND repository_id = %s
                ORDER BY id
                """,
                (resolved_tenant_id, repo["id"]),
            )
            nodes = cur.fetchall()

            cur.execute(
                """
                SELECT id, from_node_id, to_node_id, edge_type, weight, metadata
                FROM aethyme.edges
                WHERE tenant_id = %s AND repository_id = %s
                ORDER BY id
                """,
                (resolved_tenant_id, repo["id"]),
            )
            edges = cur.fetchall()

    payload = {
        "repository": {
            "id": repo["id"],
            "name": repo["name"],
            "path": repo["path"],
            "languages": repo.get("languages", []),
        },
        "stats": {
            "nodes": len(nodes),
            "edges": len(edges),
        },
        "generated_at": datetime.now(UTC).isoformat(),
        "nodes": nodes,
        "edges": edges,
    }

    output_path.write_text(json.dumps(payload, indent=2, default=str))
    click.echo(f"Exported {len(nodes)} nodes and {len(edges)} edges to {output_path}")


if __name__ == "__main__":
    main()
