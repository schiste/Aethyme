"""Git operations for cloning and analyzing repositories."""

import os
import shutil
from pathlib import Path
from typing import List, Optional, Tuple
from dataclasses import dataclass
from datetime import datetime

from git import Repo, GitCommandError


@dataclass
class FileInfo:
    """Information about a file in the repository."""
    path: str
    relative_path: str
    size_bytes: int
    is_binary: bool
    extension: str


class GitRepository:
    """Wrapper for Git operations on repositories."""

    def __init__(self, clone_dir: Optional[str] = None):
        """
        Initialize Git repository handler.

        Args:
            clone_dir: Directory to clone repositories into. Defaults to /tmp/aethyme_clones
        """
        self.clone_dir = Path(clone_dir or os.getenv("GIT_CLONE_DIR", "/tmp/aethyme_clones"))
        self.clone_dir.mkdir(parents=True, exist_ok=True)

    def clone(
        self,
        repo_url: str,
        access_token: Optional[str] = None,
        branch: Optional[str] = None,
        depth: int = 1
    ) -> Tuple[Repo, Path]:
        """
        Clone a repository.

        Args:
            repo_url: Git repository URL (https://github.com/owner/repo.git)
            access_token: OAuth access token for private repositories
            branch: Specific branch to clone (default: default branch)
            depth: Clone depth (default: 1 for shallow clone)

        Returns:
            Tuple of (Repo object, Path to cloned directory)

        Raises:
            GitCommandError: If clone fails
        """
        # Create unique directory for this clone
        timestamp = datetime.utcnow().strftime("%Y%m%d_%H%M%S_%f")
        repo_name = repo_url.rstrip("/").split("/")[-1].replace(".git", "")
        target_dir = self.clone_dir / f"{repo_name}_{timestamp}"

        try:
            # Inject access token into URL for authentication
            if access_token and "github.com" in repo_url:
                # Convert https://github.com/owner/repo.git to
                # https://oauth2:TOKEN@github.com/owner/repo.git
                auth_url = repo_url.replace(
                    "https://github.com",
                    f"https://oauth2:{access_token}@github.com"
                )
            else:
                auth_url = repo_url

            # Clone repository
            clone_kwargs = {
                "depth": depth,
                "single_branch": True,
            }
            if branch:
                clone_kwargs["branch"] = branch

            repo = Repo.clone_from(auth_url, target_dir, **clone_kwargs)

            return repo, target_dir

        except GitCommandError as e:
            # Clean up on failure
            if target_dir.exists():
                shutil.rmtree(target_dir, ignore_errors=True)
            raise RuntimeError(f"Failed to clone repository: {str(e)}")

    def get_file_tree(
        self,
        repo: Repo,
        exclude_patterns: Optional[List[str]] = None
    ) -> List[FileInfo]:
        """
        Get list of files in the repository.

        Args:
            repo: Git repository object
            exclude_patterns: Patterns to exclude (e.g., .git, node_modules)

        Returns:
            List of FileInfo objects
        """
        if exclude_patterns is None:
            exclude_patterns = [
                ".git/",
                "node_modules/",
                "venv/",
                "env/",
                ".venv/",
                "__pycache__/",
                "dist/",
                "build/",
                "target/",
                ".next/",
                "coverage/",
                ".pytest_cache/",
                ".tox/",
                "vendor/",
            ]

        files = []
        repo_path = Path(repo.working_dir)

        for file_path in repo_path.rglob("*"):
            if not file_path.is_file():
                continue

            # Skip excluded patterns
            relative_path = file_path.relative_to(repo_path)
            relative_str = str(relative_path)

            if any(pattern in relative_str for pattern in exclude_patterns):
                continue

            # Determine if binary
            is_binary = self._is_binary_file(file_path)

            files.append(FileInfo(
                path=str(file_path),
                relative_path=relative_str,
                size_bytes=file_path.stat().st_size,
                is_binary=is_binary,
                extension=file_path.suffix
            ))

        return files

    def get_file_content(
        self,
        file_path: Path,
        max_size_bytes: Optional[int] = None
    ) -> Optional[str]:
        """
        Read file content with encoding detection.

        Args:
            file_path: Path to file
            max_size_bytes: Maximum file size to read (default: 10MB)

        Returns:
            File content as string, or None if file is too large or binary
        """
        if max_size_bytes is None:
            max_size_bytes = int(os.getenv("MAX_FILE_SIZE_MB", "10")) * 1024 * 1024

        try:
            file_size = file_path.stat().st_size

            # Skip if too large
            if file_size > max_size_bytes:
                return None

            # Read as bytes first
            with open(file_path, "rb") as f:
                raw_content = f.read()

            # Try to decode
            try:
                # Try UTF-8 first (most common)
                content = raw_content.decode("utf-8")
            except UnicodeDecodeError:
                # Try to detect encoding
                import chardet
                detected = chardet.detect(raw_content)
                if detected["encoding"]:
                    try:
                        content = raw_content.decode(detected["encoding"])
                    except Exception:
                        # Cannot decode, likely binary
                        return None
                else:
                    return None

            return content

        except Exception:
            return None

    def cleanup(self, target_dir: Path) -> None:
        """
        Clean up cloned repository.

        Args:
            target_dir: Directory to remove
        """
        if target_dir.exists():
            try:
                shutil.rmtree(target_dir, ignore_errors=True)
            except Exception:
                pass

    def _is_binary_file(self, file_path: Path) -> bool:
        """
        Check if file is binary.

        Args:
            file_path: Path to file

        Returns:
            True if file is binary, False otherwise
        """
        # Common binary extensions
        binary_extensions = {
            ".pyc", ".pyo", ".so", ".dylib", ".dll", ".exe",
            ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".ico",
            ".pdf", ".zip", ".tar", ".gz", ".bz2", ".xz",
            ".mp3", ".mp4", ".avi", ".mov", ".wmv",
            ".woff", ".woff2", ".ttf", ".eot",
            ".class", ".jar", ".war", ".ear",
            ".o", ".a", ".lib",
        }

        if file_path.suffix.lower() in binary_extensions:
            return True

        # Try reading first few bytes
        try:
            with open(file_path, "rb") as f:
                chunk = f.read(1024)

            # Check for null bytes (common in binary files)
            if b"\x00" in chunk:
                return True

            # Try to decode as text
            try:
                chunk.decode("utf-8")
                return False
            except UnicodeDecodeError:
                return True

        except Exception:
            return True

    def get_repository_stats(self, repo: Repo) -> dict:
        """
        Get repository statistics.

        Args:
            repo: Git repository object

        Returns:
            Dictionary with repository stats
        """
        try:
            # Get latest commit
            latest_commit = repo.head.commit

            # Count commits (limited to avoid performance issues)
            commit_count = sum(1 for _ in repo.iter_commits(max_count=1000))

            # Get branches
            branches = [branch.name for branch in repo.branches]

            # Get remote URL
            try:
                remote_url = repo.remotes.origin.url
            except Exception:
                remote_url = None

            return {
                "latest_commit": {
                    "sha": latest_commit.hexsha,
                    "author": latest_commit.author.name,
                    "date": latest_commit.committed_datetime.isoformat(),
                    "message": latest_commit.message.strip(),
                },
                "commit_count": commit_count if commit_count < 1000 else "1000+",
                "branches": branches,
                "default_branch": repo.active_branch.name,
                "remote_url": remote_url,
            }

        except Exception as e:
            return {"error": str(e)}
