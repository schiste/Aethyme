"""Repository indexing background tasks."""

import time
from typing import Dict, Any
from celery import Task
from datetime import datetime

from app.core.celery_app import celery_app
from app.core.database import get_db
from app.models.repository import Repository
from app.core.git import GitRepository
from app.core.elasticsearch import CodeIndexer
from app.indexing.parser import FileParser


class DatabaseTask(Task):
    """Base task with database session support."""

    _db = None

    @property
    def db(self):
        if self._db is None:
            self._db = next(get_db())
        return self._db


@celery_app.task(bind=True, base=DatabaseTask, name="app.tasks.indexing.index_repository")
def index_repository(self, repository_id: str) -> Dict[str, Any]:
    """
    Index a repository's contents into Elasticsearch.

    This task:
    1. Fetches repository metadata from database
    2. Clones the repository (shallow clone)
    3. Extracts files and metadata
    4. Indexes content into Elasticsearch
    5. Updates repository record with stats

    Args:
        repository_id: UUID of the repository to index

    Returns:
        dict: Indexing results with stats

    Example:
        >>> result = index_repository.delay("abc-123")
        >>> result.get(timeout=300)
        {
            'repository_id': 'abc-123',
            'files_indexed': 142,
            'total_lines': 5834,
            'duration': 23.4
        }
    """
    start_time = time.time()
    git_repo_handler = GitRepository()
    code_indexer = CodeIndexer()
    file_parser = FileParser()
    repo_obj = None
    target_dir = None

    try:
        # Progress: 0% - Starting
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 0,
                "status": "Initializing...",
            },
        )

        # Progress: 10% - Fetch repository from database
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 10,
                "status": "Fetching repository metadata...",
            },
        )

        # Fetch repository (synchronous for now)
        from sqlalchemy import select
        result = self.db.execute(
            select(Repository).where(Repository.id == repository_id)
        )
        repo = result.scalar_one_or_none()

        if not repo:
            raise ValueError(f"Repository {repository_id} not found")

        # Progress: 20% - Clone repository
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 20,
                "status": f"Cloning repository from {repo.url}...",
            },
        )

        # Clone the repository
        # TODO: Get access token from GitHub account associated with user
        repo_obj, target_dir = git_repo_handler.clone(
            repo_url=repo.url,
            access_token=None,  # For public repos
            branch=None,  # Use default branch
            depth=1  # Shallow clone
        )

        # Progress: 40% - Get file tree
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 40,
                "status": "Scanning files...",
            },
        )

        file_infos = git_repo_handler.get_file_tree(repo_obj)

        # Progress: 50% - Parse files
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 50,
                "status": f"Parsing {len(file_infos)} files...",
            },
        )

        parsed_files = file_parser.parse_files(file_infos)

        # Progress: 60% - Calculate stats
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 60,
                "status": "Analyzing code...",
            },
        )

        repo_stats = file_parser.calculate_repository_stats(parsed_files)

        # Progress: 70% - Create Elasticsearch index
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 70,
                "status": "Creating search index...",
            },
        )

        code_indexer.create_index(repository_id)

        # Progress: 80% - Index files to Elasticsearch
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 80,
                "status": "Indexing code to search engine...",
            },
        )

        # Prepare files for indexing
        files_to_index = []
        for pf in parsed_files:
            files_to_index.append({
                "file_path": pf.relative_path,
                "file_name": pf.relative_path.split("/")[-1],
                "language": pf.language,
                "extension": pf.extension,
                "content": pf.content,
                "line_count": pf.line_count,
                "code_lines": pf.code_lines,
                "size_bytes": pf.size_bytes,
                "content_hash": pf.content_hash,
                "indexed_at": datetime.utcnow().isoformat(),
            })

        # Bulk index
        index_result = code_indexer.bulk_index_files(repository_id, files_to_index)

        # Progress: 90% - Update repository stats in database
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 90,
                "status": "Updating repository statistics...",
            },
        )

        # Update repository record
        repo.total_files = repo_stats["total_files"]
        repo.total_lines = repo_stats["total_lines"]
        repo.languages = repo_stats["languages"]
        repo.indexed_at = datetime.utcnow()
        self.db.commit()

        # Progress: 100% - Cleanup
        self.update_state(
            state="INDEXING",
            meta={
                "repository_id": repository_id,
                "progress": 100,
                "status": "Cleaning up...",
            },
        )

        # Cleanup cloned repository
        if target_dir:
            git_repo_handler.cleanup(target_dir)

        duration = time.time() - start_time

        # Return success result
        return {
            "repository_id": repository_id,
            "status": "completed",
            "files_indexed": repo_stats["total_files"],
            "total_lines": repo_stats["total_lines"],
            "languages": repo_stats["languages"],
            "primary_language": repo_stats["primary_language"],
            "duration": round(duration, 2),
            "indexed_at": datetime.utcnow().isoformat(),
            "elasticsearch_success": index_result["success"],
            "elasticsearch_failed": index_result["failed"],
        }

    except Exception as e:
        # Cleanup on failure
        if target_dir:
            try:
                git_repo_handler.cleanup(target_dir)
            except Exception:
                pass

        # Update state to failed
        self.update_state(
            state="FAILURE",
            meta={
                "repository_id": repository_id,
                "error": str(e),
                "status": "Indexing failed",
            },
        )
        raise


@celery_app.task(name="app.tasks.indexing.update_repository_stats")
def update_repository_stats(repository_id: str) -> Dict[str, Any]:
    """
    Update repository statistics without full reindexing.

    Updates:
    - File count
    - Line count
    - Language statistics
    - Last indexed timestamp

    Args:
        repository_id: UUID of the repository

    Returns:
        dict: Updated statistics
    """
    # Mock implementation
    return {
        "repository_id": repository_id,
        "status": "updated",
        "file_count": 142,
        "line_count": 5834,
    }


@celery_app.task(name="app.tasks.indexing.delete_repository_index")
def delete_repository_index(repository_id: str) -> Dict[str, Any]:
    """
    Delete repository index from Elasticsearch.

    Called when a repository is deleted from the system.

    Args:
        repository_id: UUID of the repository

    Returns:
        dict: Deletion result
    """
    # Mock implementation
    return {
        "repository_id": repository_id,
        "status": "deleted",
        "documents_removed": 142,
    }
