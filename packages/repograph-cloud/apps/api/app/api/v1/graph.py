"""
Graph Analysis API Endpoints

Provides graph-based code analysis:
- Call graphs
- Class hierarchies
- Dependency graphs
- Impact analysis
"""

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlalchemy.ext.asyncio import AsyncSession
from typing import Literal

from app.core.deps import get_db, get_current_user
from app.models.user import User
from app.services.graph_analysis import GraphAnalysisService

router = APIRouter()


@router.get("/call-graph/{symbol_id}")
async def get_call_graph(
    symbol_id: str,
    max_depth: int = Query(default=3, ge=1, le=10, description="Maximum traversal depth"),
    direction: Literal["callers", "callees", "both"] = Query(
        default="both",
        description="Direction to traverse: callers (who calls this), callees (what this calls), or both"
    ),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Get call graph for a function/method.

    Shows which functions call this function (callers) and
    which functions this function calls (callees).

    **Example**:
    ```
    GET /api/v1/graph/call-graph/myapp.services.auth.authenticate?direction=both&max_depth=3
    ```

    **Response**:
    ```json
    {
      "nodes": [
        {
          "id": "myapp.services.auth.authenticate",
          "label": "authenticate",
          "type": "root",
          "depth": 0
        },
        {
          "id": "myapp.api.login.login_user",
          "label": "login_user",
          "type": "caller",
          "depth": 1,
          "file": "api/login.py",
          "kind": "function"
        }
      ],
      "edges": [
        {
          "source": "myapp.api.login.login_user",
          "target": "myapp.services.auth.authenticate",
          "type": "calls",
          "file": "api/login.py",
          "line": 45
        }
      ],
      "root": "myapp.services.auth.authenticate",
      "stats": {
        "total_nodes": 15,
        "total_edges": 14,
        "max_depth": 3
      }
    }
    ```
    """
    service = GraphAnalysisService(db)

    try:
        graph_data = await service.get_call_graph(
            symbol_id=symbol_id,
            max_depth=max_depth,
            direction=direction
        )
        return graph_data
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to generate call graph: {str(e)}")


@router.get("/class-hierarchy/{symbol_id}")
async def get_class_hierarchy(
    symbol_id: str,
    direction: Literal["parents", "children", "both"] = Query(
        default="both",
        description="Direction: parents (superclasses), children (subclasses), or both"
    ),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Get class inheritance hierarchy.

    Shows parent classes (superclasses) and child classes (subclasses)
    for a given class.

    **Example**:
    ```
    GET /api/v1/graph/class-hierarchy/myapp.services.UserService?direction=both
    ```

    **Response**:
    ```json
    {
      "nodes": [
        {
          "id": "myapp.services.UserService",
          "label": "UserService",
          "type": "root",
          "level": 0
        },
        {
          "id": "myapp.services.BaseService",
          "label": "BaseService",
          "type": "parent",
          "level": 1,
          "file": "services/base.py"
        },
        {
          "id": "myapp.services.AdminUserService",
          "label": "AdminUserService",
          "type": "child",
          "level": -1,
          "file": "services/admin.py"
        }
      ],
      "edges": [
        {
          "source": "myapp.services.UserService",
          "target": "myapp.services.BaseService",
          "type": "inherits"
        },
        {
          "source": "myapp.services.AdminUserService",
          "target": "myapp.services.UserService",
          "type": "inherits"
        }
      ],
      "root": "myapp.services.UserService"
    }
    ```
    """
    service = GraphAnalysisService(db)

    try:
        hierarchy_data = await service.get_class_hierarchy(
            symbol_id=symbol_id,
            direction=direction
        )
        return hierarchy_data
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to generate class hierarchy: {str(e)}")


@router.get("/dependencies/{repository_id}/{file_path:path}")
async def get_dependencies(
    repository_id: str,
    file_path: str,
    direction: Literal["imports", "imported_by", "both"] = Query(
        default="both",
        description="Direction: imports (what this imports), imported_by (who imports this), or both"
    ),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Get import dependencies for a file.

    Shows which files this file imports and which files import this file.

    **Example**:
    ```
    GET /api/v1/graph/dependencies/repo-123/src/services/auth.py?direction=both
    ```

    **Response**:
    ```json
    {
      "nodes": [
        {
          "id": "src/services/auth.py",
          "label": "auth.py",
          "type": "root"
        },
        {
          "id": "src/services/jwt.py",
          "label": "jwt.py",
          "type": "import"
        },
        {
          "id": "src/api/login.py",
          "label": "login.py",
          "type": "imported_by"
        }
      ],
      "edges": [
        {
          "source": "src/services/auth.py",
          "target": "src/services/jwt.py",
          "type": "imports"
        },
        {
          "source": "src/api/login.py",
          "target": "src/services/auth.py",
          "type": "imports"
        }
      ],
      "root": "src/services/auth.py"
    }
    ```
    """
    service = GraphAnalysisService(db)

    try:
        dependency_data = await service.get_dependencies(
            file_path=file_path,
            repository_id=repository_id,
            direction=direction
        )
        return dependency_data
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to generate dependency graph: {str(e)}")


@router.get("/impact/{symbol_id}")
async def get_impact_analysis(
    symbol_id: str,
    max_depth: int = Query(default=5, ge=1, le=10, description="Maximum traversal depth"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Analyze the impact of changing a symbol.

    Shows all symbols that would be affected if this symbol is modified.
    Useful for understanding the "blast radius" of a change.

    **Example**:
    ```
    GET /api/v1/graph/impact/myapp.models.User?max_depth=5
    ```

    **Response**:
    ```json
    {
      "symbol_id": "myapp.models.User",
      "total_affected": 42,
      "affected_symbols": [
        {
          "symbol_id": "myapp.services.UserService",
          "inbound_calls": 15,
          "outbound_calls": 8,
          "dependencies": 3,
          "dependents": 12,
          "impact_score": 85
        },
        {
          "symbol_id": "myapp.api.users.get_user",
          "inbound_calls": 5,
          "outbound_calls": 2,
          "dependencies": 1,
          "dependents": 3,
          "impact_score": 45
        }
      ],
      "max_depth_searched": 5
    }
    ```

    **Interpretation**:
    - `total_affected`: Number of symbols that depend on this one
    - `impact_score`: 0-100, higher = more critical
    - Higher scores indicate changes here would have wide-reaching effects
    """
    service = GraphAnalysisService(db)

    try:
        impact_data = await service.get_impact_analysis(
            symbol_id=symbol_id,
            max_depth=max_depth
        )
        return impact_data
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to generate impact analysis: {str(e)}")


@router.get("/statistics/{repository_id}")
async def get_repository_statistics(
    repository_id: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Get graph statistics for a repository.

    Provides high-level metrics about the code graph:
    - Total symbols and relationships
    - Relationship type breakdown
    - Most connected/critical symbols

    **Example**:
    ```
    GET /api/v1/graph/statistics/repo-123
    ```

    **Response**:
    ```json
    {
      "repository_id": "repo-123",
      "total_symbols": 1543,
      "relationship_counts": {
        "calls": 4521,
        "inherits": 234,
        "imports": 892,
        "references": 3421
      },
      "total_relationships": 9068,
      "top_impact_symbols": [
        {
          "symbol_id": "myapp.core.BaseModel",
          "impact_score": 98,
          "inbound_calls": 145,
          "outbound_calls": 12
        },
        {
          "symbol_id": "myapp.utils.logger",
          "impact_score": 92,
          "inbound_calls": 234,
          "outbound_calls": 3
        }
      ]
    }
    ```
    """
    service = GraphAnalysisService(db)

    try:
        stats = await service.get_symbol_statistics(repository_id=repository_id)
        return stats
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Failed to get statistics: {str(e)}")
