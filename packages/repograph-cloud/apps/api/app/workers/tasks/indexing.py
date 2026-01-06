"""Celery tasks for repository indexing."""

from typing import List, Optional
from celery import Task
from sqlalchemy.orm import Session

from app.workers.celery_app import celery_app
from app.services.git import GitService
from app.services.tree_sitter import TreeSitterParser
from app.services.scip import SCIPGenerator
from app.services.elasticsearch import ElasticsearchService
from app.models.repository import Repository
from app.core.database import get_db


class IndexingTask(Task):
    """Base task with database session management."""

    _git_service = None
    _parser = None
    _elasticsearch_service = None

    @property
    def git_service(self):
        if self._git_service is None:
            self._git_service = GitService()
        return self._git_service

    @property
    def parser(self):
        if self._parser is None:
            self._parser = TreeSitterParser()
        return self._parser

    @property
    def elasticsearch_service(self):
        if self._elasticsearch_service is None:
            self._elasticsearch_service = ElasticsearchService()
        return self._elasticsearch_service


@celery_app.task(bind=True, base=IndexingTask, name="app.workers.tasks.indexing.index_repository")
async def index_repository_task(self, repository_id: str, full_reindex: bool = True):
    """Index a repository (clone and parse all files).

    Args:
        repository_id: Repository UUID
        full_reindex: If True, delete and re-index everything. If False, incremental update.

    Returns:
        Dict with indexing statistics
    """
    print(f"[{self.request.id}] Starting indexing for repository {repository_id}")

    db: Session = next(get_db())
    try:
        # Get repository from database
        repository = db.query(Repository).filter(Repository.id == repository_id).first()
        if not repository:
            raise ValueError(f"Repository {repository_id} not found")

        # Update status to indexing
        repository.status = "indexing"
        db.commit()

        # Step 0: If full reindex, delete existing symbols from Elasticsearch
        if full_reindex:
            print(f"[{self.request.id}] Deleting existing symbols from Elasticsearch")
            await self.elasticsearch_service.delete_repository_symbols(repository_id)

        # Step 1: Clone repository
        print(f"[{self.request.id}] Cloning repository from {repository.clone_url}")
        repo_path = self.git_service.clone_repository(
            repository_id=repository_id,
            clone_url=repository.clone_url,
            access_token=_get_access_token(db, repository),
            branch=repository.default_branch or "main"
        )

        # Step 2: Get all files to index
        print(f"[{self.request.id}] Discovering files to index")
        # Index code files only (filter by extension)
        code_extensions = [
            '.py', '.js', '.ts', '.tsx', '.jsx',
            '.go', '.rs', '.java', '.kt', '.swift',
            '.c', '.cpp', '.h', '.hpp', '.cs',
            '.rb', '.php', '.scala', '.clj',
            '.sh', '.bash', '.sql', '.graphql'
        ]
        files_to_index = self.git_service.get_all_files(
            repository_id,
            extensions=code_extensions
        )

        print(f"[{self.request.id}] Found {len(files_to_index)} files to index")

        # Step 3: Initialize SCIP generator
        scip_generator = SCIPGenerator(
            repository_id=repository_id,
            repository_name=repository.full_name
        )

        # Step 4: Parse and index files
        indexed_files = 0
        indexed_symbols = 0

        for file_path in files_to_index:
            try:
                # Get file content
                content = self.git_service.get_file_content(repository_id, file_path)

                # Parse with Tree-sitter (Phase 6 Day 3-4) ✅
                symbols = self.parser.extract_symbols(file_path, content)
                indexed_symbols += len(symbols)

                print(f"[{self.request.id}] Extracted {len(symbols)} symbols from {file_path}")

                # Generate SCIP index (Phase 6 Day 5-6) ✅
                language = self.parser.get_language(file_path)
                if language and symbols:
                    scip_doc = scip_generator.add_file(file_path, language, symbols)
                    print(f"[{self.request.id}] Added {file_path} to SCIP index")

                    # Index in Elasticsearch (Phase 6 Day 7-8) ✅
                    es_result = await self.elasticsearch_service.index_scip_document(
                        repository_id=repository_id,
                        repository_name=repository.full_name,
                        scip_doc=scip_doc
                    )
                    print(f"[{self.request.id}] Indexed in Elasticsearch: {es_result['success']} symbols")

                indexed_files += 1

                # Update progress every 10 files
                if indexed_files % 10 == 0:
                    self.update_state(
                        state='PROGRESS',
                        meta={
                            'current': indexed_files,
                            'total': len(files_to_index),
                            'symbols': indexed_symbols,
                            'status': f'Indexing {file_path}'
                        }
                    )

            except Exception as e:
                print(f"[{self.request.id}] Error indexing {file_path}: {e}")
                # Continue with next file
                continue

        # Step 5: Get SCIP statistics
        scip_stats = scip_generator.get_statistics()

        print(f"[{self.request.id}] SCIP Index Statistics:")
        print(f"  - Documents: {scip_stats['total_documents']}")
        print(f"  - Symbols: {scip_stats['total_symbols']}")
        print(f"  - By kind: {scip_stats['symbols_by_kind']}")

        # Step 6: Get Elasticsearch statistics
        es_stats = await self.elasticsearch_service.get_repository_statistics(repository_id)

        print(f"[{self.request.id}] Elasticsearch Statistics:")
        print(f"  - Total symbols: {es_stats['total_symbols']}")
        print(f"  - By kind: {es_stats['by_kind']}")
        print(f"  - By language: {es_stats['by_language']}")

        # Step 7: Update repository status and statistics
        repository.status = "active"
        repository.indexed_at = db.func.now()
        repository.total_files = indexed_files
        repository.total_lines = 0  # TODO: Count lines
        repository.languages = scip_stats['files_by_language']
        db.commit()

        print(f"[{self.request.id}] Indexing complete: {indexed_files} files, {indexed_symbols} symbols")

        return {
            "repository_id": repository_id,
            "status": "success",
            "files_indexed": indexed_files,
            "symbols_extracted": indexed_symbols,
            "scip_statistics": scip_stats,
            "elasticsearch_statistics": es_stats,
        }

    except Exception as e:
        # Update status to error
        repository.status = "error"
        repository.last_error = str(e)
        db.commit()

        print(f"[{self.request.id}] Indexing failed: {e}")
        raise

    finally:
        db.close()


