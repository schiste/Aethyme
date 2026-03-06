"""Freshness monitoring and re-indexing support."""

from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timedelta
from enum import Enum
from typing import Protocol, TypedDict

import structlog

from ..graph.connection_pool import DatabaseRow, SQLParams

logger = structlog.get_logger(__name__)


class DatabaseExecutor(Protocol):
    """Minimal DB interface used by freshness tracking."""

    def execute(
        self,
        query: str,
        params: SQLParams | None = None,
        fetch: bool = True,
    ) -> list[DatabaseRow] | None: ...


class WebhookPayload(TypedDict, total=False):
    """Subset of webhook fields used by reindex triggers."""

    ref: str


class FreshnessStatus(Enum):
    """Status of repository index freshness."""
    FRESH = "fresh"
    STALE = "stale"
    CRITICAL = "critical"  # Very stale
    NEVER_INDEXED = "never_indexed"


@dataclass
class FreshnessMetrics:
    """Metrics about repository freshness."""
    repository_id: str
    repository_name: str
    last_indexed_at: datetime | None
    staleness_hours: float | None
    status: FreshnessStatus
    warning_threshold_hours: float
    critical_threshold_hours: float


class FreshnessMonitor:
    """
    Monitors index freshness and triggers re-indexing.

    Tracks when repositories were last indexed and identifies stale indexes
    that need refreshing.
    """

    def __init__(
        self,
        db_pool: DatabaseExecutor,
        warning_threshold_hours: float = 24.0,
        critical_threshold_hours: float = 72.0,
    ) -> None:
        """
        Initialize freshness monitor.

        Args:
            db_pool: Database connection pool
            warning_threshold_hours: Hours after which index is considered stale
            critical_threshold_hours: Hours after which index is critically stale
        """
        self.db_pool = db_pool
        self.warning_threshold_hours = warning_threshold_hours
        self.critical_threshold_hours = critical_threshold_hours
        self.logger = logger.bind(component="FreshnessMonitor")

    def get_repository_freshness(
        self,
        repository_id: str,
        tenant_id: str,
    ) -> FreshnessMetrics:
        """
        Get freshness metrics for a repository.

        Args:
            repository_id: Repository ID
            tenant_id: Tenant ID

        Returns:
            FreshnessMetrics
        """
        result = self.db_pool.execute(
            """
            SELECT id, name, last_indexed_at
            FROM aethyme.repositories
            WHERE id = %s AND tenant_id = %s
            """,
            (repository_id, tenant_id),
        )

        if not result:
            raise ValueError(f"Repository {repository_id} not found")

        repo = result[0]
        last_indexed_at = repo.get("last_indexed_at")

        if not last_indexed_at:
            return FreshnessMetrics(
                repository_id=repository_id,
                repository_name=repo["name"],
                last_indexed_at=None,
                staleness_hours=None,
                status=FreshnessStatus.NEVER_INDEXED,
                warning_threshold_hours=self.warning_threshold_hours,
                critical_threshold_hours=self.critical_threshold_hours,
            )

        # Calculate staleness
        now = datetime.now()
        staleness = now - last_indexed_at
        staleness_hours = staleness.total_seconds() / 3600

        # Determine status
        if staleness_hours > self.critical_threshold_hours:
            status = FreshnessStatus.CRITICAL
        elif staleness_hours > self.warning_threshold_hours:
            status = FreshnessStatus.STALE
        else:
            status = FreshnessStatus.FRESH

        return FreshnessMetrics(
            repository_id=repository_id,
            repository_name=repo["name"],
            last_indexed_at=last_indexed_at,
            staleness_hours=staleness_hours,
            status=status,
            warning_threshold_hours=self.warning_threshold_hours,
            critical_threshold_hours=self.critical_threshold_hours,
        )

    def get_stale_repositories(
        self,
        tenant_id: str,
        threshold_hours: float | None = None,
        include_never_indexed: bool = True,
    ) -> list[FreshnessMetrics]:
        """
        Get all stale repositories for a tenant.

        Args:
            tenant_id: Tenant ID
            threshold_hours: Custom threshold (uses warning threshold if not provided)
            include_never_indexed: Include repos that have never been indexed

        Returns:
            List of FreshnessMetrics for stale repositories
        """
        threshold_hours = threshold_hours or self.warning_threshold_hours
        threshold_time = datetime.now() - timedelta(hours=threshold_hours)

        query_parts: list[str] = []
        params: list[str | datetime] = [tenant_id]

        if include_never_indexed:
            query_parts.append("last_indexed_at IS NULL")

        query_parts.append("last_indexed_at < %s")
        params.append(threshold_time)

        where_clause = " OR ".join(query_parts)

        results = self.db_pool.execute(
            f"""
            SELECT id, name, last_indexed_at
            FROM aethyme.repositories
            WHERE tenant_id = %s AND ({where_clause} )
            ORDER BY last_indexed_at ASC NULLS FIRST
            """,
            params,
        )

        metrics_list: list[FreshnessMetrics] = []
        for repo in results or []:
            metrics = self.get_repository_freshness(str(repo["id"]), tenant_id)
            metrics_list.append(metrics)

        self.logger.info(
            "Found stale repositories",
            count=len(metrics_list),
            threshold_hours=threshold_hours,
        )

        return metrics_list

    def mark_index_started(
        self,
        repository_id: str,
        tenant_id: str,
    ) -> None:
        """
        Mark that indexing has started for a repository.

        Args:
            repository_id: Repository ID
            tenant_id: Tenant ID
        """
        self.db_pool.execute(
            """
            UPDATE aethyme.repositories
            SET index_status = 'indexing',
                updated_at = CURRENT_TIMESTAMP
            WHERE id = %s AND tenant_id = %s
            """,
            (repository_id, tenant_id),
            fetch=False,
        )

        self.logger.info(
            "Marked indexing started",
            repository_id=repository_id,
        )

    def mark_index_completed(
        self,
        repository_id: str,
        tenant_id: str,
        success: bool = True,
        error_message: str | None = None,
    ) -> None:
        """
        Mark that indexing completed for a repository.

        Args:
            repository_id: Repository ID
            tenant_id: Tenant ID
            success: Whether indexing succeeded
            error_message: Error message if failed
        """
        status = "completed" if success else "failed"

        self.db_pool.execute(
            """
            UPDATE aethyme.repositories
            SET index_status = %s,
                last_indexed_at = CASE WHEN %s THEN CURRENT_TIMESTAMP ELSE last_indexed_at END,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = %s AND tenant_id = %s
            """,
            (status, success, repository_id, tenant_id),
            fetch=False,
        )

        self.logger.info(
            "Marked indexing completed",
            repository_id=repository_id,
            success=success,
            error=error_message,
        )

    def get_freshness_summary(self, tenant_id: str) -> dict[str, int]:
        """
        Get summary of freshness status across all repositories.

        Args:
            tenant_id: Tenant ID

        Returns:
            Dict with counts by status
        """
        results = self.db_pool.execute(
            """
            SELECT id
            FROM aethyme.repositories
            WHERE tenant_id = %s
            """,
            (tenant_id,),
        )

        summary = {
            "fresh": 0,
            "stale": 0,
            "critical": 0,
            "never_indexed": 0,
        }

        for repo in results or []:
            metrics = self.get_repository_freshness(str(repo["id"]), tenant_id)
            summary[metrics.status.value] += 1

        return summary


