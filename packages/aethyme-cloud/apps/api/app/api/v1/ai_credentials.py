"""
AI Credentials API Endpoints

Endpoints for managing customer AI provider credentials (BYOK).
"""

from typing import List, Optional
from fastapi import APIRouter, Depends, HTTPException, status
from pydantic import BaseModel, Field
from datetime import datetime

from app.core.auth import get_current_user
from app.core.database import get_db
from app.models.user import User
from app.services.ai_credentials_service import AICredentialsService
from app.services.ai import AIProviderType


router = APIRouter()


# Request/Response Models

class CreateCredentialRequest(BaseModel):
    """Request to create AI credentials."""

    provider_type: AIProviderType
    provider_name: Optional[str] = None
    api_key: str
    organization_id: Optional[str] = None  # OpenAI
    resource_name: Optional[str] = None    # Azure
    deployment_name: Optional[str] = None  # Azure
    region: Optional[str] = None           # AWS, GCP
    project_id: Optional[str] = None       # GCP
    endpoint: Optional[str] = None         # Custom endpoints
    validate: bool = Field(default=True, description="Validate credentials before saving")


class UpdateCredentialRequest(BaseModel):
    """Request to update AI credentials."""

    provider_name: Optional[str] = None
    api_key: Optional[str] = None
    organization_id: Optional[str] = None
    resource_name: Optional[str] = None
    deployment_name: Optional[str] = None
    region: Optional[str] = None
    project_id: Optional[str] = None
    endpoint: Optional[str] = None
    is_active: Optional[bool] = None


class CredentialResponse(BaseModel):
    """Response with credential information (API key masked)."""

    id: int
    provider_type: str
    provider_name: str
    is_active: bool
    is_validated: bool
    last_validated_at: Optional[datetime] = None
    validation_error: Optional[str] = None
    last_used_at: Optional[datetime] = None
    total_requests: int
    total_tokens_used: int
    created_at: datetime
    updated_at: Optional[datetime] = None

    # Masked API key (first 8 chars + ***)
    api_key_preview: str

    class Config:
        from_attributes = True


class SupportedModelsResponse(BaseModel):
    """Response with supported models for a provider."""

    provider_type: AIProviderType
    embedding_models: List[str]
    chat_models: List[str]


class UsageSummaryResponse(BaseModel):
    """Response with usage summary."""

    total_tokens: int
    total_requests: int
    by_provider: dict
    period_start: Optional[str] = None
    period_end: Optional[str] = None


# Endpoints

