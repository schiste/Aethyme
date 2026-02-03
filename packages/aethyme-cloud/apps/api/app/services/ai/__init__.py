"""
AI Services Package

Provides AI provider abstraction with BYOK (Bring Your Own Key) architecture.
Supports: Claude, OpenAI, Azure OpenAI, AWS Bedrock, Google Vertex AI.
"""

from .base import (
    AIProvider,
    AIProviderType,
    AIProviderCredentials,
    EmbeddingModel,
    ChatModel,
    EmbeddingRequest,
    EmbeddingResponse,
    ChatRequest,
    ChatResponse,
    AIProviderError,
    InvalidAPIKeyError,
    RateLimitError,
    QuotaExceededError,
    UsageTracker,
)
from .openai_provider import OpenAIProvider
from .claude_provider import ClaudeProvider
from .azure_provider import AzureOpenAIProvider


class AIProviderFactory:
    """
    Factory for creating AI provider instances.

    Usage:
        credentials = AIProviderCredentials(
            provider_type=AIProviderType.OPENAI,
            api_key="sk-..."
        )
        provider = AIProviderFactory.create(credentials)
        response = await provider.generate_embeddings(request)
    """

    @staticmethod
    def create(credentials: AIProviderCredentials) -> AIProvider:
        """
        Create AI provider instance based on provider type.

        Args:
            credentials: Provider credentials with type and API key

        Returns:
            AIProvider instance

        Raises:
            ValueError: If provider type is not supported
        """
        provider_map = {
            AIProviderType.OPENAI: OpenAIProvider,
            AIProviderType.CLAUDE: ClaudeProvider,
            AIProviderType.AZURE_OPENAI: AzureOpenAIProvider,
            # TODO: Add AWS Bedrock and Google Vertex AI
        }

        provider_class = provider_map.get(credentials.provider_type)

        if not provider_class:
            raise ValueError(
                f"Unsupported provider type: {credentials.provider_type}. "
                f"Supported: {', '.join(p.value for p in provider_map.keys())}"
            )

        return provider_class(credentials)

    @staticmethod
    def get_supported_providers() -> list[AIProviderType]:
        """Return list of supported provider types."""
        return [
            AIProviderType.OPENAI,
            AIProviderType.CLAUDE,
            AIProviderType.AZURE_OPENAI,
            # AIProviderType.AWS_BEDROCK,  # TODO
            # AIProviderType.GOOGLE_VERTEX,  # TODO
        ]


__all__ = [
    # Base classes
    "AIProvider",
    "AIProviderType",
    "AIProviderCredentials",
    "EmbeddingModel",
    "ChatModel",
    # Request/Response
    "EmbeddingRequest",
    "EmbeddingResponse",
    "ChatRequest",
    "ChatResponse",
    # Errors
    "AIProviderError",
    "InvalidAPIKeyError",
    "RateLimitError",
    "QuotaExceededError",
    # Providers
    "OpenAIProvider",
    "ClaudeProvider",
    "AzureOpenAIProvider",
    # Factory
    "AIProviderFactory",
    # Usage tracking
    "UsageTracker",
]
