"""
Celery Tasks for Embedding Generation

Background tasks for generating vector embeddings using customer AI keys.
"""

import asyncio
from typing import List, Optional
from celery import Task
from celery.utils.log import get_task_logger

from app.core.celery_app import celery_app
from app.core.database import async_session_maker
from app.services.embeddings_service import EmbeddingsService
from app.services.elasticsearch import ElasticsearchService
from app.services.ai import AIProviderType, EmbeddingModel


logger = get_task_logger(__name__)


class EmbeddingTask(Task):
    """Base task for embedding generation with error handling."""

    autoretry_for = (Exception,)
    retry_kwargs = {"max_retries": 3, "countdown": 60}
    retry_backoff = True
    retry_backoff_max = 600  # 10 minutes
    retry_jitter = True


@celery_app.task(bind=True, base=EmbeddingTask, name="embeddings.generate_for_repository")
def generate_embeddings_for_repository_task(
    self,
    user_id: int,
    repository_id: str,
    provider_type: str = "openai",
    model: str = "text-embedding-3-small",
    credential_id: Optional[int] = None,
    force_regenerate: bool = False,
):
    """
    Generate embeddings for all symbols in a repository.

    Args:
        user_id: User ID (owner of repository)
        repository_id: Repository ID
        provider_type: AI provider type (openai, claude, azure_openai)
        model: Embedding model name
        credential_id: Specific credential ID to use (optional)
        force_regenerate: If True, delete existing embeddings first

    Returns:
        Dictionary with results
    """
    logger.info(
        f"Starting embedding generation for repository {repository_id} "
        f"(user={user_id}, provider={provider_type}, model={model})"
    )

    try:
        # Run async function in event loop
        result = asyncio.run(
            _generate_embeddings_for_repository(
                user_id=user_id,
                repository_id=repository_id,
                provider_type=AIProviderType(provider_type),
                model=EmbeddingModel(model),
                credential_id=credential_id,
                force_regenerate=force_regenerate,
            )
        )

        logger.info(
            f"Completed embedding generation for repository {repository_id}: "
            f"{result['embeddings_created']} embeddings created"
        )

        return result

    except Exception as e:
        logger.error(
            f"Error generating embeddings for repository {repository_id}: {str(e)}",
            exc_info=True,
        )
        raise


@celery_app.task(bind=True, base=EmbeddingTask, name="embeddings.generate_for_files")
def generate_embeddings_for_files_task(
    self,
    user_id: int,
    repository_id: str,
    file_paths: List[str],
    provider_type: str = "openai",
    model: str = "text-embedding-3-small",
    credential_id: Optional[int] = None,
):
    """
    Generate embeddings for specific files (incremental update).

    Used when files change via webhooks.

    Args:
        user_id: User ID
        repository_id: Repository ID
        file_paths: List of file paths to generate embeddings for
        provider_type: AI provider type
        model: Embedding model name
        credential_id: Specific credential ID to use (optional)

    Returns:
        Dictionary with results
    """
    logger.info(
        f"Starting incremental embedding generation for {len(file_paths)} files "
        f"in repository {repository_id}"
    )

    try:
        result = asyncio.run(
            _generate_embeddings_for_files(
                user_id=user_id,
                repository_id=repository_id,
                file_paths=file_paths,
                provider_type=AIProviderType(provider_type),
                model=EmbeddingModel(model),
                credential_id=credential_id,
            )
        )

        logger.info(
            f"Completed incremental embedding generation: "
            f"{result['embeddings_created']} embeddings created"
        )

        return result

    except Exception as e:
        logger.error(
            f"Error generating embeddings for files: {str(e)}",
            exc_info=True,
        )
        raise


@celery_app.task(bind=True, base=EmbeddingTask, name="embeddings.delete_for_files")
def delete_embeddings_for_files_task(
    self,
    repository_id: str,
    file_paths: List[str],
):
    """
    Delete embeddings for deleted files.

    Used when files are deleted via webhooks.

    Args:
        repository_id: Repository ID
        file_paths: List of file paths to delete embeddings for

    Returns:
        Dictionary with results
    """
    logger.info(
        f"Deleting embeddings for {len(file_paths)} files in repository {repository_id}"
    )

    try:
        result = asyncio.run(
            _delete_embeddings_for_files(
                repository_id=repository_id,
                file_paths=file_paths,
            )
        )

        logger.info(
            f"Deleted {result['embeddings_deleted']} embeddings for {len(file_paths)} files"
        )

        return result

    except Exception as e:
        logger.error(
            f"Error deleting embeddings: {str(e)}",
            exc_info=True,
        )
        raise


# Async implementation functions

