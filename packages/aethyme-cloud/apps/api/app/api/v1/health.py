"""Health check endpoints for monitoring."""

from fastapi import APIRouter, Depends, HTTPException
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession
from app.core.database import get_db
from datetime import datetime
from typing import Dict, Any
import redis.asyncio as aioredis
import httpx
from app.core.config import settings
from app.core.celery_app import celery_app

router = APIRouter()


async def check_database(db: AsyncSession) -> Dict[str, Any]:
    """Check database connectivity and performance."""
    try:
        start = datetime.utcnow()
        result = await db.execute(text("SELECT 1"))
        latency_ms = (datetime.utcnow() - start).total_seconds() * 1000

        if result:
            return {
                "status": "healthy",
                "latency_ms": round(latency_ms, 2),
                "connection_pool": "active"
            }
        return {"status": "unhealthy", "error": "No result from query"}
    except Exception as e:
        return {"status": "unhealthy", "error": str(e)}


async def check_redis() -> Dict[str, Any]:
    """Check Redis connectivity and performance."""
    redis_client = None
    try:
        redis_client = aioredis.from_url(
            str(settings.REDIS_URL),
            encoding="utf-8",
            decode_responses=True,
        )
        start = datetime.utcnow()
        await redis_client.ping()
        latency_ms = (datetime.utcnow() - start).total_seconds() * 1000

        # Get Redis info
        info = await redis_client.info()

        return {
            "status": "healthy",
            "latency_ms": round(latency_ms, 2),
            "connected_clients": info.get("connected_clients", "unknown"),
            "used_memory_human": info.get("used_memory_human", "unknown"),
        }
    except Exception as e:
        return {"status": "unhealthy", "error": str(e)}
    finally:
        if redis_client:
            await redis_client.close()


async def check_elasticsearch() -> Dict[str, Any]:
    """Check Elasticsearch connectivity and cluster health."""
    try:
        async with httpx.AsyncClient() as client:
            start = datetime.utcnow()
            response = await client.get(
                f"{settings.ELASTICSEARCH_URL}/_cluster/health",
                timeout=5.0
            )
            latency_ms = (datetime.utcnow() - start).total_seconds() * 1000

            if response.status_code == 200:
                health = response.json()
                return {
                    "status": "healthy" if health["status"] in ["green", "yellow"] else "unhealthy",
                    "latency_ms": round(latency_ms, 2),
                    "cluster_status": health["status"],
                    "number_of_nodes": health["number_of_nodes"],
                    "active_shards": health["active_shards"],
                }
            return {"status": "unhealthy", "error": f"HTTP {response.status_code}"}
    except httpx.TimeoutException:
        return {"status": "unhealthy", "error": "Connection timeout"}
    except Exception as e:
        return {"status": "unhealthy", "error": str(e)}


def check_celery_workers() -> Dict[str, Any]:
    """Check Celery worker availability and status."""
    try:
        # Inspect active workers
        inspect = celery_app.control.inspect()

        # Get active workers (with timeout)
        active_workers = inspect.active()
        if not active_workers:
            return {
                "status": "unhealthy",
                "error": "No active workers found",
                "active_workers": 0
            }

        worker_count = len(active_workers)
        total_tasks = sum(len(tasks) for tasks in active_workers.values())

        return {
            "status": "healthy",
            "active_workers": worker_count,
            "active_tasks": total_tasks,
            "workers": list(active_workers.keys())
        }
    except Exception as e:
        return {"status": "unhealthy", "error": str(e)}


@router.get("/health")
async def health_check():
    """
    Basic health check endpoint.
    Returns 200 OK if the API is running.
    Use this for Kubernetes liveness probes.
    """
    return {
        "status": "healthy",
        "timestamp": datetime.utcnow().isoformat(),
        "service": "aethyme-cloud-api"
    }


@router.get("/health/ready")
async def readiness_check(db: AsyncSession = Depends(get_db)):
    """
    Readiness check endpoint.
    Returns 200 OK only if all critical dependencies are healthy.
    Use this for Kubernetes readiness probes.
    """
    # Check critical dependencies
    db_check = await check_database(db)
    redis_check = await check_redis()

    # API is ready if database and redis are healthy
    is_ready = (
        db_check["status"] == "healthy" and
        redis_check["status"] == "healthy"
    )

    if not is_ready:
        raise HTTPException(
            status_code=503,
            detail={
                "status": "not_ready",
                "database": db_check,
                "redis": redis_check,
                "timestamp": datetime.utcnow().isoformat()
            }
        )

    return {
        "status": "ready",
        "timestamp": datetime.utcnow().isoformat()
    }


@router.get("/health/detailed")
async def detailed_health(db: AsyncSession = Depends(get_db)):
    """
    Detailed health check with all dependencies.
    Returns comprehensive status for monitoring dashboards.
    """
    checks = {
        "api": {"status": "healthy", "version": "1.0.0"},
        "timestamp": datetime.utcnow().isoformat(),
    }

    # Check all dependencies
    checks["database"] = await check_database(db)
    checks["redis"] = await check_redis()
    checks["elasticsearch"] = await check_elasticsearch()
    checks["celery"] = check_celery_workers()

    # Calculate overall status
    dependency_statuses = [
        checks["database"]["status"],
        checks["redis"]["status"],
        checks["elasticsearch"]["status"],
        checks["celery"]["status"],
    ]

    if all(s == "healthy" for s in dependency_statuses):
        overall_status = "healthy"
    elif any(s == "healthy" for s in dependency_statuses):
        overall_status = "degraded"
    else:
        overall_status = "unhealthy"

    checks["overall_status"] = overall_status

    return checks
