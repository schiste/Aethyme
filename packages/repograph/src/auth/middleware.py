"""Authentication middleware for FastAPI with scope checking and tenant isolation."""

from typing import Optional, List, Callable
from dataclasses import dataclass
from functools import wraps

from fastapi import Depends, HTTPException, status, Request
from fastapi.security import HTTPBearer, HTTPAuthorizationCredentials
import structlog

from .oidc import JWTTokenGenerator, oidc_client, OIDCValidationError
from ..graph.connection_pool import db_pool
from ..config import settings

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
    org_id: str
    scopes: List[str]
    email: Optional[str] = None
    is_api_key: bool = False
    api_key_id: Optional[str] = None

    def has_scope(self, scope: str) -> bool:
        """Check if user has a specific scope.

        Args:
            scope: Scope to check (e.g., 'repo:read', 'repo:write')

        Returns:
            True if user has the scope
        """
        # Admin scope grants all permissions
        if 'org:admin' in self.scopes:
            return True

        return scope in self.scopes

    def has_any_scope(self, scopes: List[str]) -> bool:
        """Check if user has any of the specified scopes.

        Args:
            scopes: List of scopes to check

        Returns:
            True if user has at least one scope
        """
        if 'org:admin' in self.scopes:
            return True

        return any(scope in self.scopes for scope in scopes)

    def has_all_scopes(self, scopes: List[str]) -> bool:
        """Check if user has all of the specified scopes.

        Args:
            scopes: List of scopes to check

        Returns:
            True if user has all scopes
        """
        if 'org:admin' in self.scopes:
            return True

        return all(scope in self.scopes for scope in scopes)

    def to_dict(self) -> dict:
        """Convert to dictionary."""
        return {
            'user_id': self.user_id,
            'org_id': self.org_id,
            'scopes': self.scopes,
            'email': self.email,
            'is_api_key': self.is_api_key,
        }


async def verify_jwt_token(token: str) -> UserContext:
    """Verify JWT token and extract user context.

    Supports both RepoGraph-issued tokens and OIDC tokens.

    Args:
        token: JWT token

    Returns:
        UserContext

    Raises:
        HTTPException: If token is invalid
    """
    # Try RepoGraph token first
    try:
        payload = JWTTokenGenerator.decode_token(token)

        return UserContext(
            user_id=payload['sub'],
            org_id=payload['org'],
            scopes=payload.get('scopes', []),
            email=payload.get('email'),
        )

    except Exception as e:
        logger.debug("Not a RepoGraph token, trying OIDC", error=str(e))

    # Try OIDC token if configured
    if oidc_client.is_configured:
        try:
            payload = await oidc_client.verify_token(token)

            # Map OIDC claims to RepoGraph scopes
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
            if not org_id:
                # Look up org by email domain or use default
                org_id = 'default'

            return UserContext(
                user_id=payload['sub'],
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
            )

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
    from .api_keys import hash_api_key

    key_hash = hash_api_key(api_key)

    # Query database for API key
    query = """
        SELECT ak.*, t.name as tenant_name
        FROM repograph.api_keys ak
        JOIN repograph.tenants t ON t.id = ak.tenant_id
        WHERE ak.key_hash = %s
          AND (ak.expires_at IS NULL OR ak.expires_at > NOW())
          AND ak.revoked_at IS NULL
    """

    result = db_pool.execute(query, (key_hash,))

    if not result:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid API key",
            headers={"WWW-Authenticate": "Bearer"},
        )

    key_data = result[0]

    # Update last used timestamp
    update_query = """
        UPDATE repograph.api_keys
        SET last_used_at = NOW()
        WHERE id = %s
    """
    db_pool.execute(update_query, (key_data['id'],), fetch=False)

    # Extract scopes from JSONB
    scopes = key_data.get('scopes', ['repo:read'])
    if isinstance(scopes, str):
        import json
        scopes = json.loads(scopes)

    return UserContext(
        user_id=f"api_key_{key_data['id']}",
        org_id=str(key_data['tenant_id']),
        scopes=scopes,
        email=None,
        is_api_key=True,
        api_key_id=str(key_data['id']),
    )


async def get_current_user(
    credentials: HTTPAuthorizationCredentials = Depends(security),
) -> UserContext:
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

    # Detect token type by prefix
    if token.startswith('rg_'):
        # API key (format: rg_live_... or rg_test_...)
        return await verify_api_key(token)
    else:
        # JWT token
        return await verify_jwt_token(token)


async def set_tenant_context(user: UserContext) -> None:
    """Set PostgreSQL session variables for RLS policies.

    This must be called before any database queries to ensure
    tenant isolation via Row-Level Security.

    Args:
        user: User context with org_id
    """
    # Set session variable for RLS
    query = "SET app.current_tenant = %s"

    try:
        db_pool.execute(query, (user.org_id,), fetch=False)
        logger.debug("Tenant context set", org_id=user.org_id)
    except Exception as e:
        logger.error("Failed to set tenant context", error=str(e), org_id=user.org_id)
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to set tenant context",
        )


def require_scope(scope: str) -> Callable:
    """Dependency to require a specific scope.

    Usage:
        @router.post("/", dependencies=[Depends(require_scope("repo:write"))])

    Args:
        scope: Required scope (e.g., 'repo:write')

    Returns:
        Dependency function
    """

    async def check_scope(user: UserContext = Depends(get_current_user)):
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

        # Set tenant context for RLS
        await set_tenant_context(user)

        return user

    return check_scope


def require_scopes(scopes: List[str], require_all: bool = True) -> Callable:
    """Dependency to require multiple scopes.

    Usage:
        @router.post("/", dependencies=[Depends(require_scopes(["repo:read", "repo:write"]))])

    Args:
        scopes: List of required scopes
        require_all: If True, require all scopes. If False, require at least one.

    Returns:
        Dependency function
    """

    async def check_scopes(user: UserContext = Depends(get_current_user)):
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

        # Set tenant context for RLS
        await set_tenant_context(user)

        return user

    return check_scopes


def scope_checker(required_scopes: List[str], require_all: bool = True):
    """Decorator for route handlers to check scopes.

    This is an alternative to using dependencies.

    Usage:
        @router.post("/")
        @scope_checker(['repo:write'])
        async def create_resource(user: UserContext):
            ...

    Args:
        required_scopes: List of required scopes
        require_all: If True, require all scopes. If False, require at least one.

    Returns:
        Decorator function
    """

    def decorator(func):
        @wraps(func)
        async def wrapper(*args, **kwargs):
            # Extract user from kwargs
            user = kwargs.get('user')
            if not user or not isinstance(user, UserContext):
                raise HTTPException(
                    status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
                    detail="User context not available",
                )

            # Check scopes
            if require_all:
                has_scopes = user.has_all_scopes(required_scopes)
                error_msg = f"All scopes required: {', '.join(required_scopes)}"
            else:
                has_scopes = user.has_any_scope(required_scopes)
                error_msg = f"At least one scope required: {', '.join(required_scopes)}"

            if not has_scopes:
                logger.warning(
                    "Insufficient scopes",
                    user_id=user.user_id,
                    required=required_scopes,
                    has=user.scopes,
                    require_all=require_all,
                )
                raise HTTPException(
                    status_code=status.HTTP_403_FORBIDDEN,
                    detail=error_msg,
                )

            return await func(*args, **kwargs)

        return wrapper

    return decorator


# Convenience aliases
jwt_required = get_current_user
read_scope = require_scope('repo:read')
write_scope = require_scope('repo:write')
admin_scope = require_scope('org:admin')
