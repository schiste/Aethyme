"""
Tests for AI Provider Abstraction

Tests OpenAI, Claude, and Azure providers with mocked API calls.
"""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch
import httpx

from app.services.ai import (
    AIProviderFactory,
    AIProviderType,
    AIProviderCredentials,
    OpenAIProvider,
    ClaudeProvider,
    AzureOpenAIProvider,
    EmbeddingModel,
    ChatModel,
    EmbeddingRequest,
    ChatRequest,
    InvalidAPIKeyError,
    RateLimitError,
    QuotaExceededError,
)


# Fixtures

@pytest.fixture
def openai_credentials():
    """OpenAI credentials for testing."""
    return AIProviderCredentials(
        provider_type=AIProviderType.OPENAI,
        api_key="sk-test123",
        organization_id="org-test",
    )


@pytest.fixture
def claude_credentials():
    """Claude credentials for testing."""
    return AIProviderCredentials(
        provider_type=AIProviderType.CLAUDE,
        api_key="sk-ant-test123",
    )


@pytest.fixture
def azure_credentials():
    """Azure OpenAI credentials for testing."""
    return AIProviderCredentials(
        provider_type=AIProviderType.AZURE_OPENAI,
        api_key="azure-test123",
        resource_name="my-resource",
        deployment_name="gpt-4-deployment",
    )


# OpenAI Provider Tests

@pytest.mark.asyncio
async def test_openai_provider_embeddings_success(openai_credentials):
    """Test successful embedding generation with OpenAI."""
    provider = OpenAIProvider(openai_credentials)

    # Mock HTTP response
    mock_response = {
        "data": [
            {"index": 0, "embedding": [0.1, 0.2, 0.3]},
            {"index": 1, "embedding": [0.4, 0.5, 0.6]},
        ],
        "usage": {"total_tokens": 100},
    }

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=200,
            json=lambda: mock_response,
        )

        request = EmbeddingRequest(
            texts=["hello world", "test text"],
            model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
            user_id="user123",
        )

        response = await provider.generate_embeddings(request)

        assert len(response.embeddings) == 2
        assert response.embeddings[0] == [0.1, 0.2, 0.3]
        assert response.embeddings[1] == [0.4, 0.5, 0.6]
        assert response.tokens_used == 100
        assert response.provider == AIProviderType.OPENAI
        assert response.model == "text-embedding-3-small"


@pytest.mark.asyncio
async def test_openai_provider_invalid_key(openai_credentials):
    """Test invalid API key handling."""
    provider = OpenAIProvider(openai_credentials)

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=401,
            raise_for_status=lambda: None,
        )

        request = EmbeddingRequest(
            texts=["test"],
            model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
            user_id="user123",
        )

        with pytest.raises(InvalidAPIKeyError) as exc_info:
            await provider.generate_embeddings(request)

        assert "Invalid OpenAI API key" in str(exc_info.value)


@pytest.mark.asyncio
async def test_openai_provider_rate_limit(openai_credentials):
    """Test rate limit handling."""
    provider = OpenAIProvider(openai_credentials)

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=429,
            headers={"Retry-After": "60"},
            raise_for_status=lambda: None,
        )

        request = EmbeddingRequest(
            texts=["test"],
            model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
            user_id="user123",
        )

        with pytest.raises(RateLimitError) as exc_info:
            await provider.generate_embeddings(request)

        assert exc_info.value.retry_after == 60


@pytest.mark.asyncio
async def test_openai_provider_chat_completion(openai_credentials):
    """Test chat completion with OpenAI."""
    provider = OpenAIProvider(openai_credentials)

    mock_response = {
        "choices": [
            {
                "message": {"content": "This is a test response"},
                "finish_reason": "stop",
            }
        ],
        "usage": {"total_tokens": 50},
    }

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=200,
            json=lambda: mock_response,
        )

        request = ChatRequest(
            messages=[{"role": "user", "content": "Hello"}],
            model=ChatModel.GPT_4,
            user_id="user123",
        )

        response = await provider.chat_completion(request)

        assert response.content == "This is a test response"
        assert response.tokens_used == 50
        assert response.finish_reason == "stop"


@pytest.mark.asyncio
async def test_openai_token_estimation(openai_credentials):
    """Test token count estimation."""
    provider = OpenAIProvider(openai_credentials)

    text = "This is a test string for token counting."
    tokens = provider.estimate_tokens(text)

    # Should estimate roughly 9-11 tokens for this text
    assert 5 < tokens < 20


# Claude Provider Tests

@pytest.mark.asyncio
async def test_claude_provider_embeddings_not_supported(claude_credentials):
    """Test that Claude doesn't support embeddings."""
    provider = ClaudeProvider(claude_credentials)

    request = EmbeddingRequest(
        texts=["test"],
        model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
        user_id="user123",
    )

    with pytest.raises(Exception) as exc_info:
        await provider.generate_embeddings(request)

    assert "doesn't provide embeddings" in str(exc_info.value)


