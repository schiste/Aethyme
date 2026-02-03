"""
Tests for Embeddings Service

Tests vector embeddings generation and semantic search.
"""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from sqlalchemy.ext.asyncio import AsyncSession

from app.services.embeddings_service import EmbeddingsService
from app.services.ai import (
    AIProviderType,
    EmbeddingModel,
    EmbeddingResponse,
)
from app.models.code_embeddings import CodeEmbedding


@pytest.fixture
def mock_db():
    """Mock database session."""
    db = AsyncMock(spec=AsyncSession)
    db.add = MagicMock()
    db.commit = AsyncMock()
    db.execute = AsyncMock()
    return db


@pytest.fixture
def embeddings_service(mock_db):
    """Embeddings service with mocked DB."""
    return EmbeddingsService(mock_db)


@pytest.fixture
def sample_symbols():
    """Sample code symbols for testing."""
    return [
        {
            "symbol_id": "scip:python::module.file:function.validate_email",
            "name": "validate_email",
            "kind": "function",
            "language": "python",
            "file_path": "utils/validators.py",
            "signature": "def validate_email(email: str) -> bool",
            "documentation": "Validates email addresses using regex pattern.",
            "code": "def validate_email(email: str) -> bool:\n    pattern = r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$'\n    return bool(re.match(pattern, email))",
        },
        {
            "symbol_id": "scip:python::module.file:function.hash_password",
            "name": "hash_password",
            "kind": "function",
            "language": "python",
            "file_path": "auth/security.py",
            "signature": "def hash_password(password: str) -> str",
            "documentation": "Hashes a password using bcrypt.",
            "code": "def hash_password(password: str) -> str:\n    return bcrypt.hashpw(password.encode(), bcrypt.gensalt()).decode()",
        },
    ]


# Embedding Text Generation Tests

def test_create_embedding_text_with_all_fields(embeddings_service):
    """Test embedding text generation with all fields present."""
    symbol = {
        "kind": "function",
        "name": "validate_email",
        "signature": "def validate_email(email: str) -> bool",
        "documentation": "Validates email addresses using regex pattern.",
        "code": "def validate_email(email: str) -> bool:\n    return True",
    }

    text = embeddings_service._create_embedding_text(symbol)

    assert "function: validate_email" in text
    assert "Signature: def validate_email(email: str) -> bool" in text
    assert "Validates email addresses" in text
    assert "Code:" in text


def test_create_embedding_text_without_documentation(embeddings_service):
    """Test embedding text generation without documentation."""
    symbol = {
        "kind": "class",
        "name": "User",
        "signature": "class User",
        "code": "class User:\n    pass",
    }

    text = embeddings_service._create_embedding_text(symbol)

    assert "class: User" in text
    assert "Signature: class User" in text
    assert "Code: class User:" in text


def test_create_embedding_text_truncates_long_documentation(embeddings_service):
    """Test that very long documentation is truncated."""
    long_doc = " ".join(["word"] * 300)  # 300 words
    symbol = {
        "kind": "function",
        "name": "test_func",
        "documentation": long_doc,
    }

    text = embeddings_service._create_embedding_text(symbol)

    # Should be truncated to 200 words
    word_count = len(text.split())
    assert word_count < 250  # 200 words + metadata


# Embedding Generation Tests

@pytest.mark.asyncio
async def test_generate_embeddings_for_symbols_success(
    embeddings_service,
    mock_db,
    sample_symbols,
):
    """Test successful embedding generation."""

    # Mock AI provider
    mock_provider = AsyncMock()
    mock_provider.generate_embeddings.return_value = EmbeddingResponse(
        embeddings=[
            [0.1, 0.2, 0.3] * 512,  # 1536 dimensions
            [0.4, 0.5, 0.6] * 512,
        ],
        model="text-embedding-3-small",
        dimensions=1536,
        tokens_used=100,
        provider=AIProviderType.OPENAI,
    )
    mock_provider.close = AsyncMock()

    mock_credential = MagicMock(id=1)

    # Mock credentials service
    with patch.object(
        embeddings_service.credentials_service,
        "get_provider_for_user",
        return_value=(mock_provider, mock_credential),
    ):
        with patch.object(
            embeddings_service.credentials_service,
            "log_usage",
            new=AsyncMock(),
        ):
            count = await embeddings_service.generate_embeddings_for_symbols(
                user_id=1,
                symbols=sample_symbols,
                repository_id="repo123",
            )

            assert count == 2
            assert mock_db.add.call_count == 2
            assert mock_db.commit.called
            assert mock_provider.close.called


