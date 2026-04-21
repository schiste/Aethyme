"""
AI Credentials Service

Manages customer AI provider credentials (BYOK architecture).
Handles encryption, validation, and storage of API keys.
"""

from typing import List, Optional, Dict, Any
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select
from datetime import datetime

from app.models.ai_credentials import AICredential, AIUsageLog
from app.services.ai import (
    AIProviderFactory,
    AIProviderType,
    AIProviderCredentials,
    InvalidAPIKeyError,
    AIProviderError,
)


class AICredentialsService:
    """Service for managing AI provider credentials."""

    def __init__(self, db: AsyncSession):
        self.db = db

    async def create_credential(
        self,
        user_id: int,
        provider_type: AIProviderType,
        credentials_dict: dict,
        provider_name: Optional[str] = None,
        validate: bool = True,
    ) -> AICredential:
        """
        Create and store AI provider credentials.

        Args:
            user_id: User ID
            provider_type: AI provider type (openai, claude, etc.)
            credentials_dict: Provider-specific credentials
                OpenAI: {"api_key": "sk-...", "organization_id": "org-..."}
                Claude: {"api_key": "sk-ant-..."}
                Azure: {"api_key": "...", "resource_name": "...", "deployment_name": "..."}
            provider_name: User-friendly name for this credential
            validate: Whether to validate credentials before storing

        Returns:
            Created AICredential

        Raises:
            InvalidAPIKeyError: If validation fails
        """
        # Validate credentials if requested
        if validate:
            is_valid = await self.validate_credentials(provider_type, credentials_dict)
            if not is_valid:
                raise InvalidAPIKeyError(
                    f"Invalid {provider_type.value} credentials",
                    provider_type,
                )

        # Create credential record
        credential = AICredential(
            user_id=user_id,
            provider_type=provider_type.value,
            provider_name=provider_name or f"{provider_type.value} API Key",
            is_active=True,
            is_validated=validate,
            last_validated_at=datetime.utcnow() if validate else None,
        )

        # Encrypt and store credentials
        credential.set_credentials_dict(credentials_dict)

        self.db.add(credential)
        await self.db.commit()
        await self.db.refresh(credential)

        return credential

    async def get_credentials(
        self,
        user_id: int,
        provider_type: Optional[AIProviderType] = None,
        credential_id: Optional[int] = None,
        active_only: bool = True,
    ) -> List[AICredential]:
        """
        Get user's AI credentials.

        Args:
            user_id: User ID
            provider_type: Filter by provider type (optional)
            credential_id: Get specific credential (optional)
            active_only: Only return active credentials

        Returns:
            List of AICredential records
        """
        query = select(AICredential).where(AICredential.user_id == user_id)

        if credential_id:
            query = query.where(AICredential.id == credential_id)

        if provider_type:
            query = query.where(AICredential.provider_type == provider_type.value)

        if active_only:
            query = query.where(AICredential.is_active.is_(True))

        result = await self.db.execute(query)
        return result.scalars().all()

    async def update_credential(
        self,
        credential_id: int,
        user_id: int,
        credentials_dict: Optional[dict] = None,
        provider_name: Optional[str] = None,
        is_active: Optional[bool] = None,
    ) -> AICredential:
        """
        Update AI credential.

        Args:
            credential_id: Credential ID
            user_id: User ID (for authorization)
            credentials_dict: New credentials (optional)
            provider_name: New provider name (optional)
            is_active: Active status (optional)

        Returns:
            Updated AICredential
        """
        # Get credential
        credentials = await self.get_credentials(
            user_id=user_id,
            credential_id=credential_id,
            active_only=False,
        )

        if not credentials:
            raise ValueError(f"Credential {credential_id} not found")

        credential = credentials[0]

        # Update fields
        if credentials_dict:
            credential.set_credentials_dict(credentials_dict)
            credential.is_validated = False  # Needs re-validation
            credential.validation_error = None

        if provider_name:
            credential.provider_name = provider_name

        if is_active is not None:
            credential.is_active = is_active

        credential.updated_at = datetime.utcnow()

        await self.db.commit()
        await self.db.refresh(credential)

        return credential

    async def delete_credential(self, credential_id: int, user_id: int):
        """
        Delete AI credential.

        Args:
            credential_id: Credential ID
            user_id: User ID (for authorization)
        """
        credentials = await self.get_credentials(
            user_id=user_id,
            credential_id=credential_id,
            active_only=False,
        )

        if not credentials:
            raise ValueError(f"Credential {credential_id} not found")

        credential = credentials[0]
        await self.db.delete(credential)
        await self.db.commit()

    async def validate_credentials(
        self,
        provider_type: AIProviderType,
        credentials_dict: dict,
    ) -> bool:
        """
        Validate AI provider credentials by making a test API call.

        Args:
            provider_type: Provider type
            credentials_dict: Provider-specific credentials

        Returns:
            True if valid, False otherwise
        """
        try:
            # Create provider credentials
            provider_creds = AIProviderCredentials(
                provider_type=provider_type,
                api_key=credentials_dict.get("api_key"),
                organization_id=credentials_dict.get("organization_id"),
                resource_name=credentials_dict.get("resource_name"),
                deployment_name=credentials_dict.get("deployment_name"),
                region=credentials_dict.get("region"),
                project_id=credentials_dict.get("project_id"),
                endpoint=credentials_dict.get("endpoint"),
            )

            # Create provider instance
            provider = AIProviderFactory.create(provider_creds)

            # Validate
            is_valid = await provider.validate_credentials()
            await provider.close()

            return is_valid

        except (InvalidAPIKeyError, AIProviderError):
            return False
        except Exception:
            return False

    async def revalidate_credential(
        self,
        credential_id: int,
        user_id: int,
    ) -> tuple[bool, Optional[str]]:
        """
        Re-validate stored credentials.

        Args:
            credential_id: Credential ID
            user_id: User ID

        Returns:
            Tuple of (is_valid, error_message)
        """
        credentials = await self.get_credentials(
            user_id=user_id,
            credential_id=credential_id,
            active_only=False,
        )

        if not credentials:
            raise ValueError(f"Credential {credential_id} not found")

        credential = credentials[0]

        try:
            # Get provider type
            provider_type = AIProviderType(credential.provider_type)

            # Decrypt credentials
            credentials_dict = credential.get_credentials_dict()

            # Validate
            is_valid = await self.validate_credentials(provider_type, credentials_dict)

            # Update credential
            credential.is_validated = is_valid
            credential.last_validated_at = datetime.utcnow()

            if is_valid:
                credential.validation_error = None
            else:
                credential.validation_error = "Validation failed"

            await self.db.commit()

            return is_valid, credential.validation_error

        except Exception as e:
            error_message = str(e)
            credential.is_validated = False
            credential.validation_error = error_message
            credential.last_validated_at = datetime.utcnow()
            await self.db.commit()

            return False, error_message

    async def get_provider_for_user(
        self,
        user_id: int,
        provider_type: AIProviderType,
        credential_id: Optional[int] = None,
    ):
        """
        Get AI provider instance for a user.

        Args:
            user_id: User ID
            provider_type: Provider type
            credential_id: Specific credential ID (optional, uses first active if not provided)

        Returns:
            AIProvider instance

        Raises:
            ValueError: If no credentials found
        """
        # Get credentials
        credentials = await self.get_credentials(
            user_id=user_id,
            provider_type=provider_type,
            credential_id=credential_id,
            active_only=True,
        )

        if not credentials:
            raise ValueError(
                f"No active {provider_type.value} credentials found for user {user_id}. "
                "Please add your API key in settings."
            )

        credential = credentials[0]

        # Decrypt credentials
        credentials_dict = credential.get_credentials_dict()

        # Create provider credentials
        provider_creds = AIProviderCredentials(
            provider_type=provider_type,
            api_key=credentials_dict.get("api_key"),
            organization_id=credentials_dict.get("organization_id"),
            resource_name=credentials_dict.get("resource_name"),
            deployment_name=credentials_dict.get("deployment_name"),
            region=credentials_dict.get("region"),
            project_id=credentials_dict.get("project_id"),
            endpoint=credentials_dict.get("endpoint"),
        )

        # Create provider instance
        provider = AIProviderFactory.create(provider_creds)

        # Update last used timestamp
        credential.last_used_at = datetime.utcnow()
        await self.db.commit()

        return provider, credential

    async def log_usage(
        self,
        user_id: int,
        credential_id: int,
        provider_type: AIProviderType,
        operation_type: str,
        model: str,
        tokens_used: int,
        estimated_cost: Optional[str] = None,
        repository_id: Optional[str] = None,
        session_id: Optional[str] = None,
    ):
        """
        Log AI API usage.

        Args:
            user_id: User ID
            credential_id: Credential ID
            provider_type: Provider type
            operation_type: Operation type (embedding, chat_completion)
            model: Model name
            tokens_used: Tokens used
            estimated_cost: Estimated cost in USD (optional)
            repository_id: Repository ID (optional)
            session_id: Session ID (optional)
        """
        usage_log = AIUsageLog(
            user_id=user_id,
            credential_id=credential_id,
            provider_type=provider_type.value,
            operation_type=operation_type,
            model=model,
            tokens_used=tokens_used,
            estimated_cost=estimated_cost,
            repository_id=repository_id,
            session_id=session_id,
        )

        self.db.add(usage_log)

        # Update credential usage stats
        query = select(AICredential).where(AICredential.id == credential_id)
        result = await self.db.execute(query)
        credential = result.scalar_one_or_none()

        if credential:
            credential.total_requests += 1
            credential.total_tokens_used += tokens_used

        await self.db.commit()

    async def get_usage_summary(
        self,
        user_id: int,
        start_date: Optional[datetime] = None,
        end_date: Optional[datetime] = None,
    ) -> Dict[str, Any]:
        """
        Get usage summary for a user.

        Args:
            user_id: User ID
            start_date: Start date (optional)
            end_date: End date (optional)

        Returns:
            Usage summary dictionary
        """
        query = select(AIUsageLog).where(AIUsageLog.user_id == user_id)

        if start_date:
            query = query.where(AIUsageLog.created_at >= start_date)

        if end_date:
            query = query.where(AIUsageLog.created_at <= end_date)

        result = await self.db.execute(query)
        logs = result.scalars().all()

        # Calculate summary
        total_tokens = sum(log.tokens_used for log in logs)
        total_requests = len(logs)

        by_provider = {}
        for log in logs:
            provider = log.provider_type
            if provider not in by_provider:
                by_provider[provider] = {"tokens": 0, "requests": 0}

            by_provider[provider]["tokens"] += log.tokens_used
            by_provider[provider]["requests"] += 1

        return {
            "total_tokens": total_tokens,
            "total_requests": total_requests,
            "by_provider": by_provider,
            "period_start": start_date.isoformat() if start_date else None,
            "period_end": end_date.isoformat() if end_date else None,
        }