@pytest.mark.asyncio
async def test_claude_provider_chat_completion(claude_credentials):
    """Test chat completion with Claude."""
    provider = ClaudeProvider(claude_credentials)

    mock_response = {
        "content": [
            {"type": "text", "text": "This is Claude's response"}
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
        },
    }

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=200,
            json=lambda: mock_response,
        )

        request = ChatRequest(
            messages=[{"role": "user", "content": "Hello"}],
            model=ChatModel.CLAUDE_3_HAIKU,
            user_id="user123",
        )

        response = await provider.chat_completion(request)

        assert response.content == "This is Claude's response"
        assert response.tokens_used == 30  # input + output
        assert response.finish_reason == "end_turn"


@pytest.mark.asyncio
async def test_claude_system_prompt_handling(claude_credentials):
    """Test that Claude handles system prompts correctly."""
    provider = ClaudeProvider(claude_credentials)

    mock_response = {
        "content": [{"type": "text", "text": "Response"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5},
    }

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=200,
            json=lambda: mock_response,
        )

        request = ChatRequest(
            messages=[{"role": "user", "content": "Hello"}],
            model=ChatModel.CLAUDE_3_HAIKU,
            user_id="user123",
            system_prompt="You are a helpful assistant.",
        )

        await provider.chat_completion(request)

        # Verify system prompt was added to payload
        call_args = mock_post.call_args
        payload = call_args[1]["json"]
        assert payload["system"] == "You are a helpful assistant."


# Azure OpenAI Provider Tests

@pytest.mark.asyncio
async def test_azure_provider_embeddings(azure_credentials):
    """Test embedding generation with Azure OpenAI."""
    provider = AzureOpenAIProvider(azure_credentials)

    mock_response = {
        "data": [
            {"index": 0, "embedding": [0.1, 0.2, 0.3]},
        ],
        "usage": {"total_tokens": 50},
    }

    with patch.object(provider.client, "post") as mock_post:
        mock_post.return_value = MagicMock(
            status_code=200,
            json=lambda: mock_response,
        )

        request = EmbeddingRequest(
            texts=["test"],
            model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
            user_id="user123",
        )

        response = await provider.generate_embeddings(request)

        assert len(response.embeddings) == 1
        assert response.model == "gpt-4-deployment"  # deployment name
        assert response.provider == AIProviderType.AZURE_OPENAI


@pytest.mark.asyncio
async def test_azure_provider_missing_credentials():
    """Test that Azure requires resource_name and deployment_name."""
    # Missing resource_name
    with pytest.raises(ValueError) as exc_info:
        credentials = AIProviderCredentials(
            provider_type=AIProviderType.AZURE_OPENAI,
            api_key="test",
            deployment_name="test-deployment",
        )
        AzureOpenAIProvider(credentials)

    assert "resource_name" in str(exc_info.value)

    # Missing deployment_name
    with pytest.raises(ValueError) as exc_info:
        credentials = AIProviderCredentials(
            provider_type=AIProviderType.AZURE_OPENAI,
            api_key="test",
            resource_name="test-resource",
        )
        AzureOpenAIProvider(credentials)

    assert "deployment_name" in str(exc_info.value)


# Provider Factory Tests

def test_provider_factory_openai(openai_credentials):
    """Test factory creates OpenAI provider."""
    provider = AIProviderFactory.create(openai_credentials)
    assert isinstance(provider, OpenAIProvider)


def test_provider_factory_claude(claude_credentials):
    """Test factory creates Claude provider."""
    provider = AIProviderFactory.create(claude_credentials)
    assert isinstance(provider, ClaudeProvider)


def test_provider_factory_azure(azure_credentials):
    """Test factory creates Azure provider."""
    provider = AIProviderFactory.create(azure_credentials)
    assert isinstance(provider, AzureOpenAIProvider)


def test_provider_factory_unsupported():
    """Test factory rejects unsupported provider."""
    # Create a fake provider type
    credentials = AIProviderCredentials(
        provider_type="unsupported",  # type: ignore
        api_key="test",
    )

    with pytest.raises(ValueError) as exc_info:
        AIProviderFactory.create(credentials)

    assert "Unsupported provider type" in str(exc_info.value)


def test_get_supported_providers():
    """Test getting list of supported providers."""
    providers = AIProviderFactory.get_supported_providers()

    assert AIProviderType.OPENAI in providers
    assert AIProviderType.CLAUDE in providers
    assert AIProviderType.AZURE_OPENAI in providers


# Model Support Tests

def test_openai_supported_models(openai_credentials):
    """Test OpenAI supported models."""
    provider = OpenAIProvider(openai_credentials)

    embedding_models = provider.get_supported_embedding_models()
    assert EmbeddingModel.TEXT_EMBEDDING_3_SMALL in embedding_models
    assert EmbeddingModel.TEXT_EMBEDDING_3_LARGE in embedding_models

    chat_models = provider.get_supported_chat_models()
    assert ChatModel.GPT_4 in chat_models
    assert ChatModel.GPT_4_TURBO in chat_models


def test_claude_supported_models(claude_credentials):
    """Test Claude supported models."""
    provider = ClaudeProvider(claude_credentials)

    embedding_models = provider.get_supported_embedding_models()
    assert len(embedding_models) == 0  # Claude doesn't support embeddings

    chat_models = provider.get_supported_chat_models()
    assert ChatModel.CLAUDE_3_5_SONNET in chat_models
    assert ChatModel.CLAUDE_3_OPUS in chat_models
    assert ChatModel.CLAUDE_3_HAIKU in chat_models
