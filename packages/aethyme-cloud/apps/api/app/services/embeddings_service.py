"""
Embeddings Generation Service

Generates and stores vector embeddings for semantic code search using customer AI keys.
"""

from typing import List, Optional, Dict, Any
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, delete, and_, text
from datetime import datetime
import asyncio

from app.models.code_embeddings import CodeEmbedding
from app.services.ai_credentials_service import AICredentialsService
from app.services.ai import (
    AIProviderType,
    EmbeddingModel,
    EmbeddingRequest,
    InvalidAPIKeyError,
    AIProviderError,
)


class EmbeddingsService:
    """
    Service for generating and managing code embeddings.

    Uses customer-provided AI API keys to generate embeddings.
    """

    # Default embedding model
    DEFAULT_MODEL = EmbeddingModel.TEXT_EMBEDDING_3_SMALL
    DEFAULT_PROVIDER = AIProviderType.OPENAI

    # Batch size for embedding generation (API limits)
    BATCH_SIZE = 100  # OpenAI allows up to 2048 inputs

    def __init__(self, db: AsyncSession):
        self.db = db
        self.credentials_service = AICredentialsService(db)

    async def generate_embeddings_for_symbols(
        self,
        user_id: int,
        symbols: List[Dict[str, Any]],
        repository_id: str,
        provider_type: AIProviderType = DEFAULT_PROVIDER,
        model: EmbeddingModel = DEFAULT_MODEL,
        credential_id: Optional[int] = None,
    ) -> int:
        """
        Generate embeddings for a list of code symbols.

        Args:
            user_id: User ID (owner of repository)
            symbols: List of symbol dicts from SCIP/Elasticsearch
                Each symbol: {
                    "symbol_id": "...",
                    "name": "...",
                    "kind": "function",
                    "language": "python",
                    "file_path": "...",
                    "signature": "def foo(x: int) -> str",
                    "documentation": "...",
                    "code": "...",
                }
            repository_id: Repository ID
            provider_type: AI provider to use
            model: Embedding model to use
            credential_id: Specific credential ID (optional)

        Returns:
            Number of embeddings created

        Raises:
            InvalidAPIKeyError: If no valid API key found
            AIProviderError: If embedding generation fails
        """
        if not symbols:
            return 0

        # Get AI provider for user
        provider, credential = await self.credentials_service.get_provider_for_user(
            user_id=user_id,
            provider_type=provider_type,
            credential_id=credential_id,
        )

        try:
            # Process in batches
            embeddings_created = 0

            for i in range(0, len(symbols), self.BATCH_SIZE):
                batch = symbols[i : i + self.BATCH_SIZE]

                # Create text representations for embedding
                texts = []
                for symbol in batch:
                    text = self._create_embedding_text(symbol)
                    texts.append(text)

                # Generate embeddings
                request = EmbeddingRequest(
                    texts=texts,
                    model=model,
                    user_id=str(user_id),
                )

                response = await provider.generate_embeddings(request)

                # Store embeddings in database
                for symbol, embedding in zip(batch, response.embeddings):
                    code_embedding = CodeEmbedding(
                        repository_id=repository_id,
                        file_path=symbol["file_path"],
                        symbol_id=symbol["symbol_id"],
                        symbol_name=symbol["name"],
                        symbol_kind=symbol["kind"],
                        language=symbol["language"],
                        signature=symbol.get("signature"),
                        documentation=symbol.get("documentation"),
                        code_snippet=symbol.get("code", "")[:500],  # First 500 chars
                        embedding=embedding,
                        embedding_model=model.value,
                        embedding_provider=provider_type.value,
                    )

                    self.db.add(code_embedding)
                    embeddings_created += 1

                # Log usage
                await self.credentials_service.log_usage(
                    user_id=user_id,
                    credential_id=credential.id,
                    provider_type=provider_type,
                    operation_type="embedding",
                    model=model.value,
                    tokens_used=response.tokens_used,
                    repository_id=repository_id,
                )

                # Commit batch
                await self.db.commit()

            return embeddings_created

        finally:
            await provider.close()

    def _create_embedding_text(self, symbol: Dict[str, Any]) -> str:
        """
        Create text representation for embedding.

        Combines symbol name, signature, documentation, and code snippet
        into a single text optimized for semantic search.
        """
        parts = []

        # Symbol name and kind
        parts.append(f"{symbol['kind']}: {symbol['name']}")

        # Signature (if available)
        if symbol.get("signature"):
            parts.append(f"Signature: {symbol['signature']}")

        # Documentation (if available)
        if symbol.get("documentation"):
            # Limit documentation to 200 words
            doc = symbol["documentation"]
            words = doc.split()[:200]
            parts.append(" ".join(words))

        # Code snippet (if available and documentation is short)
        if symbol.get("code") and len(symbol.get("documentation", "")) < 100:
            code = symbol["code"][:500]  # First 500 chars
            parts.append(f"Code: {code}")

        return "\n".join(parts)

    async def search_similar_symbols(
        self,
        query_embedding: List[float],
        repository_id: Optional[str] = None,
        language: Optional[str] = None,
        symbol_kind: Optional[str] = None,
        limit: int = 10,
        similarity_threshold: float = 0.7,
    ) -> List[CodeEmbedding]:
        """
        Search for similar code symbols using vector similarity.

        Args:
            query_embedding: Query vector embedding
            repository_id: Filter by repository (optional)
            language: Filter by language (optional)
            symbol_kind: Filter by symbol kind (optional)
            limit: Maximum results to return
            similarity_threshold: Minimum cosine similarity (0-1)

        Returns:
            List of similar CodeEmbedding records with similarity scores
        """
        # Build WHERE clause
        where_clauses = []

        if repository_id:
            where_clauses.append(f"repository_id = '{repository_id}'")

        if language:
            where_clauses.append(f"language = '{language}'")

        if symbol_kind:
            where_clauses.append(f"symbol_kind = '{symbol_kind}'")

        where_sql = " AND ".join(where_clauses) if where_clauses else "TRUE"

        # Convert embedding to pgvector format
        embedding_str = "[" + ",".join(str(x) for x in query_embedding) + "]"

        # Query with cosine similarity
        # 1 - cosine_distance = cosine_similarity
        query = text(f"""
            SELECT *,
                   1 - (embedding <=> :query_embedding::vector) as similarity
            FROM code_embeddings
            WHERE {where_sql}
              AND 1 - (embedding <=> :query_embedding::vector) >= :threshold
            ORDER BY embedding <=> :query_embedding::vector
            LIMIT :limit
        """)

        result = await self.db.execute(
            query,
            {
                "query_embedding": embedding_str,
                "threshold": similarity_threshold,
                "limit": limit,
            },
        )

        return result.fetchall()

    async def search_by_natural_language(
        self,
        user_id: int,
        query: str,
        repository_id: Optional[str] = None,
        language: Optional[str] = None,
        symbol_kind: Optional[str] = None,
        limit: int = 10,
        provider_type: AIProviderType = DEFAULT_PROVIDER,
        model: EmbeddingModel = DEFAULT_MODEL,
        credential_id: Optional[int] = None,
    ) -> List[Dict[str, Any]]:
        """
        Search code using natural language query.

        Example queries:
        - "function that validates email addresses"
        - "authentication middleware"
        - "parse JSON configuration file"

        Args:
            user_id: User ID
            query: Natural language query
            repository_id: Filter by repository (optional)
            language: Filter by language (optional)
            symbol_kind: Filter by symbol kind (optional)
            limit: Maximum results
            provider_type: AI provider to use
            model: Embedding model to use
            credential_id: Specific credential ID (optional)

        Returns:
            List of search results with similarity scores
        """
        # Get AI provider for user
        provider, credential = await self.credentials_service.get_provider_for_user(
            user_id=user_id,
            provider_type=provider_type,
            credential_id=credential_id,
        )

        try:
            # Generate embedding for query
            request = EmbeddingRequest(
                texts=[query],
                model=model,
                user_id=str(user_id),
            )

            response = await provider.generate_embeddings(request)
            query_embedding = response.embeddings[0]

            # Log usage
            await self.credentials_service.log_usage(
                user_id=user_id,
                credential_id=credential.id,
                provider_type=provider_type,
                operation_type="embedding",
                model=model.value,
                tokens_used=response.tokens_used,
            )

            # Search for similar symbols
            results = await self.search_similar_symbols(
                query_embedding=query_embedding,
                repository_id=repository_id,
                language=language,
                symbol_kind=symbol_kind,
                limit=limit,
            )

            # Format results
            formatted_results = []
            for row in results:
                formatted_results.append({
                    "symbol_id": row.symbol_id,
                    "symbol_name": row.symbol_name,
                    "symbol_kind": row.symbol_kind,
                    "language": row.language,
                    "file_path": row.file_path,
                    "signature": row.signature,
                    "documentation": row.documentation,
                    "code_snippet": row.code_snippet,
                    "similarity": row.similarity,
                })

            return formatted_results

        finally:
            await provider.close()

    async def delete_repository_embeddings(self, repository_id: str) -> int:
        """
        Delete all embeddings for a repository.

        Args:
            repository_id: Repository ID

        Returns:
            Number of embeddings deleted
        """
        result = await self.db.execute(
            delete(CodeEmbedding).where(CodeEmbedding.repository_id == repository_id)
        )

        await self.db.commit()
        return result.rowcount

    async def delete_file_embeddings(self, repository_id: str, file_path: str) -> int:
        """
        Delete embeddings for a specific file.

        Args:
            repository_id: Repository ID
            file_path: File path

        Returns:
            Number of embeddings deleted
        """
        result = await self.db.execute(
            delete(CodeEmbedding).where(
                and_(
                    CodeEmbedding.repository_id == repository_id,
                    CodeEmbedding.file_path == file_path,
                )
            )
        )

        await self.db.commit()
        return result.rowcount

    async def get_embedding_stats(self, repository_id: Optional[str] = None) -> Dict[str, Any]:
        """
        Get statistics about stored embeddings.

        Args:
            repository_id: Filter by repository (optional)

        Returns:
            Statistics dictionary
        """
        query = select(CodeEmbedding)

        if repository_id:
            query = query.where(CodeEmbedding.repository_id == repository_id)

        result = await self.db.execute(query)
        embeddings = result.scalars().all()

        # Calculate stats
        total_count = len(embeddings)

        by_language = {}
        by_kind = {}
        by_provider = {}

        for embedding in embeddings:
            # Count by language
            lang = embedding.language
            by_language[lang] = by_language.get(lang, 0) + 1

            # Count by kind
            kind = embedding.symbol_kind
            by_kind[kind] = by_kind.get(kind, 0) + 1

            # Count by provider
            provider = embedding.embedding_provider
            by_provider[provider] = by_provider.get(provider, 0) + 1

        return {
            "total_embeddings": total_count,
            "by_language": by_language,
            "by_symbol_kind": by_kind,
            "by_provider": by_provider,
        }