@router.post("/credentials", response_model=CredentialResponse, status_code=status.HTTP_201_CREATED)
async def create_credential(
    request: CreateCredentialRequest,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Create AI provider credentials.

    Stores customer-provided API keys (encrypted) for BYOK architecture.
    Optionally validates credentials before storing.
    """
    service = AICredentialsService(db)

    # Build credentials dict
    credentials_dict = {"api_key": request.api_key}

    if request.organization_id:
        credentials_dict["organization_id"] = request.organization_id
    if request.resource_name:
        credentials_dict["resource_name"] = request.resource_name
    if request.deployment_name:
        credentials_dict["deployment_name"] = request.deployment_name
    if request.region:
        credentials_dict["region"] = request.region
    if request.project_id:
        credentials_dict["project_id"] = request.project_id
    if request.endpoint:
        credentials_dict["endpoint"] = request.endpoint

    try:
        credential = await service.create_credential(
            user_id=current_user.id,
            provider_type=request.provider_type,
            credentials_dict=credentials_dict,
            provider_name=request.provider_name,
            validate=request.validate,
        )

        # Get decrypted credentials for preview
        decrypted = credential.get_credentials_dict()
        api_key = decrypted.get("api_key", "")
        api_key_preview = api_key[:8] + "***" if len(api_key) > 8 else "***"

        return CredentialResponse(
            id=credential.id,
            provider_type=credential.provider_type,
            provider_name=credential.provider_name,
            is_active=credential.is_active,
            is_validated=credential.is_validated,
            last_validated_at=credential.last_validated_at,
            validation_error=credential.validation_error,
            last_used_at=credential.last_used_at,
            total_requests=credential.total_requests,
            total_tokens_used=credential.total_tokens_used,
            created_at=credential.created_at,
            updated_at=credential.updated_at,
            api_key_preview=api_key_preview,
        )

    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=str(e),
        )


@router.get("/credentials", response_model=List[CredentialResponse])
async def list_credentials(
    provider_type: Optional[AIProviderType] = None,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    List user's AI credentials.

    Optionally filter by provider type.
    """
    service = AICredentialsService(db)

    credentials = await service.get_credentials(
        user_id=current_user.id,
        provider_type=provider_type,
        active_only=False,
    )

    results = []
    for credential in credentials:
        # Get decrypted credentials for preview
        decrypted = credential.get_credentials_dict()
        api_key = decrypted.get("api_key", "")
        api_key_preview = api_key[:8] + "***" if len(api_key) > 8 else "***"

        results.append(
            CredentialResponse(
                id=credential.id,
                provider_type=credential.provider_type,
                provider_name=credential.provider_name,
                is_active=credential.is_active,
                is_validated=credential.is_validated,
                last_validated_at=credential.last_validated_at,
                validation_error=credential.validation_error,
                last_used_at=credential.last_used_at,
                total_requests=credential.total_requests,
                total_tokens_used=credential.total_tokens_used,
                created_at=credential.created_at,
                updated_at=credential.updated_at,
                api_key_preview=api_key_preview,
            )
        )

    return results


@router.get("/credentials/{credential_id}", response_model=CredentialResponse)
async def get_credential(
    credential_id: int,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """Get specific AI credential."""
    service = AICredentialsService(db)

    credentials = await service.get_credentials(
        user_id=current_user.id,
        credential_id=credential_id,
        active_only=False,
    )

    if not credentials:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Credential not found",
        )

    credential = credentials[0]

    # Get decrypted credentials for preview
    decrypted = credential.get_credentials_dict()
    api_key = decrypted.get("api_key", "")
    api_key_preview = api_key[:8] + "***" if len(api_key) > 8 else "***"

    return CredentialResponse(
        id=credential.id,
        provider_type=credential.provider_type,
        provider_name=credential.provider_name,
        is_active=credential.is_active,
        is_validated=credential.is_validated,
        last_validated_at=credential.last_validated_at,
        validation_error=credential.validation_error,
        last_used_at=credential.last_used_at,
        total_requests=credential.total_requests,
        total_tokens_used=credential.total_tokens_used,
        created_at=credential.created_at,
        updated_at=credential.updated_at,
        api_key_preview=api_key_preview,
    )


@router.patch("/credentials/{credential_id}", response_model=CredentialResponse)
async def update_credential(
    credential_id: int,
    request: UpdateCredentialRequest,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """Update AI credential."""
    service = AICredentialsService(db)

    # Build credentials dict if API key is being updated
    credentials_dict = None
    if request.api_key:
        credentials_dict = {"api_key": request.api_key}

        if request.organization_id:
            credentials_dict["organization_id"] = request.organization_id
        if request.resource_name:
            credentials_dict["resource_name"] = request.resource_name
        if request.deployment_name:
            credentials_dict["deployment_name"] = request.deployment_name
        if request.region:
            credentials_dict["region"] = request.region
        if request.project_id:
            credentials_dict["project_id"] = request.project_id
        if request.endpoint:
            credentials_dict["endpoint"] = request.endpoint

    try:
        credential = await service.update_credential(
            credential_id=credential_id,
            user_id=current_user.id,
            credentials_dict=credentials_dict,
            provider_name=request.provider_name,
            is_active=request.is_active,
        )

        # Get decrypted credentials for preview
        decrypted = credential.get_credentials_dict()
        api_key = decrypted.get("api_key", "")
        api_key_preview = api_key[:8] + "***" if len(api_key) > 8 else "***"

        return CredentialResponse(
            id=credential.id,
            provider_type=credential.provider_type,
            provider_name=credential.provider_name,
            is_active=credential.is_active,
            is_validated=credential.is_validated,
            last_validated_at=credential.last_validated_at,
            validation_error=credential.validation_error,
            last_used_at=credential.last_used_at,
            total_requests=credential.total_requests,
            total_tokens_used=credential.total_tokens_used,
            created_at=credential.created_at,
            updated_at=credential.updated_at,
            api_key_preview=api_key_preview,
        )

    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=str(e),
        )


@router.delete("/credentials/{credential_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_credential(
    credential_id: int,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """Delete AI credential."""
    service = AICredentialsService(db)

    try:
        await service.delete_credential(
            credential_id=credential_id,
            user_id=current_user.id,
        )
    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=str(e),
        )


@router.post("/credentials/{credential_id}/validate")
async def validate_credential(
    credential_id: int,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Re-validate stored credentials.

    Makes a test API call to verify the credentials still work.
    """
    service = AICredentialsService(db)

    try:
        is_valid, error_message = await service.revalidate_credential(
            credential_id=credential_id,
            user_id=current_user.id,
        )

        return {
            "is_valid": is_valid,
            "error": error_message,
            "validated_at": datetime.utcnow().isoformat(),
        }

    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=str(e),
        )


@router.get("/providers", response_model=List[str])
async def list_supported_providers():
    """List supported AI providers."""
    from app.services.ai import AIProviderFactory

    providers = AIProviderFactory.get_supported_providers()
    return [p.value for p in providers]


@router.get("/providers/{provider_type}/models", response_model=SupportedModelsResponse)
async def get_supported_models(provider_type: AIProviderType):
    """
    Get supported models for a provider.

    Returns embedding and chat models available for the provider.
    """
    from app.services.ai import (
        OpenAIProvider,
        ClaudeProvider,
        AzureOpenAIProvider,
        AIProviderCredentials,
    )

    # Create temporary provider instance (no real credentials needed)
    provider_map = {
        AIProviderType.OPENAI: OpenAIProvider,
        AIProviderType.CLAUDE: ClaudeProvider,
        AIProviderType.AZURE_OPENAI: AzureOpenAIProvider,
    }

    provider_class = provider_map.get(provider_type)
    if not provider_class:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail=f"Provider {provider_type} not supported",
        )

    # Create dummy credentials
    dummy_creds = AIProviderCredentials(
        provider_type=provider_type,
        api_key="dummy",
    )

    try:
        provider = provider_class(dummy_creds)
        embedding_models = [m.value for m in provider.get_supported_embedding_models()]
        chat_models = [m.value for m in provider.get_supported_chat_models()]

        return SupportedModelsResponse(
            provider_type=provider_type,
            embedding_models=embedding_models,
            chat_models=chat_models,
        )
    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=str(e),
        )


@router.get("/usage", response_model=UsageSummaryResponse)
async def get_usage_summary(
    start_date: Optional[str] = None,
    end_date: Optional[str] = None,
    current_user: User = Depends(get_current_user),
    db=Depends(get_db),
):
    """
    Get AI usage summary for the current user.

    Optionally filter by date range.
    """
    service = AICredentialsService(db)

    # Parse dates
    start_dt = datetime.fromisoformat(start_date) if start_date else None
    end_dt = datetime.fromisoformat(end_date) if end_date else None

    summary = await service.get_usage_summary(
        user_id=current_user.id,
        start_date=start_dt,
        end_date=end_dt,
    )

    return UsageSummaryResponse(**summary)
