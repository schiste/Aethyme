"""Health check API routes."""

from typing import Dict, Any
from fastapi import APIRouter, status
import psutil
import datetime

from ...graph.connection_pool import db_pool
from ...graph.store import GraphStore

router = APIRouter()


@router.get("/", status_code=status.HTTP_200_OK)
async def health_check() -> Dict[str, Any]:
    """
    Basic health check endpoint.

    Returns 200 if the service is running.
    """
    return {
        "status": "healthy",
        "timestamp": datetime.datetime.utcnow().isoformat(),
    }


@router.get("/live", status_code=status.HTTP_200_OK)
async def liveness_probe() -> Dict[str, str]:
    """
    Kubernetes liveness probe endpoint.

    Returns 200 if the service is alive.
    """
    return {"status": "alive"}


@router.get("/ready")
async def readiness_probe() -> Dict[str, Any]:
    """
    Kubernetes readiness probe endpoint.

    Checks if all dependencies are ready.
    """
    checks = {
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
async def detailed_health() -> Dict[str, Any]:
    """
    Detailed health check with system metrics.

    Includes database stats, memory usage, and graph statistics.
    """
    health = {
        "status": "healthy",
        "timestamp": datetime.datetime.utcnow().isoformat(),
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
        # Get default tenant ID
        query = "SELECT id FROM repograph.tenants WHERE name = 'aeptus' LIMIT 1"
        result = db_pool.execute(query)

        if result:
            tenant_id = str(result[0]["id"])
            store = GraphStore(tenant_id=tenant_id)
            stats = store.get_statistics()
            health["graph"] = stats
    except Exception as e:
        health["graph"] = {"error": str(e)}

    return health