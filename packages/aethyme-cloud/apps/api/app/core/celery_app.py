"""Celery application configuration for background tasks."""

from celery import Celery
from app.core.config import settings

# Create Celery application
celery_app = Celery(
    "aethyme_cloud",
    broker=str(settings.REDIS_URL),
    backend=str(settings.REDIS_URL),
    include=[
        "app.tasks.indexing",
        "app.tasks.github",
    ],
)

# Configure Celery
celery_app.conf.update(
    # Serialization
    task_serializer="json",
    accept_content=["json"],
    result_serializer="json",

    # Timezone
    timezone="UTC",
    enable_utc=True,

    # Task execution
    task_track_started=True,
    task_time_limit=3600,  # 1 hour hard limit
    task_soft_time_limit=3000,  # 50 minutes soft limit
    task_acks_late=True,  # Acknowledge tasks after completion (safer for failures)
    task_reject_on_worker_lost=True,  # Requeue tasks if worker crashes

    # Worker
    worker_prefetch_multiplier=1,  # Fetch one task at a time (better for long tasks)
    worker_max_tasks_per_child=1000,  # Restart worker after 1000 tasks (prevent memory leaks)
    worker_disable_rate_limits=False,

    # Results
    result_expires=86400,  # Results expire after 24 hours
    result_extended=True,  # Store more task metadata

    # Retry policy
    task_autoretry_for=(Exception,),  # Auto-retry on any exception
    task_retry_kwargs={"max_retries": 3, "countdown": 60},  # Retry 3 times with 60s delay

    # Beat scheduler (for periodic tasks)
    beat_scheduler="celery.beat:PersistentScheduler",
    beat_schedule_filename="/tmp/celerybeat-schedule",
)

# Task routes (optional - for multiple queues)
celery_app.conf.task_routes = {
    "app.tasks.indexing.*": {"queue": "indexing"},
    "app.tasks.github.*": {"queue": "github"},
}

# Event monitoring
celery_app.conf.worker_send_task_events = True
celery_app.conf.task_send_sent_event = True


if __name__ == "__main__":
    celery_app.start()
