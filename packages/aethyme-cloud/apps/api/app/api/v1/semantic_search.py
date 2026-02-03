"""
Semantic Search API Endpoints

Natural language code search using vector embeddings and customer AI keys.
"""

from typing import List, Optional
from fastapi import APIRouter, Depends, HTTPException, status, BackgroundTasks
from pydantic import BaseModel, Field

from app.core.auth import get_current_user
from app.core.database import get_db
from app.models.user import User
from app.services.embeddings_service import EmbeddingsService
from app.services.ai import AIProviderType, EmbeddingModel


router = APIRouter()


# Request/Response Models

class SemanticSearchRequest(BaseModel):
    """Request for semantic code search."""

    query: str = Field(..., description="Natural language query", min_length=3)
    repository_id: Optional[str] = Field(None, description="Filter by repository")
    language: Optional[str] = Field(None, description="Filter by programming language")
    symbol_kind: Optional[str] = Field(None, description="Filter by symbol kind (function, class, etc.)")
    limit: int = Field(10, ge=1, le=100, description="Maximum results to return")
    provider_type: AIProviderType = Field(
        AIProviderType.OPENAI,
        description="AI provider to use for embeddings"
    )
    model: EmbeddingModel = Field(
        EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
        description="Embedding model to use"
    )
    credential_id: Optional[int] = Field(None, description="Specific credential to use")


class SemanticSearchResult(BaseModel):
    """Search result with similarity score."""

    symbol_id: str
    symbol_name: str
    symbol_kind: str
    language: str
    file_path: str
    signature: Optional[str] = None
    documentation: Optional[str] = None
    code_snippet: Optional[str] = None
    similarity: float = Field(..., ge=0.0, le=1.0, description="Cosine similarity score (0-1)")


class SemanticSearchResponse(BaseModel):
    """Response with search results."""

    query: str
    results: List[SemanticSearchResult]
    total: int
    model_used: str
    provider_used: str


class GenerateEmbeddingsRequest(BaseModel):
    """Request to generate embeddings for a repository."""

    repository_id: str
    provider_type: AIProviderType = AIProviderType.OPENAI
    model: EmbeddingModel = EmbeddingModel.TEXT_EMBEDDING_3_SMALL
    credential_id: Optional[int] = None


class GenerateEmbeddingsResponse(BaseModel):
    """Response after generating embeddings."""

    repository_id: str
    embeddings_created: int
    model_used: str
    provider_used: str
    status: str


class EmbeddingStatsResponse(BaseModel):
    """Statistics about stored embeddings."""

    total_embeddings: int
    by_language: dict
    by_symbol_kind: dict
    by_provider: dict


# Endpoints

@router.post("/search", response_model=SemanticSearchResponse)
async def semantic_search(
    request: SemanticSearchRequest,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Search code using natural language.

    Examples:
    - "function that validates email addresses"
    - "authentication middleware"
    - "parse JSON configuration file"
    - "algorithm to sort an array"

    Uses customer-provided AI API keys to generate embeddings for the query,
    then finds similar code using vector similarity search.
    """
    service = EmbeddingsService(db)

    try:
        results = await service.search_by_natural_language(
            user_id=current_user.id,
            query=request.query,
            repository_id=request.repository_id,
            language=request.language,
            symbol_kind=request.symbol_kind,
            limit=request.limit,
            provider_type=request.provider_type,
            model=request.model,
            credential_id=request.credential_id,
        )

        return SemanticSearchResponse(
            query=request.query,
            results=[SemanticSearchResult(**r) for r in results],
            total=len(results),
            model_used=request.model.value,
            provider_used=request.provider_type.value,
        )

    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=str(e),
        )
    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Search failed: {str(e)}",
        )


@router.post("/embeddings/generate", response_model=GenerateEmbeddingsResponse, status_code=status.HTTP_202_ACCEPTED)
async def generate_embeddings(
    request: GenerateEmbeddingsRequest,
    background_tasks: BackgroundTasks,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Generate embeddings for a repository.

    This endpoint triggers background embedding generation for all symbols
    in the repository. Uses customer's AI API key to generate embeddings.

    Returns immediately with 202 Accepted. Embeddings are generated in the background.
    """
    from app.workers.tasks.embeddings import generate_embeddings_for_repository_task

    # Trigger background task
    task = generate_embeddings_for_repository_task.delay(
        user_id=current_user.id,
        repository_id=request.repository_id,
        provider_type=request.provider_type.value,
        model=request.model.value,
        credential_id=request.credential_id,
        force_regenerate=False,
    )

    return GenerateEmbeddingsResponse(
        repository_id=request.repository_id,
        embeddings_created=0,
        model_used=request.model.value,
        provider_used=request.provider_type.value,
        status="accepted",
    )


@router.delete("/embeddings/repository/{repository_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_repository_embeddings(
    repository_id: str,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Delete all embeddings for a repository.

    Useful when re-indexing or removing a repository.
    """
    service = EmbeddingsService(db)

    deleted_count = await service.delete_repository_embeddings(repository_id)

    return {"deleted": deleted_count}


@router.delete("/embeddings/file", status_code=status.HTTP_204_NO_CONTENT)
async def delete_file_embeddings(
    repository_id: str,
    file_path: str,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Delete embeddings for a specific file.

    Useful for incremental re-indexing.
    """
    service = EmbeddingsService(db)

    deleted_count = await service.delete_file_embeddings(repository_id, file_path)

    return {"deleted": deleted_count}


@router.get("/embeddings/stats", response_model=EmbeddingStatsResponse)
async def get_embedding_stats(
    repository_id: Optional[str] = None,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Get statistics about stored embeddings.

    Optionally filter by repository.
    """
    service = EmbeddingsService(db)

    stats = await service.get_embedding_stats(repository_id=repository_id)

    return EmbeddingStatsResponse(**stats)


@router.get("/examples", response_model=List[str])
async def get_example_queries():
    """
    Get example natural language queries for semantic search.

    Helps users understand how to use semantic search effectively.
    """
    return [
        "function that validates email addresses",
        "authentication middleware with JWT",
        "parse JSON configuration file",
        "algorithm to sort an array",
        "HTTP request handler for user login",
        "database connection pool manager",
        "cache implementation with expiration",
        "file upload with validation",
        "error handling middleware",
        "rate limiting decorator",
        "async task queue worker",
        "data serialization utility",
        "date formatting helper",
        "password hashing function",
        "API client with retry logic",
    ]
