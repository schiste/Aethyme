import datetime
from typing import Any

import psutil
from fastapi import APIRouter, status

from ...graph.connection_pool import db_pool
from ...graph.store import GraphStore

router = APIRouter()


@router.get("/", status_code=status.HTTP_200_OK)
async def health_check() -> dict[str, Any]:
    """
    Basic health check endpoint.

    Returns 200 if the service is running.
    """
    return {
        "status": "healthy",
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(),
    }


@router.get("/live", status_code=status.HTTP_200_OK)
async def liveness_probe() -> dict[str, str]:
    """
    Kubernetes liveness probe endpoint.

    Returns 200 if the service is alive.
    """
    return {"status": "alive"}


@router.get("/ready")
async def readiness_probe() -> dict[str, Any]:
    """
    Kubernetes readiness probe endpoint.

    Checks if all dependencies are ready.
    """
    checks: dict[str, bool | None] = {
        "database": False,
        "redis": False,
    }

    # Check database
    try:
        health = db_pool.health_check()
        checks["database"] = health["status"] == "healthy"
    except Exception:
        checks["database"] = False

    # Check Redis
    from ..main import app

    if hasattr(app.state, "redis") and app.state.redis:
        try:
            app.state.redis.ping()
            checks["redis"] = True
        except Exception:
            checks["redis"] = False
    else:
        checks["redis"] = None  # Redis is optional

    # Overall readiness
    ready = checks["database"] and (checks["redis"] is None or checks["redis"])

    return {
        "status": "ready" if ready else "not_ready",
        "checks": checks,
    }


@router.get("/detailed")
async def detailed_health() -> dict[str, Any]:
    """
    Detailed health check with system metrics.

    Includes database stats, memory usage, and graph statistics.
    """
    health: dict[str, Any] = {
        "status": "healthy",
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(),
        "database": {},
        "system": {},
        "graph": {},
    }

    # Database health
    try:
        db_health = db_pool.health_check()
        pool_stats = db_pool.get_stats()
        health["database"] = {
            "status": db_health["status"],
            "pool": pool_stats,
        }
    except Exception as e:
        health["database"] = {
            "status": "error",
            "error": str(e),
        }
        health["status"] = "degraded"

    # System metrics
    try:
        health["system"] = {
            "cpu_percent": psutil.cpu_percent(interval=0.1),
            "memory": {
                "percent": psutil.virtual_memory().percent,
                "used_mb": psutil.virtual_memory().used / (1024 * 1024),
                "available_mb": psutil.virtual_memory().available / (1024 * 1024),
            },
            "disk": {
                "percent": psutil.disk_usage("/").percent,
                "free_gb": psutil.disk_usage("/").free / (1024 * 1024 * 1024),
            },
        }
    except Exception as e:
        health["system"] = {"error": str(e)}

    # Graph statistics (from default tenant)
    try:
        result = db_pool.execute(
            """
            SELECT t.id, t.org_id
            FROM aethyme.tenants t
            JOIN aethyme.orgs o ON o.id = t.org_id
            WHERE o.slug = 'default' AND t.slug = 'default'
            LIMIT 1
            """
        )

        if result:
            tenant_id = str(result[0]["id"])
            org_id = str(result[0]["org_id"])
            store = GraphStore(tenant_id=tenant_id, org_id=org_id)
            stats = store.get_statistics()
            health["graph"] = stats
    except Exception as e:
        health["graph"] = {"error": str(e)}

    return health
