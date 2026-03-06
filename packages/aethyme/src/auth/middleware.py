"""Authentication middleware for FastAPI with strict tenant isolation."""

import json
from dataclasses import dataclass
from typing import Annotated, Any

import structlog
from fastapi import Depends, HTTPException, status
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from .oidc import JWTTokenGenerator, OIDCValidationError, oidc_client

logger = structlog.get_logger(__name__)

# HTTP Bearer token authentication
security = HTTPBearer()


@dataclass
class UserContext:
    """User context from authenticated request.

    This contains all the information needed for authorization
    and tenant isolation.
    """

    user_id: str
    tenant_id: str
    scopes: list[str]
    org_id: str | None = None
    email: str | None = None
    is_api_key: bool = False
    api_key_id: str | None = None

    def has_scope(self, scope: str) -> bool:
        """Check if user has a specific scope.

        Args:
            scope: Scope to check (e.g., 'repo:read', 'repo:write')

        Returns:
            True if user has the scope
        """
        # Admin scope grants all capabilities
        if 'org:admin' in self.scopes:
            return True

        return scope in self.scopes

    def has_any_scope(self, scopes: list[str]) -> bool:
        """Check if user has any of the specified scopes.

        Args:
            scopes: List of scopes to check

        Returns:
            True if user has at least one scope
        """
        if 'org:admin' in self.scopes:
            return True

        return any(scope in self.scopes for scope in scopes)

    def has_all_scopes(self, scopes: list[str]) -> bool:
        """Check if user has all of the specified scopes.

        Args:
            scopes: List of scopes to check

        Returns:
            True if user has all scopes
        """
        if 'org:admin' in self.scopes:
            return True

        return all(scope in self.scopes for scope in scopes)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            'sub': self.user_id,
            'user_id': self.user_id,
            'org_id': self.org_id,
            'tenant_id': self.tenant_id,
            'scopes': self.scopes,
            'email': self.email,
            'is_api_key': self.is_api_key,
        }

    def get(self, key: str, default: Any = None) -> Any:
        """Dict-like accessor used by request handlers."""
        aliases = {
            'sub': self.user_id,
            'user_id': self.user_id,
            'org_id': self.org_id,
            'tenant_id': self.tenant_id,
            'email': self.email,
            'scopes': self.scopes,
            'is_api_key': self.is_api_key,
            'api_key_id': self.api_key_id,
        }
        return aliases.get(key, default)


async def verify_jwt_token(token: str) -> UserContext:
    """Verify JWT token and extract user context.

    Supports both Aethyme-issued tokens and OIDC tokens.

    Args:
        token: JWT token

    Returns:
        UserContext

    Raises:
        HTTPException: If token is invalid
    """
    # Try Aethyme token first
    try:
        payload = JWTTokenGenerator.decode_token(token)

        tenant_id = payload.get('tenant_id') or payload.get('tenant')
        org_id = payload.get('org') or payload.get('org_id')
        if not tenant_id:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="Token missing tenant claim",
                headers={"WWW-Authenticate": "Bearer"},
            )

        return UserContext(
            user_id=payload['sub'],
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=payload.get('scopes', []),
            email=payload.get('email'),
        )

    except Exception as e:
        logger.debug("Not a Aethyme token, trying OIDC", error=str(e))

    # Try OIDC token if configured
    if oidc_client.is_configured:
        try:
            payload = await oidc_client.verify_token(token)

            # Map OIDC claims to Aethyme scopes
            # This is a simplified mapping - customize based on your OIDC provider
            scopes = ['repo:read']  # Default read access

            # Check for additional claims
            if payload.get('realm_access', {}).get('roles'):
                roles = payload['realm_access']['roles']
                if 'admin' in roles:
                    scopes.append('org:admin')
                if 'writer' in roles:
                    scopes.append('repo:write')

            # Extract org_id from token or use default
            org_id = payload.get('org_id') or payload.get('organization')
            tenant_id = payload.get('tenant_id') or payload.get('tenant')
            if not tenant_id:
                raise HTTPException(
                    status_code=status.HTTP_401_UNAUTHORIZED,
                    detail="OIDC token missing tenant claim",
                    headers={"WWW-Authenticate": "Bearer"},
                )

            return UserContext(
                user_id=payload['sub'],
                tenant_id=tenant_id,
                org_id=org_id,
                scopes=scopes,
                email=payload.get('email'),
            )

        except OIDCValidationError as e:
            logger.warning("OIDC token validation failed", error=str(e))
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail=f"Invalid OIDC token: {e}",
                headers={"WWW-Authenticate": "Bearer"},
            ) from e

    # Both methods failed
    raise HTTPException(
        status_code=status.HTTP_401_UNAUTHORIZED,
        detail="Invalid authentication token",
        headers={"WWW-Authenticate": "Bearer"},
    )


async def verify_api_key(api_key: str) -> UserContext:
    """Verify API key and return user context.

    Args:
        api_key: API key string

    Returns:
        UserContext

    Raises:
        HTTPException: If API key is invalid
    """
    from .api_keys import get_api_key_record

    key_data = get_api_key_record(api_key)

    if not key_data:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid API key",
            headers={"WWW-Authenticate": "Bearer"},
        )

    # Extract scopes from JSONB
    scopes = key_data.get('scopes', ['repo:read'])
    if isinstance(scopes, str):
        scopes = json.loads(scopes)

    return UserContext(
        user_id=f"api_key_{key_data['id']}",
        tenant_id=str(key_data['tenant_id']),
        org_id=str(key_data['org_id']) if key_data.get('org_id') else None,
        scopes=scopes,
        email=None,
        is_api_key=True,
        api_key_id=str(key_data['id']),
    )


AuthCredentials = Annotated[HTTPAuthorizationCredentials, Depends(security)]


async def get_current_user(credentials: AuthCredentials) -> UserContext:
    """Get current user from JWT token or API key.

    This is the main dependency for authentication.

    Args:
        credentials: HTTP authorization credentials

    Returns:
        UserContext with user information

    Raises:
        HTTPException: If authentication fails
    """
    token = credentials.credentials

    if token.startswith('rg_'):
        return await verify_api_key(token)

    return await verify_jwt_token(token)


CurrentUser = Annotated[UserContext, Depends(get_current_user)]


def require_scope(scope: str):
    """Dependency to require a specific scope."""
    async def check_scope(user: CurrentUser) -> UserContext:
        if not user.has_scope(scope):
            logger.warning(
                "Insufficient scope",
                user_id=user.user_id,
                required=scope,
                has=user.scopes,
            )
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail=f"Scope '{scope}' required",
            )

        return user

    return check_scope


def require_scopes(scopes: list[str], require_all: bool = True):
    """Dependency to require multiple scopes."""
    async def check_scopes(user: CurrentUser) -> UserContext:
        if require_all:
            has_scopes = user.has_all_scopes(scopes)
            error_msg = f"All scopes required: {', '.join(scopes)}"
        else:
            has_scopes = user.has_any_scope(scopes)
            error_msg = f"At least one scope required: {', '.join(scopes)}"

        if not has_scopes:
            logger.warning(
                "Insufficient scopes",
                user_id=user.user_id,
                required=scopes,
                has=user.scopes,
                require_all=require_all,
            )
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail=error_msg,
            )

        return user

    return check_scopes
