"""Scorecard API for Aethyme SDK."""

from typing import TYPE_CHECKING, Any, cast

from .models import ScorecardResult

if TYPE_CHECKING:
    from .client import AethymeClient


class ScorecardAPI:
    """
    Scorecard API interface.

    Provides methods for running AI-readiness scorecards.
    """

    def __init__(self, client: "AethymeClient"):
        self.client = client

    def scan(
        self,
        repo_id: str,
        checks: list[str] | None = None,
        include_suggestions: bool = True,
    ) -> ScorecardResult:
        """
        Run AI-readiness scorecard on a repository.

        Args:
            repo_id: Repository ID to scan
            checks: Specific checks to run (runs all if None)
            include_suggestions: Include fix suggestions

        Returns:
            Scorecard result

        Example:
            >>> scorecard = client.scorecard.scan(repo_id="abc123")
            >>> print(f"Score: {scorecard.overall_score}/100")
            >>> print(f"Violations: {len(scorecard.violations)}")
        """
        data: dict[str, object] = {
            "repo_id": repo_id,
            "include_suggestions": include_suggestions,
        }
        if checks:
            data["checks"] = checks

        response = self.client.post("/api/v1/scorecard/scan", json=data)
        return ScorecardResult(**response)

    def get_history(
        self,
        repo_id: str,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        """
        Get scorecard history for a repository.

        Args:
            repo_id: Repository ID
            limit: Maximum number of results

        Returns:
            List of historical scan results

        Example:
            >>> history = client.scorecard.get_history(repo_id="abc123")
            >>> for scan in history:
            ...     print(f"Score: {scan['overall_score']}")
        """
        response = self.client.get(
            f"/api/v1/scorecard/history/{repo_id}",
            params={"limit": limit},
        )
        history = response.get("history", [])
        if isinstance(history, list):
            history_items: list[dict[str, Any]] = []
            for item in cast(list[object], history):
                if isinstance(item, dict):
                    history_items.append(cast(dict[str, Any], item))
            return history_items
        return []

    def list_checks(self) -> list[dict[str, Any]]:
        """
        List all available scorecard checks.

        Returns:
            List of available checks

        Example:
            >>> checks = client.scorecard.list_checks()
            >>> for check in checks:
            ...     print(f"{check['name']}: {check['description']}")
        """
        response = self.client.get("/api/v1/scorecard/checks")
        checks = response.get("checks", [])
        if isinstance(checks, list):
            check_items: list[dict[str, Any]] = []
            for item in cast(list[object], checks):
                if isinstance(item, dict):
                    check_items.append(cast(dict[str, Any], item))
            return check_items
        return []
