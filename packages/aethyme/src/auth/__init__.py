"""Authentication and authorization module for Aethyme."""

from datetime import timedelta
from typing import Annotated, Any

from fastapi import Depends

from .api_keys import (
    APIKeyManager,
    generate_api_key,
    get_api_key_record,
    hash_api_key,
)
from .middleware import (
    CurrentUser,
    UserContext,
    get_current_user,
    require_scope,
    require_scopes,
)
from .oidc import (
    JWTTokenGenerator,
    OIDCClient,
    OIDCConfigurationError,
    OIDCError,
    OIDCValidationError,
    oidc_client,
)

RepoReadUser = Annotated[UserContext, Depends(require_scope("repo:read"))]
RepoWriteUser = Annotated[UserContext, Depends(require_scope("repo:write"))]
OrgAdminUser = Annotated[UserContext, Depends(require_scope("org:admin"))]


def create_access_token(
    data: dict[str, Any],
    expires_delta: timedelta | None = None,
) -> str:
    """Create a canonical Aethyme JWT token."""
    return JWTTokenGenerator.create_token(
        user_id=data["sub"],
        tenant_id=data["tenant_id"],
        scopes=data.get("scopes", ["repo:read"]),
        org_id=data.get("org"),
        email=data.get("email"),
        expires_delta=expires_delta,
    )


async def jwt_or_api_key(user: CurrentUser) -> UserContext:
    """Explicit alias for handlers that accept JWT or API key auth."""
    return user


User = UserContext

__all__ = [
    # OIDC
    'OIDCClient',
    'OIDCError',
    'OIDCConfigurationError',
    'OIDCValidationError',
    'JWTTokenGenerator',
    'oidc_client',
    # Middleware
    'get_current_user',
    'jwt_or_api_key',
    'require_scope',
    'require_scopes',
    'CurrentUser',
    'RepoReadUser',
    'RepoWriteUser',
    'OrgAdminUser',
    'UserContext',
    'User',
    'create_access_token',
    # API Keys
    'APIKeyManager',
    'generate_api_key',
    'get_api_key_record',
    'hash_api_key',
]
