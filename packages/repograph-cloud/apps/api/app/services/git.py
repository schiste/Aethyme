"""Git operations service for repository cloning and updating."""

import os
import shutil
from pathlib import Path
from typing import List, Optional
import git
from git import Repo
from app.core.config import settings


class GitService:
    """Service for Git repository operations."""

    def __init__(self, repos_base_path: str = "/tmp/repograph/repos"):
        """Initialize Git service.

        Args:
            repos_base_path: Base directory for cloned repositories
        """
        self.repos_base_path = Path(repos_base_path)
        self.repos_base_path.mkdir(parents=True, exist_ok=True)

    def get_repo_path(self, repository_id: str) -> Path:
        """Get local path for a repository.

        Args:
            repository_id: Repository UUID

        Returns:
            Path to repository directory
        """
        return self.repos_base_path / repository_id

    def clone_repository(
        self,
        repository_id: str,
        clone_url: str,
        access_token: Optional[str] = None,
        branch: str = "main"
    ) -> Path:
        """Clone a repository from remote.

        Args:
            repository_id: Repository UUID
            clone_url: Git clone URL (HTTPS)
            access_token: OAuth access token for authentication
            branch: Branch to clone (default: main)

        Returns:
            Path to cloned repository

        Raises:
            git.GitCommandError: If clone fails
        """
        repo_path = self.get_repo_path(repository_id)

        # Remove existing directory if it exists
        if repo_path.exists():
            shutil.rmtree(repo_path)

        # Inject access token into clone URL for authentication
        if access_token:
            # Convert https://github.com/user/repo.git
            # to https://oauth2:TOKEN@github.com/user/repo.git
            if clone_url.startswith("https://"):
                parts = clone_url.replace("https://", "").split("/", 1)
                clone_url = f"https://oauth2:{access_token}@{parts[0]}/{parts[1]}"

        # Clone repository
        repo = Repo.clone_from(
            clone_url,
            repo_path,
            branch=branch,
            depth=1,  # Shallow clone for speed (only latest commit)
        )

        return repo_path

    def pull_updates(self, repository_id: str) -> List[str]:
        """Pull latest updates from remote.

        Args:
            repository_id: Repository UUID

        Returns:
            List of changed file paths

        Raises:
            git.GitCommandError: If pull fails
        """
        repo_path = self.get_repo_path(repository_id)

        if not repo_path.exists():
            raise ValueError(f"Repository {repository_id} not cloned yet")

        repo = Repo(repo_path)

        # Get commit before pull
        old_commit = repo.head.commit

        # Pull latest changes
        origin = repo.remotes.origin
        origin.pull()

        # Get commit after pull
        new_commit = repo.head.commit

        # Get changed files between commits
        changed_files = []
        if old_commit != new_commit:
            diffs = old_commit.diff(new_commit)
            changed_files = [diff.a_path for diff in diffs]

        return changed_files

    def get_changed_files(
        self,
        repository_id: str,
        since_commit: Optional[str] = None
    ) -> List[str]:
        """Get list of changed files since a commit.

        Args:
            repository_id: Repository UUID
            since_commit: Commit SHA to compare from (default: previous commit)

        Returns:
            List of changed file paths
        """
        repo_path = self.get_repo_path(repository_id)

        if not repo_path.exists():
            raise ValueError(f"Repository {repository_id} not cloned yet")

        repo = Repo(repo_path)

        if since_commit:
            # Compare with specific commit
            old_commit = repo.commit(since_commit)
            new_commit = repo.head.commit
        else:
            # Compare with previous commit
            commits = list(repo.iter_commits(max_count=2))
            if len(commits) < 2:
                # Only one commit, return all files
                return self.get_all_files(repository_id)
            old_commit = commits[1]
            new_commit = commits[0]

        # Get changed files
        diffs = old_commit.diff(new_commit)
        return [diff.a_path for diff in diffs]

    def get_all_files(self, repository_id: str, extensions: Optional[List[str]] = None) -> List[str]:
        """Get all files in repository.

        Args:
            repository_id: Repository UUID
            extensions: Filter by file extensions (e.g., ['.py', '.ts'])

        Returns:
            List of all file paths
        """
        repo_path = self.get_repo_path(repository_id)

        if not repo_path.exists():
            raise ValueError(f"Repository {repository_id} not cloned yet")

        all_files = []
        for root, dirs, files in os.walk(repo_path):
            # Skip .git directory
            if '.git' in dirs:
                dirs.remove('.git')

            # Skip node_modules, venv, etc.
            skip_dirs = ['node_modules', 'venv', '__pycache__', '.next', 'dist', 'build']
            for skip_dir in skip_dirs:
                if skip_dir in dirs:
                    dirs.remove(skip_dir)

            for file in files:
                file_path = os.path.join(root, file)
                rel_path = os.path.relpath(file_path, repo_path)

                # Filter by extensions if provided
                if extensions:
                    if not any(rel_path.endswith(ext) for ext in extensions):
                        continue

                all_files.append(rel_path)

        return all_files

    def get_file_content(self, repository_id: str, file_path: str) -> str:
        """Get content of a file.

        Args:
            repository_id: Repository UUID
            file_path: Relative path to file

        Returns:
            File content as string
        """
        repo_path = self.get_repo_path(repository_id)
        full_path = repo_path / file_path

        if not full_path.exists():
            raise FileNotFoundError(f"File {file_path} not found")

        with open(full_path, 'r', encoding='utf-8', errors='ignore') as f:
            return f.read()

    def delete_repository(self, repository_id: str):
        """Delete cloned repository from disk.

        Args:
            repository_id: Repository UUID
        """
        repo_path = self.get_repo_path(repository_id)

        if repo_path.exists():
            shutil.rmtree(repo_path)
