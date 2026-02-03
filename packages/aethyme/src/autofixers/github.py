"""GitHub integration for creating pull requests with autofixes."""

import subprocess
from pathlib import Path
from typing import Dict, List, Optional
from datetime import datetime
import structlog

from .patch import PatchGenerator

logger = structlog.get_logger()


class GitHubIntegration:
    """Manages GitHub integration for autofix pull requests."""

    def __init__(self, repo_path: Path, use_gh_cli: bool = True):
        self.repo_path = Path(repo_path)
        self.use_gh_cli = use_gh_cli
        self.branch_prefix = "autofix"

    def create_branch(self, branch_name: Optional[str] = None) -> str:
        """Create a new branch for autofixes."""
        if branch_name is None:
            timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
            branch_name = f"{self.branch_prefix}/{timestamp}"

        try:
            # Create and checkout new branch
            subprocess.run(
                ["git", "checkout", "-b", branch_name],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
            )

            logger.info("Created branch", branch=branch_name)
            return branch_name

        except subprocess.CalledProcessError as e:
            logger.error("Failed to create branch", error=str(e))
            raise

    def commit_changes(
        self,
        message: str,
        files: Optional[List[Path]] = None,
    ) -> str:
        """Commit changes to current branch."""
        try:
            # Add files
            if files:
                for file_path in files:
                    subprocess.run(
                        ["git", "add", str(file_path)],
                        cwd=self.repo_path,
                        check=True,
                        capture_output=True,
                    )
            else:
                # Add all changes
                subprocess.run(
                    ["git", "add", "-A"],
                    cwd=self.repo_path,
                    check=True,
                    capture_output=True,
                )

            # Commit
            subprocess.run(
                ["git", "commit", "-m", message],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
            )

            # Get commit SHA
            result = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
                text=True,
            )

            commit_sha = result.stdout.strip()
            logger.info("Created commit", sha=commit_sha[:8])
            return commit_sha

        except subprocess.CalledProcessError as e:
            logger.error("Failed to commit", error=str(e))
            raise

    def push_branch(self, branch_name: str, set_upstream: bool = True) -> bool:
        """Push branch to remote."""
        try:
            cmd = ["git", "push"]
            if set_upstream:
                cmd.extend(["-u", "origin", branch_name])
            else:
                cmd.append(branch_name)

            subprocess.run(
                cmd,
                cwd=self.repo_path,
                check=True,
                capture_output=True,
            )

            logger.info("Pushed branch", branch=branch_name)
            return True

        except subprocess.CalledProcessError as e:
            logger.error("Failed to push branch", error=str(e))
            return False

    def create_pull_request(
        self,
        title: str,
        body: str,
        base_branch: str = "main",
        head_branch: Optional[str] = None,
        labels: Optional[List[str]] = None,
        reviewers: Optional[List[str]] = None,
    ) -> Optional[Dict[str, any]]:
        """Create a pull request using GitHub CLI."""
        if not self.use_gh_cli:
            logger.warning("GitHub CLI not enabled")
            return None

        try:
            # Build gh pr create command
            cmd = [
                "gh", "pr", "create",
                "--title", title,
                "--body", body,
                "--base", base_branch,
            ]

            if head_branch:
                cmd.extend(["--head", head_branch])

            if labels:
                cmd.extend(["--label", ",".join(labels)])

            if reviewers:
                cmd.extend(["--reviewer", ",".join(reviewers)])

            # Create PR
            result = subprocess.run(
                cmd,
                cwd=self.repo_path,
                check=True,
                capture_output=True,
                text=True,
            )

            pr_url = result.stdout.strip()
            logger.info("Created pull request", url=pr_url)

            # Get PR number from URL
            pr_number = pr_url.split('/')[-1]

            return {
                "url": pr_url,
                "number": pr_number,
                "title": title,
            }

        except subprocess.CalledProcessError as e:
            logger.error("Failed to create PR", error=str(e))
            return None

    def create_autofix_pr(
        self,
        patch_generator: PatchGenerator,
        base_branch: str = "main",
        labels: Optional[List[str]] = None,
    ) -> Optional[Dict[str, any]]:
        """Create a complete autofix PR workflow."""
        summary = patch_generator.get_summary()

        # Create branch
        branch_name = self.create_branch()

        # Commit message
        commit_message = patch_generator.create_commit_message()

        # Apply patches and commit
        try:
            result = patch_generator.apply(skip_approval=False)

            if result["status"] == "requires_approval":
                logger.warning("Patches require approval", result=result)
                return {
                    "status": "requires_approval",
                    "branch": branch_name,
                    "result": result,
                }

            if result["status"] != "success":
                logger.error("Failed to apply patches", result=result)
                return None

            # Commit changes
            changed_files = patch_generator.get_changed_files()
            commit_sha = self.commit_changes(commit_message, changed_files)

            # Push branch
            if not self.push_branch(branch_name):
                return None

            # Create PR
            pr_title = self._generate_pr_title(summary)
            pr_body = self._generate_pr_body(summary, patch_generator)

            pr_labels = labels or ["autofix", "automated"]

            pr_info = self.create_pull_request(
                title=pr_title,
                body=pr_body,
                base_branch=base_branch,
                head_branch=branch_name,
                labels=pr_labels,
            )

            if pr_info:
                pr_info.update({
                    "branch": branch_name,
                    "commit": commit_sha,
                    "summary": summary,
                })

            return pr_info

        except Exception as e:
            logger.error("Failed to create autofix PR", error=str(e))
            # Cleanup: delete branch
            try:
                subprocess.run(
                    ["git", "checkout", base_branch],
                    cwd=self.repo_path,
                    capture_output=True,
                )
                subprocess.run(
                    ["git", "branch", "-D", branch_name],
                    cwd=self.repo_path,
                    capture_output=True,
                )
            except Exception:
                pass

            return None

    def _generate_pr_title(self, summary: Dict[str, any]) -> str:
        """Generate PR title from summary."""
        total = summary["total_files"]
        fix_types = list(summary["by_fix_type"].keys())

        if len(fix_types) == 1:
            return f"fix: apply {fix_types[0]} to {total} files"
        else:
            return f"fix: apply autofixes to {total} files"

    def _generate_pr_body(
        self,
        summary: Dict[str, any],
        patch_generator: PatchGenerator
    ) -> str:
        """Generate PR body from summary."""
        lines = [
            "## Autofix Summary",
            "",
            f"This PR applies automated fixes to {summary['total_files']} files.",
            "",
            "### Changes by Type",
            "",
        ]

        # List changes by fix type
        for fix_type, count in summary["by_fix_type"].items():
            lines.append(f"- **{fix_type}**: {count} files")

        lines.extend([
            "",
            "### Risk Assessment",
            "",
            f"- Low risk: {summary['total_low_risk']} files",
            f"- Medium risk: {summary['total_medium_risk']} files",
            f"- High risk: {summary['total_high_risk']} files",
            "",
        ])

        # Add file list
        lines.extend([
            "### Files Modified",
            "",
            "<details>",
            "<summary>Click to expand</summary>",
            "",
        ])

        for patch in patch_generator.patches:
            lines.append(f"- `{patch.file_path}` ({patch.fix_type}, {patch.risk_level.value})")

        lines.extend([
            "",
            "</details>",
            "",
            "---",
            "",
            "**Generated by:** Aethyme Autofixer",
            "**Safety checks:** Enabled",
            "**Generated files:** Skipped",
            "",
            "Please review the changes and merge if they look correct.",
        ])

        return "\n".join(lines)

    def get_current_branch(self) -> str:
        """Get current branch name."""
        try:
            result = subprocess.run(
                ["git", "rev-parse", "--abbrev-ref", "HEAD"],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
                text=True,
            )
            return result.stdout.strip()
        except subprocess.CalledProcessError as e:
            logger.error("Failed to get current branch", error=str(e))
            return ""

    def is_clean_working_tree(self) -> bool:
        """Check if working tree is clean."""
        try:
            result = subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
                text=True,
            )
            return len(result.stdout.strip()) == 0
        except subprocess.CalledProcessError:
            return False

    def get_remote_info(self) -> Optional[Dict[str, str]]:
        """Get GitHub repository info."""
        try:
            result = subprocess.run(
                ["git", "remote", "get-url", "origin"],
                cwd=self.repo_path,
                check=True,
                capture_output=True,
                text=True,
            )

            remote_url = result.stdout.strip()

            # Parse GitHub URL
            # git@github.com:owner/repo.git or https://github.com/owner/repo.git
            import re

            match = re.search(r'github\.com[:/]([^/]+)/([^/.]+)', remote_url)
            if match:
                return {
                    "owner": match.group(1),
                    "repo": match.group(2),
                    "url": remote_url,
                }

        except subprocess.CalledProcessError:
            pass

        return None
