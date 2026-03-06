"""Authentication and authorization module for Aethyme."""

from datetime import timedelta
from typing import Annotated, Any

from fastapi import Depends
from passlib.context import CryptContext

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

pwd_context = CryptContext(schemes=["pbkdf2_sha256"], deprecated="auto")
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


def hash_password(password: str) -> str:
    """Hash a password using the canonical local password scheme."""
    return pwd_context.hash(password)


def verify_password(plain_password: str, hashed_password: str) -> bool:
    """Verify a password against its hash."""
    return pwd_context.verify(plain_password, hashed_password)


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
    'hash_password',
    'verify_password',
    # API Keys
    'APIKeyManager',
    'generate_api_key',
    'get_api_key_record',
    'hash_api_key',
]