async def _generate_embeddings_for_repository(
    user_id: int,
    repository_id: str,
    provider_type: AIProviderType,
    model: EmbeddingModel,
    credential_id: Optional[int],
    force_regenerate: bool,
) -> dict:
    """Generate embeddings for all symbols in a repository."""

    async with async_session_maker() as db:
        embeddings_service = EmbeddingsService(db)
        es_service = ElasticsearchService()

        # Delete existing embeddings if force regenerate
        if force_regenerate:
            deleted = await embeddings_service.delete_repository_embeddings(repository_id)
            logger.info(f"Deleted {deleted} existing embeddings")

        # Get all symbols from Elasticsearch
        search_result = await es_service.search_symbols(
            query="*",  # Match all
            repository_id=repository_id,
            limit=10000,  # Max symbols per repository
        )

        symbols = search_result.get("hits", [])

        if not symbols:
            logger.warning(f"No symbols found for repository {repository_id}")
            return {
                "repository_id": repository_id,
                "embeddings_created": 0,
                "symbols_processed": 0,
                "status": "no_symbols",
            }

        # Convert Elasticsearch results to symbol format
        symbol_list = []
        for hit in symbols:
            source = hit["_source"]
            symbol_list.append({
                "symbol_id": source.get("symbol_id", ""),
                "name": source.get("name", ""),
                "kind": source.get("kind", ""),
                "language": source.get("language", ""),
                "file_path": source.get("file_path", ""),
                "signature": source.get("signature", ""),
                "documentation": source.get("documentation", ""),
                "code": source.get("code", ""),
            })

        # Generate embeddings
        embeddings_created = await embeddings_service.generate_embeddings_for_symbols(
            user_id=user_id,
            symbols=symbol_list,
            repository_id=repository_id,
            provider_type=provider_type,
            model=model,
            credential_id=credential_id,
        )

        return {
            "repository_id": repository_id,
            "embeddings_created": embeddings_created,
            "symbols_processed": len(symbol_list),
            "model": model.value,
            "provider": provider_type.value,
            "status": "completed",
        }


async def _generate_embeddings_for_files(
    user_id: int,
    repository_id: str,
    file_paths: List[str],
    provider_type: AIProviderType,
    model: EmbeddingModel,
    credential_id: Optional[int],
) -> dict:
    """Generate embeddings for specific files (incremental)."""

    async with async_session_maker() as db:
        embeddings_service = EmbeddingsService(db)
        es_service = ElasticsearchService()

        # Delete existing embeddings for these files first
        total_deleted = 0
        for file_path in file_paths:
            deleted = await embeddings_service.delete_file_embeddings(
                repository_id=repository_id,
                file_path=file_path,
            )
            total_deleted += deleted

        logger.info(f"Deleted {total_deleted} existing embeddings for {len(file_paths)} files")

        # Get symbols for these files from Elasticsearch
        all_symbols = []

        for file_path in file_paths:
            search_result = await es_service.search_symbols(
                query="*",
                repository_id=repository_id,
                file_path=file_path,
                limit=1000,
            )

            symbols = search_result.get("hits", [])

            for hit in symbols:
                source = hit["_source"]
                all_symbols.append({
                    "symbol_id": source.get("symbol_id", ""),
                    "name": source.get("name", ""),
                    "kind": source.get("kind", ""),
                    "language": source.get("language", ""),
                    "file_path": source.get("file_path", ""),
                    "signature": source.get("signature", ""),
                    "documentation": source.get("documentation", ""),
                    "code": source.get("code", ""),
                })

        if not all_symbols:
            logger.warning(f"No symbols found for files: {file_paths}")
            return {
                "repository_id": repository_id,
                "embeddings_created": 0,
                "files_processed": len(file_paths),
                "status": "no_symbols",
            }

        # Generate embeddings
        embeddings_created = await embeddings_service.generate_embeddings_for_symbols(
            user_id=user_id,
            symbols=all_symbols,
            repository_id=repository_id,
            provider_type=provider_type,
            model=model,
            credential_id=credential_id,
        )

        return {
            "repository_id": repository_id,
            "embeddings_created": embeddings_created,
            "embeddings_deleted": total_deleted,
            "files_processed": len(file_paths),
            "symbols_processed": len(all_symbols),
            "model": model.value,
            "provider": provider_type.value,
            "status": "completed",
        }


async def _delete_embeddings_for_files(
    repository_id: str,
    file_paths: List[str],
) -> dict:
    """Delete embeddings for specific files."""

    async with async_session_maker() as db:
        embeddings_service = EmbeddingsService(db)

        total_deleted = 0
        for file_path in file_paths:
            deleted = await embeddings_service.delete_file_embeddings(
                repository_id=repository_id,
                file_path=file_path,
            )
            total_deleted += deleted

        return {
            "repository_id": repository_id,
            "embeddings_deleted": total_deleted,
            "files_processed": len(file_paths),
            "status": "completed",
        }
