"""
Azure OpenAI Provider Implementation

Implements AI provider interface for Azure OpenAI using customer-provided credentials.
Azure OpenAI requires: resource name, deployment name, and API key.
"""

import httpx
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


class AzureOpenAIProvider(AIProvider):
    """
    Azure OpenAI provider using customer credentials.

    Azure requires:
    - Resource name (e.g., "my-resource")
    - Deployment name (e.g., "gpt-4-deployment")
    - API key
    - API version (e.g., "2024-02-01")
    """

    API_VERSION = "2024-02-01"

    SUPPORTED_EMBEDDING_MODELS = [
        EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
        EmbeddingModel.TEXT_EMBEDDING_3_LARGE,
        EmbeddingModel.TEXT_EMBEDDING_ADA_002,
    ]

    SUPPORTED_CHAT_MODELS = [
        ChatModel.AZURE_GPT_4,
        ChatModel.AZURE_GPT_35_TURBO,
    ]

    EMBEDDING_DIMENSIONS = {
        EmbeddingModel.TEXT_EMBEDDING_3_SMALL: 1536,
        EmbeddingModel.TEXT_EMBEDDING_3_LARGE: 3072,
        EmbeddingModel.TEXT_EMBEDDING_ADA_002: 1536,
    }

    def __init__(self, credentials: AIProviderCredentials):
        super().__init__(credentials)

        if not credentials.resource_name:
            raise ValueError("Azure OpenAI requires resource_name")

        if not credentials.deployment_name:
            raise ValueError("Azure OpenAI requires deployment_name")

        self.api_key = credentials.api_key
        self.resource_name = credentials.resource_name
        self.deployment_name = credentials.deployment_name

        # Azure OpenAI endpoint format:
        # https://{resource}.openai.azure.com/openai/deployments/{deployment}/...
        base_url = f"https://{self.resource_name}.openai.azure.com"

        self.client = httpx.AsyncClient(
            base_url=base_url,
            headers=self._get_headers(),
            timeout=60.0,
        )

    def _get_headers(self) -> dict:
        """Get headers for Azure OpenAI API requests."""
        return {
            "api-key": self.api_key,
            "Content-Type": "application/json",
        }

    def _get_embedding_url(self, deployment_name: str) -> str:
        """Get URL for embeddings endpoint."""
        return f"/openai/deployments/{deployment_name}/embeddings?api-version={self.API_VERSION}"

    def _get_chat_url(self, deployment_name: str) -> str:
        """Get URL for chat completions endpoint."""
        return f"/openai/deployments/{deployment_name}/chat/completions?api-version={self.API_VERSION}"

    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=2, max=10),
        reraise=True,
    )
    async def generate_embeddings(self, request: EmbeddingRequest) -> EmbeddingResponse:
        """
        Generate embeddings using Azure OpenAI API.

        API Docs: https://learn.microsoft.com/en-us/azure/ai-services/openai/reference
        """
        try:
            # Azure uses deployment names instead of model names
            url = self._get_embedding_url(self.deployment_name)

            response = await self.client.post(
                url,
                json={
                    "input": request.texts,
                    "encoding_format": "float",
                },
            )

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid Azure OpenAI API key",
                    AIProviderType.AZURE_OPENAI,
                )

            if response.status_code == 429:
                retry_after = response.headers.get("Retry-After")
                raise RateLimitError(
                    "Azure OpenAI rate limit exceeded",
                    AIProviderType.AZURE_OPENAI,
                    retry_after=int(retry_after) if retry_after else None,
                )

            if response.status_code == 402 or response.status_code == 403:
                raise QuotaExceededError(
                    "Azure OpenAI quota exceeded",
                    AIProviderType.AZURE_OPENAI,
                )

            response.raise_for_status()
            data = response.json()

            # Extract embeddings (sorted by index)
            embeddings_data = sorted(data["data"], key=lambda x: x["index"])
            embeddings = [item["embedding"] for item in embeddings_data]

            return EmbeddingResponse(
                embeddings=embeddings,
                model=self.deployment_name,
                dimensions=self.EMBEDDING_DIMENSIONS.get(
                    request.model,
                    1536  # Default dimension
                ),
                tokens_used=data["usage"]["total_tokens"],
                provider=AIProviderType.AZURE_OPENAI,
            )

        except (InvalidAPIKeyError, RateLimitError, QuotaExceededError):
            raise
        except Exception as e:
            raise AIProviderError(
                f"Azure OpenAI embeddings error: {str(e)}",
                AIProviderType.AZURE_OPENAI,
                original_error=e,
            )

    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=2, max=10),
        reraise=True,
    )
    async def chat_completion(self, request: ChatRequest) -> ChatResponse:
        """
        Generate chat completion using Azure OpenAI API.
        """
        try:
            messages = request.messages.copy()

            # Add system prompt if provided
            if request.system_prompt:
                messages.insert(0, {"role": "system", "content": request.system_prompt})

            url = self._get_chat_url(self.deployment_name)

            response = await self.client.post(
                url,
                json={
                    "messages": messages,
                    "max_tokens": request.max_tokens,
                    "temperature": request.temperature,
                },
            )

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid Azure OpenAI API key",
                    AIProviderType.AZURE_OPENAI,
                )

            if response.status_code == 429:
                retry_after = response.headers.get("Retry-After")
                raise RateLimitError(
                    "Azure OpenAI rate limit exceeded",
                    AIProviderType.AZURE_OPENAI,
                    retry_after=int(retry_after) if retry_after else None,
                )

            if response.status_code == 402 or response.status_code == 403:
                raise QuotaExceededError(
                    "Azure OpenAI quota exceeded",
                    AIProviderType.AZURE_OPENAI,
                )

            response.raise_for_status()
            data = response.json()

            return ChatResponse(
                content=data["choices"][0]["message"]["content"],
                model=self.deployment_name,
                tokens_used=data["usage"]["total_tokens"],
                provider=AIProviderType.AZURE_OPENAI,
                finish_reason=data["choices"][0]["finish_reason"],
            )

        except (InvalidAPIKeyError, RateLimitError, QuotaExceededError):
            raise
        except Exception as e:
            raise AIProviderError(
                f"Azure OpenAI chat error: {str(e)}",
                AIProviderType.AZURE_OPENAI,
                original_error=e,
            )

    async def validate_credentials(self) -> bool:
        """
        Validate Azure OpenAI credentials by making a test request.
        """
        try:
            # Make a small test request
            test_request = EmbeddingRequest(
                texts=["test"],
                model=EmbeddingModel.TEXT_EMBEDDING_3_SMALL,
                user_id="validation",
            )

            await self.generate_embeddings(test_request)
            return True

        except InvalidAPIKeyError:
            raise
        except Exception as e:
            raise AIProviderError(
                f"Azure OpenAI credential validation error: {str(e)}",
                AIProviderType.AZURE_OPENAI,
                original_error=e,
            )

    def get_supported_embedding_models(self) -> List[EmbeddingModel]:
        """Return Azure OpenAI embedding models."""
        return self.SUPPORTED_EMBEDDING_MODELS

    def get_supported_chat_models(self) -> List[ChatModel]:
        """Return Azure OpenAI chat models."""
        return self.SUPPORTED_CHAT_MODELS

    def estimate_tokens(self, text: str, model: Optional[str] = None) -> int:
        """
        Estimate token count (same as OpenAI).

        Azure OpenAI uses the same tokenization as OpenAI.
        Fallback: 1 token ≈ 4 characters.
        """
        return len(text) // 4

    async def close(self):
        """Close HTTP client."""
        await self.client.aclose()
