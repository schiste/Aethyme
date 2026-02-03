"""
Base AI Provider Interface

Defines the abstract interface for AI providers using customer-provided API keys.
Supports multiple providers: Claude (Anthropic), OpenAI, Azure OpenAI, AWS Bedrock, Google Vertex AI.
"""

from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import Enum
from typing import List, Optional, Dict, Any
import time


class AIProviderType(str, Enum):
    """Supported AI providers."""
    CLAUDE = "claude"
    OPENAI = "openai"
    AZURE_OPENAI = "azure_openai"
    AWS_BEDROCK = "aws_bedrock"
    GOOGLE_VERTEX = "google_vertex"


class EmbeddingModel(str, Enum):
    """Supported embedding models."""
    # OpenAI
    TEXT_EMBEDDING_3_SMALL = "text-embedding-3-small"  # 1536 dims, $0.02/1M tokens
    TEXT_EMBEDDING_3_LARGE = "text-embedding-3-large"  # 3072 dims, $0.13/1M tokens
    TEXT_EMBEDDING_ADA_002 = "text-embedding-ada-002"  # 1536 dims, legacy

    # Vertex AI
    TEXT_EMBEDDING_GECKO = "textembedding-gecko@003"   # 768 dims
    TEXT_MULTILINGUAL = "text-multilingual-embedding-002"  # 768 dims

    # AWS Bedrock
    TITAN_EMBED_TEXT_V1 = "amazon.titan-embed-text-v1"  # 1536 dims
    TITAN_EMBED_TEXT_V2 = "amazon.titan-embed-text-v2"  # 1024 dims

    # Cohere (via Bedrock/Azure)
    COHERE_EMBED_ENGLISH = "cohere.embed-english-v3"   # 1024 dims
    COHERE_EMBED_MULTILINGUAL = "cohere.embed-multilingual-v3"  # 1024 dims


class ChatModel(str, Enum):
    """Supported chat models for code explanations."""
    # Claude
    CLAUDE_3_5_SONNET = "claude-3-5-sonnet-20241022"
    CLAUDE_3_OPUS = "claude-3-opus-20240229"
    CLAUDE_3_SONNET = "claude-3-sonnet-20240229"
    CLAUDE_3_HAIKU = "claude-3-haiku-20240307"

    # OpenAI
    GPT_4_TURBO = "gpt-4-turbo-preview"
    GPT_4 = "gpt-4"
    GPT_3_5_TURBO = "gpt-3.5-turbo"

    # Azure OpenAI (deployment names)
    AZURE_GPT_4 = "gpt-4"
    AZURE_GPT_35_TURBO = "gpt-35-turbo"


@dataclass
class EmbeddingRequest:
    """Request for generating embeddings."""
    texts: List[str]
    model: EmbeddingModel
    user_id: str  # For tracking usage


@dataclass
class EmbeddingResponse:
    """Response with generated embeddings."""
    embeddings: List[List[float]]
    model: str
    dimensions: int
    tokens_used: int
    provider: AIProviderType


@dataclass
class ChatRequest:
    """Request for chat completion."""
    messages: List[Dict[str, str]]  # [{"role": "user", "content": "..."}]
    model: ChatModel
    user_id: str
    max_tokens: Optional[int] = 4096
    temperature: float = 0.0
    system_prompt: Optional[str] = None


@dataclass
class ChatResponse:
    """Response from chat completion."""
    content: str
    model: str
    tokens_used: int
    provider: AIProviderType
    finish_reason: str


@dataclass
class AIProviderCredentials:
    """Credentials for an AI provider."""
    provider_type: AIProviderType
    api_key: str  # Encrypted in database
    organization_id: Optional[str] = None  # For OpenAI
    resource_name: Optional[str] = None    # For Azure
    deployment_name: Optional[str] = None  # For Azure
    region: Optional[str] = None           # For AWS Bedrock, Google Vertex
    project_id: Optional[str] = None       # For Google Vertex
    endpoint: Optional[str] = None         # For custom endpoints


