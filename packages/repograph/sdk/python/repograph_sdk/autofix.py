"""Autofix API for RepoGraph SDK."""

from typing import List, Optional, TYPE_CHECKING
from .models import AutofixResult

if TYPE_CHECKING:
    from .client import RepoGraphClient


class AutofixAPI:
    """
    Autofix API interface.

    Provides methods for running automatic code fixes.
    """

    def __init__(self, client: "RepoGraphClient"):
        self.client = client

    def run(
        self,
        repo_id: str,
        fix_types: Optional[List[str]] = None,
        dry_run: bool = True,
        skip_generated: bool = True,
    ) -> AutofixResult:
        """
        Run autofixes on a repository.

        Args:
            repo_id: Repository ID
            fix_types: Specific fix types to apply
            dry_run: Run in dry-run mode
            skip_generated: Skip generated files

        Returns:
            Autofix result

        Example:
            >>> result = client.autofix.run(repo_id="abc123", dry_run=True)
            >>> print(f"Found {len(result.fixes)} potential fixes")
            >>> for fix in result.fixes:
            ...     print(f"{fix.type} in {fix.file}:{fix.line}")
        """
        data = {
            "repo_id": repo_id,
            "dry_run": dry_run,
            "skip_generated": skip_generated,
        }
        if fix_types:
            data["fix_types"] = fix_types

        response = self.client.post("/api/v1/autofix/run", json=data)
        return AutofixResult(**response)

    def apply(
        self,
        repo_id: str,
        fix_ids: List[str],
        create_pr: bool = False,
        branch_name: Optional[str] = None,
    ) -> dict:
        """
        Apply selected fixes.

        Args:
            repo_id: Repository ID
            fix_ids: Fix IDs to apply
            create_pr: Create pull request
            branch_name: Branch name for PR

        Returns:
            Application result

        Example:
            >>> result = client.autofix.apply(
            ...     repo_id="abc123",
            ...     fix_ids=["fix-1", "fix-2"],
            ...     create_pr=True
            ... )
            >>> print(f"PR created: {result['pull_request']['url']}")
        """
        data = {
            "repo_id": repo_id,
            "fix_ids": fix_ids,
            "create_pr": create_pr,
        }
        if branch_name:
            data["branch_name"] = branch_name

        return self.client.post("/api/v1/autofix/apply", json=data)

    def list_types(self) -> List[dict]:
        """
        List available autofix types.

        Returns:
            List of fix types

        Example:
            >>> types = client.autofix.list_types()
            >>> for fix_type in types:
            ...     print(f"{fix_type['name']}: {fix_type['description']}")
        """
        response = self.client.get("/api/v1/autofix/types")
        return response.get("fix_types", [])

    def get_history(
        self,
        repo_id: str,
        limit: int = 10,
    ) -> List[dict]:
        """
        Get autofix history for a repository.

        Args:
            repo_id: Repository ID
            limit: Maximum number of results

        Returns:
            List of historical autofix runs

        Example:
            >>> history = client.autofix.get_history(repo_id="abc123")
            >>> for run in history:
            ...     print(f"Applied {run['fixes_applied']} fixes")
        """
        response = self.client.get(
            f"/api/v1/autofix/history/{repo_id}",
            params={"limit": limit},
        )
        return response.get("history", [])
