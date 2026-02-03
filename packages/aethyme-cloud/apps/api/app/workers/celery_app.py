"""Celery application configuration for background task processing."""

from celery import Celery
from app.core.config import settings

# Create Celery app instance
celery_app = Celery(
    "aethyme",
    broker=str(settings.REDIS_URL),
    backend=str(settings.REDIS_URL),
)

# Celery configuration
celery_app.conf.update(
    task_serializer="json",
    accept_content=["json"],
    result_serializer="json",
    timezone="UTC",
    enable_utc=True,
    task_track_started=True,
    task_time_limit=3600,  # 1 hour max per task
    task_soft_time_limit=3000,  # 50 minutes soft limit
    worker_prefetch_multiplier=1,  # One task at a time (for heavy indexing)
    worker_max_tasks_per_child=10,  # Restart worker after 10 tasks (prevent memory leaks)
)

# Auto-discover tasks from all modules
celery_app.autodiscover_tasks(["app.workers.tasks"])

# Task routing (can add priority queues later)
celery_app.conf.task_routes = {
    "app.workers.tasks.indexing.*": {"queue": "indexing"},
    "app.workers.tasks.search.*": {"queue": "search"},
}


@celery_app.task(bind=True)
def debug_task(self):
    """Debug task to test Celery is working."""
    print(f"Request: {self.request!r}")
    return "Celery is working!"