@celery_app.task(bind=True, base=IndexingTask, name="app.workers.tasks.indexing.reindex_files")
async def reindex_files_task(
    self,
    repository_id: str,
    file_paths: List[str],
    deleted_file_paths: Optional[List[str]] = None
):
    """Re-index specific files in a repository (for webhook updates).

    Args:
        repository_id: Repository UUID
        file_paths: List of file paths to re-index
        deleted_file_paths: List of file paths that were deleted

    Returns:
        Dict with indexing statistics
    """
    deleted_file_paths = deleted_file_paths or []
    print(f"[{self.request.id}] Re-indexing {len(file_paths)} files, deleting {len(deleted_file_paths)} files in repository {repository_id}")

    db: Session = next(get_db())
    try:
        # Get repository from database
        repository = db.query(Repository).filter(Repository.id == repository_id).first()
        if not repository:
            raise ValueError(f"Repository {repository_id} not found")

        # Pull latest changes
        try:
            self.git_service.pull_updates(repository_id)
            print(f"[{self.request.id}] Pulled latest changes from remote")
        except Exception as e:
            print(f"[{self.request.id}] Warning: Failed to pull updates: {e}")
            # Continue anyway - webhook might have file list

        # Initialize SCIP generator
        scip_generator = SCIPGenerator(
            repository_id=repository_id,
            repository_name=repository.full_name
        )

        # Re-index changed/added files
        indexed_files = 0
        indexed_symbols = 0

        for file_path in file_paths:
            try:
                # Get file content
                content = self.git_service.get_file_content(repository_id, file_path)

                # Parse with Tree-sitter
                symbols = self.parser.extract_symbols(file_path, content)
                indexed_symbols += len(symbols)

                print(f"[{self.request.id}] Extracted {len(symbols)} symbols from {file_path}")

                # Generate SCIP document
                language = self.parser.get_language(file_path)
                if language and symbols:
                    scip_doc = scip_generator.add_file(file_path, language, symbols)

                    # First, delete existing symbols for this file from Elasticsearch
                    # (to handle deleted/renamed symbols)
                    # Build a query to find all symbols in this file
                    # For now, we'll rely on symbol IDs being unique

                    # Index in Elasticsearch
                    es_result = await self.elasticsearch_service.index_scip_document(
                        repository_id=repository_id,
                        repository_name=repository.full_name,
                        scip_doc=scip_doc
                    )
                    print(f"[{self.request.id}] Indexed {es_result['success']} symbols in Elasticsearch")

                indexed_files += 1

            except FileNotFoundError:
                # File was deleted, will be handled below
                print(f"[{self.request.id}] File not found: {file_path} (might be deleted)")
                if file_path not in deleted_file_paths:
                    deleted_file_paths.append(file_path)
            except Exception as e:
                print(f"[{self.request.id}] Error re-indexing {file_path}: {e}")
                continue

        # Handle deleted files
        deleted_symbols = 0
        for file_path in deleted_file_paths:
            try:
                # Delete all symbols from this file
                deleted = await self.elasticsearch_service.delete_file_symbols(
                    repository_id=repository_id,
                    file_path=file_path
                )
                deleted_symbols += deleted
                print(f"[{self.request.id}] Deleted {deleted} symbols from {file_path}")

            except Exception as e:
                print(f"[{self.request.id}] Error deleting symbols for {file_path}: {e}")
                continue

        # Update last_synced timestamp
        repository.last_synced_at = db.func.now()
        db.commit()

        print(f"[{self.request.id}] Re-indexing complete: {indexed_files} files, {indexed_symbols} symbols, {deleted_symbols} symbols deleted")

        return {
            "repository_id": repository_id,
            "status": "success",
            "files_reindexed": indexed_files,
            "symbols_indexed": indexed_symbols,
            "symbols_deleted": deleted_symbols,
            "files_deleted": len(deleted_file_paths),
        }

    except Exception as e:
        print(f"[{self.request.id}] Re-indexing failed: {e}")
        raise

    finally:
        db.close()


def _get_access_token(db: Session, repository: Repository) -> Optional[str]:
    """Get decrypted OAuth access token for repository's owner.

    Args:
        db: Database session
        repository: Repository model

    Returns:
        Decrypted access token or None
    """
    # Get user's OAuth token based on provider
    user = repository.organization.users[0] if repository.organization.users else None
    if not user:
        return None

    provider = repository.provider.lower()
    if provider == "github" and user.github_access_token:
        # TODO: Decrypt token (currently stored encrypted)
        return user.github_access_token
    elif provider == "gitlab" and user.gitlab_access_token:
        return user.gitlab_access_token
    elif provider == "bitbucket" and user.bitbucket_access_token:
        return user.bitbucket_access_token

    return None
