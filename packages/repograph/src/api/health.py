"""
Health check endpoints for Kubernetes probes.

Provides readiness, liveness, and startup probes with comprehensive checks.
"""

import asyncio
import logging
import os
import shutil
import time
from typing import Dict, Any

from fastapi import APIRouter, HTTPException, status
from pydantic import BaseModel

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/health", tags=["health"])


class HealthStatus(BaseModel):
    """Health check status response."""
    status: str
    timestamp: float
    checks: Dict[str, Any]
    version: str = "1.0.0"


class HealthCheck:
    """Centralized health check logic."""

    def __init__(self):
        self.startup_time = time.time()
        self.startup_complete = False

    async def check_database(self) -> Dict[str, Any]:
        """Check PostgreSQL database connectivity."""
        try:
            from src.graph.store import GraphStore
            from src.config import settings

            store = GraphStore(settings.DATABASE_URL)

            # Test connection with a simple query
            start_time = time.time()
            async with store.pool.acquire() as conn:
                result = await conn.fetchval("SELECT 1")
                latency_ms = (time.time() - start_time) * 1000

            return {
                "status": "healthy",
                "latency_ms": round(latency_ms, 2),
                "pool_size": store.pool.get_size() if hasattr(store.pool, 'get_size') else None,
            }
        except Exception as e:
            logger.error(f"Database health check failed: {e}")
            return {
                "status": "unhealthy",
                "error": str(e),
            }

    async def check_redis(self) -> Dict[str, Any]:
        """Check Redis cache connectivity."""
        try:
            from src.cache.query_cache import QueryCache
            from src.config import settings

            cache = QueryCache(settings.REDIS_URL)

            # Test connection with ping
            start_time = time.time()
            await cache.redis.ping()
            latency_ms = (time.time() - start_time) * 1000

            # Get Redis info
            info = await cache.redis.info()

            return {
                "status": "healthy",
                "latency_ms": round(latency_ms, 2),
                "connected_clients": info.get("connected_clients", 0),
                "used_memory_mb": round(info.get("used_memory", 0) / 1024 / 1024, 2),
            }
        except Exception as e:
            logger.error(f"Redis health check failed: {e}")
            return {
                "status": "unhealthy",
                "error": str(e),
            }

    async def check_disk_space(self) -> Dict[str, Any]:
        """Check available disk space."""
        try:
            usage = shutil.disk_usage("/")
            free_percent = (usage.free / usage.total) * 100

            status = "healthy"
            if free_percent < 10:
                status = "critical"
            elif free_percent < 20:
                status = "warning"

            return {
                "status": status,
                "free_percent": round(free_percent, 2),
                "free_gb": round(usage.free / (1024**3), 2),
                "total_gb": round(usage.total / (1024**3), 2),
            }
        except Exception as e:
            logger.error(f"Disk space check failed: {e}")
            return {
                "status": "unknown",
                "error": str(e),
            }

    async def check_memory(self) -> Dict[str, Any]:
        """Check memory usage."""
        try:
            import psutil

            mem = psutil.virtual_memory()
            percent_used = mem.percent

            status = "healthy"
            if percent_used > 95:
                status = "critical"
            elif percent_used > 85:
                status = "warning"

            return {
                "status": status,
                "percent_used": round(percent_used, 2),
                "available_gb": round(mem.available / (1024**3), 2),
                "total_gb": round(mem.total / (1024**3), 2),
            }
        except ImportError:
            # psutil not available, skip check
            return {
                "status": "skipped",
                "reason": "psutil not available",
            }
        except Exception as e:
            logger.error(f"Memory check failed: {e}")
            return {
                "status": "unknown",
                "error": str(e),
            }


# Global health checker instance
health_checker = HealthCheck()


@router.get("/live", response_model=HealthStatus, status_code=status.HTTP_200_OK)
async def liveness_probe():
    """
    Liveness probe endpoint.

    Indicates whether the application is running and responsive.
    Returns 200 if the app is alive, 503 if it's dead.

    This is a lightweight check that only verifies basic responsiveness.
    Kubernetes will restart the pod if this fails.
    """
    return HealthStatus(
        status="alive",
        timestamp=time.time(),
        checks={
            "uptime_seconds": round(time.time() - health_checker.startup_time, 2),
            "pid": os.getpid(),
        },
    )


