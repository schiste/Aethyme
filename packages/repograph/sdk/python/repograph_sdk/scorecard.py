"""Scorecard API for RepoGraph SDK."""

from typing import List, Optional, TYPE_CHECKING
from .models import ScorecardResult

if TYPE_CHECKING:
    from .client import RepoGraphClient


class ScorecardAPI:
    """
    Scorecard API interface.

    Provides methods for running AI-readiness scorecards.
    """

    def __init__(self, client: "RepoGraphClient"):
        self.client = client

    def scan(
        self,
        repo_id: str,
        checks: Optional[List[str]] = None,
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
        data = {
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
    ) -> List[dict]:
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
        return response.get("history", [])

    def list_checks(self) -> List[dict]:
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
        return response.get("checks", [])