@pytest.mark.asyncio
async def test_generate_embeddings_batch_processing(
    embeddings_service,
    mock_db,
):
    """Test that large symbol lists are processed in batches."""

    # Create 250 symbols (more than batch size of 100)
    symbols = []
    for i in range(250):
        symbols.append({
            "symbol_id": f"symbol_{i}",
            "name": f"func_{i}",
            "kind": "function",
            "language": "python",
            "file_path": f"file_{i}.py",
            "signature": f"def func_{i}()",
            "documentation": f"Function {i}",
            "code": f"def func_{i}():\n    pass",
        })

    # Mock AI provider
    mock_provider = AsyncMock()
    mock_provider.generate_embeddings.return_value = EmbeddingResponse(
        embeddings=[[0.1] * 1536] * 100,  # Return 100 embeddings per batch
        model="text-embedding-3-small",
        dimensions=1536,
        tokens_used=100,
        provider=AIProviderType.OPENAI,
    )
    mock_provider.close = AsyncMock()

    mock_credential = MagicMock(id=1)

    with patch.object(
        embeddings_service.credentials_service,
        "get_provider_for_user",
        return_value=(mock_provider, mock_credential),
    ):
        with patch.object(
            embeddings_service.credentials_service,
            "log_usage",
            new=AsyncMock(),
        ):
            count = await embeddings_service.generate_embeddings_for_symbols(
                user_id=1,
                symbols=symbols,
                repository_id="repo123",
            )

            # Should process all 250 symbols
            assert count == 250

            # Should call generate_embeddings 3 times (100 + 100 + 50)
            assert mock_provider.generate_embeddings.call_count == 3


@pytest.mark.asyncio
async def test_generate_embeddings_empty_symbols(
    embeddings_service,
    mock_db,
):
    """Test handling of empty symbols list."""

    count = await embeddings_service.generate_embeddings_for_symbols(
        user_id=1,
        symbols=[],
        repository_id="repo123",
    )

    assert count == 0
    assert not mock_db.add.called


# Search Tests

@pytest.mark.asyncio
async def test_search_by_natural_language(
    embeddings_service,
    mock_db,
):
    """Test natural language search."""

    # Mock AI provider for query embedding
    mock_provider = AsyncMock()
    mock_provider.generate_embeddings.return_value = EmbeddingResponse(
        embeddings=[[0.5] * 1536],
        model="text-embedding-3-small",
        dimensions=1536,
        tokens_used=10,
        provider=AIProviderType.OPENAI,
    )
    mock_provider.close = AsyncMock()

    mock_credential = MagicMock(id=1)

    # Mock search results
    mock_row = MagicMock()
    mock_row.symbol_id = "symbol1"
    mock_row.symbol_name = "validate_email"
    mock_row.symbol_kind = "function"
    mock_row.language = "python"
    mock_row.file_path = "utils/validators.py"
    mock_row.signature = "def validate_email(email: str) -> bool"
    mock_row.documentation = "Validates email addresses"
    mock_row.code_snippet = "def validate_email..."
    mock_row.similarity = 0.85

    with patch.object(
        embeddings_service.credentials_service,
        "get_provider_for_user",
        return_value=(mock_provider, mock_credential),
    ):
        with patch.object(
            embeddings_service.credentials_service,
            "log_usage",
            new=AsyncMock(),
        ):
            with patch.object(
                embeddings_service,
                "search_similar_symbols",
                return_value=[mock_row],
            ):
                results = await embeddings_service.search_by_natural_language(
                    user_id=1,
                    query="function that validates email addresses",
                    repository_id="repo123",
                )

                assert len(results) == 1
                assert results[0]["symbol_name"] == "validate_email"
                assert results[0]["similarity"] == 0.85
                assert mock_provider.close.called


# Deletion Tests

@pytest.mark.asyncio
async def test_delete_repository_embeddings(
    embeddings_service,
    mock_db,
):
    """Test deleting all embeddings for a repository."""

    mock_result = MagicMock()
    mock_result.rowcount = 42
    mock_db.execute.return_value = mock_result

    count = await embeddings_service.delete_repository_embeddings("repo123")

    assert count == 42
    assert mock_db.commit.called


@pytest.mark.asyncio
async def test_delete_file_embeddings(
    embeddings_service,
    mock_db,
):
    """Test deleting embeddings for a specific file."""

    mock_result = MagicMock()
    mock_result.rowcount = 5
    mock_db.execute.return_value = mock_result

    count = await embeddings_service.delete_file_embeddings(
        repository_id="repo123",
        file_path="utils/validators.py",
    )

    assert count == 5
    assert mock_db.commit.called


# Statistics Tests

@pytest.mark.asyncio
async def test_get_embedding_stats(
    embeddings_service,
    mock_db,
):
    """Test getting embedding statistics."""

    # Mock embeddings
    mock_embedding_1 = MagicMock()
    mock_embedding_1.language = "python"
    mock_embedding_1.symbol_kind = "function"
    mock_embedding_1.embedding_provider = "openai"

    mock_embedding_2 = MagicMock()
    mock_embedding_2.language = "python"
    mock_embedding_2.symbol_kind = "class"
    mock_embedding_2.embedding_provider = "openai"

    mock_embedding_3 = MagicMock()
    mock_embedding_3.language = "javascript"
    mock_embedding_3.symbol_kind = "function"
    mock_embedding_3.embedding_provider = "openai"

    mock_result = MagicMock()
    mock_result.scalars.return_value.all.return_value = [
        mock_embedding_1,
        mock_embedding_2,
        mock_embedding_3,
    ]
    mock_db.execute.return_value = mock_result

    stats = await embeddings_service.get_embedding_stats(repository_id="repo123")

    assert stats["total_embeddings"] == 3
    assert stats["by_language"]["python"] == 2
    assert stats["by_language"]["javascript"] == 1
    assert stats["by_symbol_kind"]["function"] == 2
    assert stats["by_symbol_kind"]["class"] == 1
    assert stats["by_provider"]["openai"] == 3