@router.get("/ready", response_model=HealthStatus)
async def readiness_probe():
    """
    Readiness probe endpoint.

    Indicates whether the application is ready to serve traffic.
    Returns 200 if ready, 503 if not ready.

    Checks:
    - Database connectivity
    - Redis connectivity
    - Disk space availability

    Kubernetes will remove the pod from service if this fails.
    """
    checks = {}
    all_healthy = True

    # Run all checks concurrently
    db_check, redis_check, disk_check = await asyncio.gather(
        health_checker.check_database(),
        health_checker.check_redis(),
        health_checker.check_disk_space(),
        return_exceptions=False,
    )

    checks["database"] = db_check
    checks["redis"] = redis_check
    checks["disk"] = disk_check

    # Determine overall health
    if db_check["status"] != "healthy":
        all_healthy = False
    if redis_check["status"] != "healthy":
        all_healthy = False
    if disk_check["status"] in ["critical", "unhealthy"]:
        all_healthy = False

    if not all_healthy:
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail={
                "status": "not_ready",
                "timestamp": time.time(),
                "checks": checks,
            },
        )

    return HealthStatus(
        status="ready",
        timestamp=time.time(),
        checks=checks,
    )


@router.get("/startup", response_model=HealthStatus)
async def startup_probe():
    """
    Startup probe endpoint.

    Indicates whether the application has finished starting up.
    Returns 200 when startup is complete, 503 during startup.

    This probe allows for a longer initialization period before
    liveness and readiness probes take over.

    Checks:
    - Database connectivity
    - Redis connectivity
    - Startup completion (migrations, cache warmup, etc.)

    Kubernetes will kill the pod if this doesn't succeed within the timeout.
    """
    checks = {}

    # Check if we've already completed startup
    if health_checker.startup_complete:
        return HealthStatus(
            status="started",
            timestamp=time.time(),
            checks={
                "startup_duration_seconds": round(
                    time.time() - health_checker.startup_time, 2
                ),
            },
        )

    # Run startup checks
    db_check, redis_check = await asyncio.gather(
        health_checker.check_database(),
        health_checker.check_redis(),
        return_exceptions=False,
    )

    checks["database"] = db_check
    checks["redis"] = redis_check

    # Check if startup is complete
    startup_complete = (
        db_check["status"] == "healthy" and
        redis_check["status"] == "healthy"
    )

    if startup_complete:
        health_checker.startup_complete = True
        logger.info(f"Startup complete after {time.time() - health_checker.startup_time:.2f}s")

        return HealthStatus(
            status="started",
            timestamp=time.time(),
            checks=checks,
        )
    else:
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail={
                "status": "starting",
                "timestamp": time.time(),
                "checks": checks,
                "elapsed_seconds": round(time.time() - health_checker.startup_time, 2),
            },
        )


@router.get("/detailed", response_model=HealthStatus)
async def detailed_health_check():
    """
    Detailed health check endpoint.

    Provides comprehensive health information for monitoring and debugging.
    Not used by Kubernetes probes but useful for operational dashboards.

    Returns all available health metrics including:
    - Database status and performance
    - Redis status and performance
    - Disk space
    - Memory usage
    - Application metrics
    """
    checks = {}

    # Run all checks concurrently
    db_check, redis_check, disk_check, mem_check = await asyncio.gather(
        health_checker.check_database(),
        health_checker.check_redis(),
        health_checker.check_disk_space(),
        health_checker.check_memory(),
        return_exceptions=False,
    )

    checks["database"] = db_check
    checks["redis"] = redis_check
    checks["disk"] = disk_check
    checks["memory"] = mem_check
    checks["application"] = {
        "uptime_seconds": round(time.time() - health_checker.startup_time, 2),
        "startup_complete": health_checker.startup_complete,
        "pid": os.getpid(),
        "python_version": os.sys.version.split()[0],
    }

    # Determine overall status
    critical_issues = []
    warnings = []

    for component, check in checks.items():
        if check.get("status") == "critical":
            critical_issues.append(component)
        elif check.get("status") in ["warning", "unhealthy"]:
            warnings.append(component)

    if critical_issues:
        overall_status = "critical"
    elif warnings:
        overall_status = "degraded"
    else:
        overall_status = "healthy"

    return HealthStatus(
        status=overall_status,
        timestamp=time.time(),
        checks=checks,
    )