class ReindexTrigger:
    """
    Handles triggering of re-indexing operations.

    Supports scheduled re-indexing and webhook-based triggers.
    """

    def __init__(
        self,
        freshness_monitor: FreshnessMonitor,
        index_callback: Callable[[str, str], None],
    ) -> None:
        """
        Initialize re-index trigger.

        Args:
            freshness_monitor: FreshnessMonitor instance
            index_callback: Function to call to trigger indexing (repo_id, tenant_id)
        """
        self.freshness_monitor = freshness_monitor
        self.index_callback = index_callback
        self.logger = logger.bind(component="ReindexTrigger")

    async def reindex_stale_repos(
        self,
        tenant_id: str,
        threshold_hours: float | None = None,
        dry_run: bool = False,
    ) -> list[str]:
        """
        Trigger re-indexing for all stale repositories.

        Args:
            tenant_id: Tenant ID
            threshold_hours: Custom staleness threshold
            dry_run: If True, only report what would be reindexed

        Returns:
            List of repository IDs that were (or would be) reindexed
        """
        stale_repos = self.freshness_monitor.get_stale_repositories(
            tenant_id=tenant_id,
            threshold_hours=threshold_hours,
        )

        self.logger.info(
            "Starting stale repository reindex",
            count=len(stale_repos),
            dry_run=dry_run,
        )

        reindexed: list[str] = []
        for metrics in stale_repos:
            if dry_run:
                self.logger.info(
                    "Would reindex",
                    repository_id=metrics.repository_id,
                    repository_name=metrics.repository_name,
                    staleness_hours=metrics.staleness_hours,
                )
            else:
                self.logger.info(
                    "Triggering reindex",
                    repository_id=metrics.repository_id,
                    repository_name=metrics.repository_name,
                    staleness_hours=metrics.staleness_hours,
                )

                try:
                    self.index_callback(metrics.repository_id, tenant_id)
                    reindexed.append(metrics.repository_id)
                except Exception as e:
                    self.logger.error(
                        "Failed to trigger reindex",
                        repository_id=metrics.repository_id,
                        error=str(e),
                    )

        return reindexed

    def handle_webhook(
        self,
        repository_id: str,
        tenant_id: str,
        event_type: str,
        payload: WebhookPayload,
    ) -> None:
        """
        Handle webhook event for repository changes.

        Args:
            repository_id: Repository ID
            tenant_id: Tenant ID
            event_type: Type of event (e.g., "push", "pull_request")
            payload: Webhook payload
        """
        self.logger.info(
            "Received webhook",
            repository_id=repository_id,
            event_type=event_type,
        )

        # Only trigger on push events for now
        if event_type == "push":
            # Check if push is to main branch
            ref = payload.get("ref", "")
            if ref in ["refs/heads/main", "refs/heads/master"]:
                self.logger.info(
                    "Triggering reindex from push event",
                    repository_id=repository_id,
                    ref=ref,
                )

                try:
                    self.index_callback(repository_id, tenant_id)
                except Exception as e:
                    self.logger.error(
                        "Failed to trigger reindex from webhook",
                        repository_id=repository_id,
                        error=str(e),
                    )
            else:
                self.logger.debug(
                    "Ignoring push to non-main branch",
                    ref=ref,
                )
        else:
            self.logger.debug(
                "Ignoring non-push event",
                event_type=event_type,
            )


def format_staleness(staleness_hours: float | None) -> str:
    """
    Format staleness duration in human-readable form.

    Args:
        staleness_hours: Staleness in hours

    Returns:
        Formatted string
    """
    if staleness_hours is None:
        return "never indexed"

    if staleness_hours < 1:
        minutes = int(staleness_hours * 60)
        return f"{minutes} minutes ago"
    elif staleness_hours < 24:
        return f"{staleness_hours:.1f} hours ago"
    else:
        days = staleness_hours / 24
        return f"{days:.1f} days ago"
