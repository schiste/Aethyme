"""Database helpers for auth and RLS integration tests."""

from __future__ import annotations

import json
import uuid
from collections.abc import Sequence
from contextlib import AbstractContextManager
from typing import Any, Protocol, TypedDict, cast

from src.graph.connection_pool import db_pool

RUNTIME_ROLE = "aethyme_runtime_test"


class TenantRecord(TypedDict):
    org_id: str
    tenant_id: str


DatabaseRow = dict[str, Any]
DatabaseResult = list[DatabaseRow]


class CursorDescription(Protocol):
    name: str


class CursorProtocol(Protocol):
    description: Sequence[CursorDescription] | None

    def execute(self, query: str, params: Sequence[object] | None = None) -> object: ...

    def fetchall(self) -> list[Sequence[Any]]: ...


class ConnectionProtocol(Protocol):
    def cursor(self) -> AbstractContextManager[CursorProtocol]: ...


class DBPoolProtocol(Protocol):
    def execute(
        self,
        query: str,
        params: Sequence[object] | None = None,
        fetch: bool = True,
    ) -> DatabaseResult | None: ...

    def get_connection(self) -> AbstractContextManager[ConnectionProtocol]: ...


typed_db_pool = cast(DBPoolProtocol, db_pool)


def create_org_and_tenant(prefix: str) -> TenantRecord:
    """Create an org and tenant pair for integration tests."""
    org_id = str(uuid.uuid4())
    tenant_id = str(uuid.uuid4())
    org_slug = f"{prefix}-org-{org_id[:8]}"
    tenant_slug = f"{prefix}-tenant-{tenant_id[:8]}"

    typed_db_pool.execute(
        """
        INSERT INTO aethyme.orgs (id, name, slug, description)
        VALUES (%s, %s, %s, %s)
        """,
        (org_id, f"{prefix}-org", org_slug, f"Test org for {prefix}"),
        fetch=False,
    )
    typed_db_pool.execute(
        """
        INSERT INTO aethyme.tenants (id, org_id, name, slug, description)
        VALUES (%s, %s, %s, %s, %s)
        """,
        (
            tenant_id,
            org_id,
            f"{prefix}-tenant",
            tenant_slug,
            f"Test tenant for {prefix}",
        ),
        fetch=False,
    )
    return {"org_id": org_id, "tenant_id": tenant_id}


def delete_org(org_id: str) -> None:
    """Delete a test org and everything below it."""
    typed_db_pool.execute("DELETE FROM aethyme.orgs WHERE id = %s", (org_id,), fetch=False)


def execute_as(
    *,
    tenant_id: str,
    org_id: str | None,
    scopes: list[str],
    query: str,
    params: Sequence[object] | None = None,
    fetch: bool = True,
) -> DatabaseResult | None:
    """Execute SQL using one connection with tenant/org/scopes session context."""
    with typed_db_pool.get_connection() as conn:
        with conn.cursor() as cur:
            cur.execute(f"SET ROLE {RUNTIME_ROLE}")
            if org_id:
                cur.execute("SELECT set_config('app.current_org', %s, false)", (org_id,))
            cur.execute("SELECT set_config('app.current_tenant', %s, false)", (tenant_id,))
            cur.execute(
                "SELECT set_config('app.current_scopes', %s, false)",
                (json.dumps(scopes),),
            )
            cur.execute(query, params)
            if fetch and cur.description:
                rows = cur.fetchall()
                normalized_rows: DatabaseResult = []
                for row in rows:
                    if isinstance(row, dict):
                        normalized_rows.append(dict(cast(dict[str, Any], row)))
                        continue
                    column_names = [col.name for col in cur.description]
                    normalized_rows.append(dict(zip(column_names, row, strict=False)))
                return normalized_rows
    return None


def create_repository(*, org_id: str, tenant_id: str, name: str, path: str) -> str:
    """Create a repository under an authenticated tenant context."""
    repo_id = str(uuid.uuid4())
    execute_as(
        tenant_id=tenant_id,
        org_id=org_id,
        scopes=["repo:write", "org:admin"],
        query="""
            INSERT INTO aethyme.repositories (id, org_id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s, %s)
            RETURNING id
        """,
        params=(repo_id, org_id, tenant_id, name, path),
    )
    return repo_id


def create_node(
    *,
    org_id: str,
    tenant_id: str,
    repository_id: str,
    node_id: str,
    symbol: str,
    file_path: str,
    kind: str = "function",
    language: str = "python",
) -> None:
    """Create a graph node under an authenticated tenant context."""
    execute_as(
        tenant_id=tenant_id,
        org_id=org_id,
        scopes=["repo:write", "org:admin"],
        query="""
            INSERT INTO aethyme.nodes (
                id, org_id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
        """,
        params=(
            node_id,
            org_id,
            tenant_id,
            repository_id,
            symbol,
            file_path,
            1,
            1,
            kind,
            language,
        ),
        fetch=False,
    )


def create_edge(
    *,
    org_id: str,
    tenant_id: str,
    repository_id: str,
    edge_id: str,
    from_node_id: str,
    to_node_id: str,
    edge_type: str = "invoke",
) -> None:
    """Create a graph edge under an authenticated tenant context."""
    execute_as(
        tenant_id=tenant_id,
        org_id=org_id,
        scopes=["repo:write", "org:admin"],
        query="""
            INSERT INTO aethyme.edges (
                id, org_id, tenant_id, repository_id, from_node_id, to_node_id, edge_type
            ) VALUES (%s, %s, %s, %s, %s, %s, %s)
        """,
        params=(edge_id, org_id, tenant_id, repository_id, from_node_id, to_node_id, edge_type),
        fetch=False,
    )
