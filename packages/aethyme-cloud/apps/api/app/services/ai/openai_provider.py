"""
OpenAI Provider Implementation

Implements AI provider interface for OpenAI using customer-provided API keys.
"""

import httpx
import tiktoken
from typing import List, Optional
from tenacity import retry, stop_after_attempt, wait_exponential

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
)


class OpenAIProvider(AIProvider):
    """OpenAI provider using customer API keys."""

    BASE_URL = "https://api.openai.com/v1"

    SUPPORTED_EMBEDDING_MODELS = [
        EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
        EmbeddingModel.TEXT_EMBEDDING_3_LARGE,
        EmbeddingModel.TEXT_EMBEDDING_ADA_002,
    ]

    SUPPORTED_CHAT_MODELS = [
        ChatModel.GPT_4_TURBO,
        ChatModel.GPT_4,
        ChatModel.GPT_3_5_TURBO,
    ]

    # Embedding dimensions by model
    EMBEDDING_DIMENSIONS = {
        EmbeddingModel.TEXT_EMBEDDING_3_SMALL: 1536,
        EmbeddingModel.TEXT_EMBEDDING_3_LARGE: 3072,
        EmbeddingModel.TEXT_EMBEDDING_ADA_002: 1536,
    }

    def __init__(self, credentials: AIProviderCredentials):
        super().__init__(credentials)
        self.api_key = credentials.api_key
        self.organization_id = credentials.organization_id
        self.client = httpx.AsyncClient(
            base_url=self.BASE_URL,
            headers=self._get_headers(),
            timeout=60.0,
        )

    def _get_headers(self) -> dict:
        """Get headers for OpenAI API requests."""
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }
        if self.organization_id:
            headers["OpenAI-Organization"] = self.organization_id
        return headers

    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=2, max=10),
        reraise=True,
    )
    async def generate_embeddings(self, request: EmbeddingRequest) -> EmbeddingResponse:
        """
        Generate embeddings using OpenAI API.

        API Docs: https://platform.openai.com/docs/api-reference/embeddings
        """
        try:
            response = await self.client.post(
                "/embeddings",
                json={
                    "input": request.texts,
                    "model": request.model.value,
                    "encoding_format": "float",  # vs base64
                },
            )

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid OpenAI API key",
                    AIProviderType.OPENAI,
                )

            if response.status_code == 429:
                retry_after = response.headers.get("Retry-After")
                raise RateLimitError(
                    "OpenAI rate limit exceeded",
                    AIProviderType.OPENAI,
                    retry_after=int(retry_after) if retry_after else None,
                )

            if response.status_code == 402:
                raise QuotaExceededError(
                    "OpenAI quota exceeded - customer needs to add credits",
                    AIProviderType.OPENAI,
                )

            response.raise_for_status()
            data = response.json()

            # Extract embeddings (sorted by index)
            embeddings_data = sorted(data["data"], key=lambda x: x["index"])
            embeddings = [item["embedding"] for item in embeddings_data]

            return EmbeddingResponse(
                embeddings=embeddings,
                model=request.model.value,
                dimensions=self.EMBEDDING_DIMENSIONS[request.model],
                tokens_used=data["usage"]["total_tokens"],
                provider=AIProviderType.OPENAI,
            )

        except (InvalidAPIKeyError, RateLimitError, QuotaExceededError):
            raise
        except Exception as e:
            raise AIProviderError(
                f"OpenAI embeddings error: {str(e)}",
                AIProviderType.OPENAI,
                original_error=e,
            )

    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=2, max=10),
        reraise=True,
    )
    async def chat_completion(self, request: ChatRequest) -> ChatResponse:
        """
        Generate chat completion using OpenAI API.

        API Docs: https://platform.openai.com/docs/api-reference/chat
        """
        try:
            messages = request.messages.copy()

            # Add system prompt if provided
            if request.system_prompt:
                messages.insert(0, {"role": "system", "content": request.system_prompt})

            response = await self.client.post(
                "/chat/completions",
                json={
                    "model": request.model.value,
                    "messages": messages,
                    "max_tokens": request.max_tokens,
                    "temperature": request.temperature,
                },
            )

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid OpenAI API key",
                    AIProviderType.OPENAI,
                )

            if response.status_code == 429:
                retry_after = response.headers.get("Retry-After")
                raise RateLimitError(
                    "OpenAI rate limit exceeded",
                    AIProviderType.OPENAI,
                    retry_after=int(retry_after) if retry_after else None,
                )

            if response.status_code == 402:
                raise QuotaExceededError(
                    "OpenAI quota exceeded",
                    AIProviderType.OPENAI,
                )

            response.raise_for_status()
            data = response.json()

            return ChatResponse(
                content=data["choices"][0]["message"]["content"],
                model=request.model.value,
                tokens_used=data["usage"]["total_tokens"],
                provider=AIProviderType.OPENAI,
                finish_reason=data["choices"][0]["finish_reason"],
            )

        except (InvalidAPIKeyError, RateLimitError, QuotaExceededError):
            raise
        except Exception as e:
            raise AIProviderError(
                f"OpenAI chat error: {str(e)}",
                AIProviderType.OPENAI,
                original_error=e,
            )

    async def validate_credentials(self) -> bool:
        """
        Validate OpenAI API key by making a small test request.
        """
        try:
            response = await self.client.get("/models")

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid OpenAI API key",
                    AIProviderType.OPENAI,
                )

            response.raise_for_status()
            return True

        except InvalidAPIKeyError:
            raise
        except Exception as e:
            raise AIProviderError(
                f"OpenAI credential validation error: {str(e)}",
                AIProviderType.OPENAI,
                original_error=e,
            )

    def get_supported_embedding_models(self) -> List[EmbeddingModel]:
        """Return OpenAI embedding models."""
        return self.SUPPORTED_EMBEDDING_MODELS

    def get_supported_chat_models(self) -> List[ChatModel]:
        """Return OpenAI chat models."""
        return self.SUPPORTED_CHAT_MODELS

    def estimate_tokens(self, text: str, model: Optional[str] = None) -> int:
        """
        Estimate token count using tiktoken.

        For embeddings, use cl100k_base encoding (GPT-3.5/4).
        """
        try:
            # Default to cl100k_base encoding (used by text-embedding-3-* and GPT-4)
            encoding_name = "cl100k_base"

            if model:
                # Use model-specific encoding if available
                try:
                    encoding = tiktoken.encoding_for_model(model)
                except KeyError:
                    encoding = tiktoken.get_encoding(encoding_name)
            else:
                encoding = tiktoken.get_encoding(encoding_name)

            tokens = encoding.encode(text)
            return len(tokens)

        except Exception:
            # Fallback: rough estimate (1 token ≈ 4 characters)
            return len(text) // 4

    async def close(self):
        """Close HTTP client."""
        await self.client.aclose()