class AIProvider(ABC):
    """
    Abstract base class for AI providers.

    Each provider implementation handles:
    - Authentication with customer-provided API keys
    - Embeddings generation
    - Chat completions
    - Usage tracking
    - Error handling and retries
    """

    def __init__(self, credentials: AIProviderCredentials):
        """Initialize provider with customer credentials."""
        self.credentials = credentials
        self.provider_type = credentials.provider_type

    @abstractmethod
    async def generate_embeddings(self, request: EmbeddingRequest) -> EmbeddingResponse:
        """
        Generate embeddings for a list of texts.

        Args:
            request: EmbeddingRequest with texts and model

        Returns:
            EmbeddingResponse with embeddings and usage info

        Raises:
            AIProviderError: If API call fails
            InvalidAPIKeyError: If API key is invalid
            RateLimitError: If rate limit exceeded
        """
        pass

    @abstractmethod
    async def chat_completion(self, request: ChatRequest) -> ChatResponse:
        """
        Generate chat completion for code explanation/analysis.

        Args:
            request: ChatRequest with messages and model

        Returns:
            ChatResponse with completion and usage info

        Raises:
            AIProviderError: If API call fails
            InvalidAPIKeyError: If API key is invalid
            RateLimitError: If rate limit exceeded
        """
        pass

    @abstractmethod
    async def validate_credentials(self) -> bool:
        """
        Validate that the provided credentials work.

        Returns:
            True if credentials are valid

        Raises:
            InvalidAPIKeyError: If credentials are invalid
        """
        pass

    @abstractmethod
    def get_supported_embedding_models(self) -> List[EmbeddingModel]:
        """Return list of embedding models supported by this provider."""
        pass

    @abstractmethod
    def get_supported_chat_models(self) -> List[ChatModel]:
        """Return list of chat models supported by this provider."""
        pass

    @abstractmethod
    def estimate_tokens(self, text: str) -> int:
        """
        Estimate token count for a text.
        Used for cost estimation before API calls.
        """
        pass


class AIProviderError(Exception):
    """Base exception for AI provider errors."""

    def __init__(self, message: str, provider: AIProviderType, original_error: Optional[Exception] = None):
        self.message = message
        self.provider = provider
        self.original_error = original_error
        super().__init__(self.message)


class InvalidAPIKeyError(AIProviderError):
    """Raised when API key is invalid or expired."""
    pass


class RateLimitError(AIProviderError):
    """Raised when rate limit is exceeded."""

    def __init__(
        self,
        message: str,
        provider: AIProviderType,
        retry_after: Optional[int] = None,
        original_error: Optional[Exception] = None
    ):
        super().__init__(message, provider, original_error)
        self.retry_after = retry_after  # Seconds until retry allowed


class QuotaExceededError(AIProviderError):
    """Raised when customer's quota is exceeded."""
    pass


class UsageTracker:
    """
    Track AI usage per customer.

    Stores:
    - Tokens used per provider
    - API calls made
    - Estimated costs
    - Rate limit status
    """

    def __init__(self, user_id: str, db_session):
        self.user_id = user_id
        self.db_session = db_session

    async def record_embedding_usage(
        self,
        provider: AIProviderType,
        model: str,
        tokens_used: int,
        request_count: int = 1
    ):
        """Record embedding generation usage."""
        # Implementation will store in database
        pass

    async def record_chat_usage(
        self,
        provider: AIProviderType,
        model: str,
        tokens_used: int,
        request_count: int = 1
    ):
        """Record chat completion usage."""
        pass

    async def get_usage_summary(
        self,
        start_date: Optional[str] = None,
        end_date: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        Get usage summary for a time period.

        Returns:
            {
                "total_tokens": 123456,
                "total_requests": 789,
                "by_provider": {
                    "openai": {"tokens": 100000, "requests": 500},
                    "claude": {"tokens": 23456, "requests": 289}
                },
                "estimated_cost": 12.34  # USD
            }
        """
        pass
