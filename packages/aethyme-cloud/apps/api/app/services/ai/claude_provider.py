"""
Claude (Anthropic) Provider Implementation

Implements AI provider interface for Claude using customer-provided API keys.

Note: Anthropic doesn't provide embeddings models, so we use Voyage AI for embeddings
(Anthropic's recommended partner) or customers can use OpenAI for embeddings.
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


class ClaudeProvider(AIProvider):
    """
    Claude (Anthropic) provider using customer API keys.

    Claude is excellent for code explanation and analysis but doesn't provide embeddings.
    For embeddings, customers should use OpenAI or Voyage AI.
    """

    BASE_URL = "https://api.anthropic.com/v1"
    API_VERSION = "2023-06-01"

    SUPPORTED_CHAT_MODELS = [
        ChatModel.CLAUDE_3_5_SONNET,
        ChatModel.CLAUDE_3_OPUS,
        ChatModel.CLAUDE_3_SONNET,
        ChatModel.CLAUDE_3_HAIKU,
    ]

    # Claude doesn't provide embeddings
    SUPPORTED_EMBEDDING_MODELS = []

    def __init__(self, credentials: AIProviderCredentials):
        super().__init__(credentials)
        self.api_key = credentials.api_key
        self.client = httpx.AsyncClient(
            base_url=self.BASE_URL,
            headers=self._get_headers(),
            timeout=120.0,  # Claude can be slower for long responses
        )

    def _get_headers(self) -> dict:
        """Get headers for Anthropic API requests."""
        return {
            "x-api-key": self.api_key,
            "anthropic-version": self.API_VERSION,
            "Content-Type": "application/json",
        }

    async def generate_embeddings(self, request: EmbeddingRequest) -> EmbeddingResponse:
        """
        Claude doesn't provide embeddings.

        Customers should use OpenAI or Voyage AI for embeddings.
        """
        raise AIProviderError(
            "Claude (Anthropic) doesn't provide embeddings. Use OpenAI or Voyage AI instead.",
            AIProviderType.CLAUDE,
        )

    @retry(
        stop=stop_after_attempt(3),
        wait=wait_exponential(multiplier=1, min=2, max=10),
        reraise=True,
    )
    async def chat_completion(self, request: ChatRequest) -> ChatResponse:
        """
        Generate chat completion using Claude API.

        API Docs: https://docs.anthropic.com/claude/reference/messages_post
        """
        try:
            # Convert messages to Claude format
            # Claude uses separate system parameter
            messages = []
            system_prompt = request.system_prompt

            for msg in request.messages:
                # Extract system message if present
                if msg["role"] == "system":
                    system_prompt = msg["content"]
                else:
                    messages.append({
                        "role": msg["role"],
                        "content": msg["content"],
                    })

            payload = {
                "model": request.model.value,
                "messages": messages,
                "max_tokens": request.max_tokens or 4096,
                "temperature": request.temperature,
            }

            if system_prompt:
                payload["system"] = system_prompt

            response = await self.client.post(
                "/messages",
                json=payload,
            )

            if response.status_code == 401:
                raise InvalidAPIKeyError(
                    "Invalid Anthropic API key",
                    AIProviderType.CLAUDE,
                )

            if response.status_code == 429:
                retry_after = response.headers.get("retry-after")
                raise RateLimitError(
                    "Claude rate limit exceeded",
                    AIProviderType.CLAUDE,
                    retry_after=int(retry_after) if retry_after else None,
                )

            if response.status_code == 402:
                raise QuotaExceededError(
                    "Claude quota exceeded",
                    AIProviderType.CLAUDE,
                )

            response.raise_for_status()
            data = response.json()

            # Extract text from content blocks
            content = ""
            for block in data["content"]:
                if block["type"] == "text":
                    content += block["text"]

            # Calculate total tokens
            input_tokens = data["usage"]["input_tokens"]
            output_tokens = data["usage"]["output_tokens"]
            total_tokens = input_tokens + output_tokens

            return ChatResponse(
                content=content,
                model=request.model.value,
                tokens_used=total_tokens,
                provider=AIProviderType.CLAUDE,
                finish_reason=data["stop_reason"],
            )

        except (InvalidAPIKeyError, RateLimitError, QuotaExceededError):
            raise
        except Exception as e:
            raise AIProviderError(
                f"Claude chat error: {str(e)}",
                AIProviderType.CLAUDE,
                original_error=e,
            )

    async def validate_credentials(self) -> bool:
        """
        Validate Claude API key by making a small test request.
        """
        try:
            # Make a minimal request to validate key
            test_request = ChatRequest(
                messages=[{"role": "user", "content": "Hi"}],
                model=ChatModel.CLAUDE_3_HAIKU,  # Cheapest model
                user_id="validation",
                max_tokens=10,
            )

            await self.chat_completion(test_request)
            return True

        except InvalidAPIKeyError:
            raise
        except Exception as e:
            raise AIProviderError(
                f"Claude credential validation error: {str(e)}",
                AIProviderType.CLAUDE,
                original_error=e,
            )

    def get_supported_embedding_models(self) -> List[EmbeddingModel]:
        """Claude doesn't provide embeddings."""
        return []

    def get_supported_chat_models(self) -> List[ChatModel]:
        """Return Claude chat models."""
        return self.SUPPORTED_CHAT_MODELS

    def estimate_tokens(self, text: str, model: Optional[str] = None) -> int:
        """
        Estimate token count for Claude.

        Claude uses a similar tokenizer to GPT-4 (cl100k_base).
        Rough estimate: 1 token ≈ 3.5 characters for English text.
        """
        # Simple character-based estimate
        # For code, this is roughly accurate
        return int(len(text) / 3.5)

    async def close(self):
        """Close HTTP client."""
        await self.client.aclose()
