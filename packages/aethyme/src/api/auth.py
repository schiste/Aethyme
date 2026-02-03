"""Authentication and authorization for Aethyme API."""

from typing import Optional, Dict, Any
from datetime import datetime, timedelta
from fastapi import Depends, HTTPException, status
from fastapi.security import HTTPBearer, HTTPAuthorizationCredentials
from jose import JWTError, jwt
from passlib.context import CryptContext
import structlog

from ..config import settings
from ..graph.store import GraphStore
from ..graph.connection_pool import db_pool

logger = structlog.get_logger(__name__)

# Password hashing
pwd_context = CryptContext(schemes=["bcrypt"], deprecated="auto")

# HTTP Bearer token authentication
security = HTTPBearer()


class AuthenticationError(Exception):
    """Custom authentication error."""
    pass


def create_access_token(
    data: Dict[str, Any],
    expires_delta: Optional[timedelta] = None
) -> str:
    """
    Create a JWT access token.

    Args:
        data: Data to encode in the token
        expires_delta: Token expiration time

    Returns:
        Encoded JWT token
    """
    to_encode = data.copy()

    if expires_delta:
        expire = datetime.utcnow() + expires_delta
    else:
        expire = datetime.utcnow() + timedelta(seconds=settings.jwt_expiration_delta)

    to_encode.update({"exp": expire})

    encoded_jwt = jwt.encode(
        to_encode,
        settings.jwt_secret_key,
        algorithm=settings.jwt_algorithm
    )

    return encoded_jwt


def decode_token(token: str) -> Dict[str, Any]:
    """
    Decode and validate a JWT token.

    Args:
        token: JWT token to decode

    Returns:
        Token payload

    Raises:
        AuthenticationError: If token is invalid
    """
    try:
        payload = jwt.decode(
            token,
            settings.jwt_secret_key,
            algorithms=[settings.jwt_algorithm]
        )
        return payload
    except JWTError as e:
        logger.warning("Invalid JWT token", error=str(e))
        raise AuthenticationError("Invalid authentication token")


def hash_password(password: str) -> str:
    """Hash a password using bcrypt."""
    return pwd_context.hash(password)


def verify_password(plain_password: str, hashed_password: str) -> bool:
    """Verify a password against its hash."""
    return pwd_context.verify(plain_password, hashed_password)


class User:
    """User model for authentication."""

    def __init__(
        self,
        user_id: str,
        tenant_id: str,
        email: str,
        permissions: Optional[list] = None
    ):
        self.user_id = user_id
        self.tenant_id = tenant_id
        self.email = email
        self.permissions = permissions or ["read"]

    def has_permission(self, permission: str) -> bool:
        """Check if user has a specific permission."""
        return permission in self.permissions or "admin" in self.permissions

    def to_dict(self) -> Dict[str, Any]:
        """Convert user to dictionary."""
        return {
            "user_id": self.user_id,
            "tenant_id": self.tenant_id,
            "email": self.email,
            "permissions": self.permissions,
        }


async def get_current_user(
    credentials: HTTPAuthorizationCredentials = Depends(security)
) -> User:
    """
    Get the current authenticated user from JWT token.

    Args:
        credentials: HTTP authorization credentials

    Returns:
        Current user

    Raises:
        HTTPException: If authentication fails
    """
    token = credentials.credentials

    try:
        payload = decode_token(token)

        # Extract user information from token
        user_id = payload.get("sub")
        tenant_id = payload.get("tenant_id")
        email = payload.get("email")
        permissions = payload.get("permissions", ["read"])

        if not user_id or not tenant_id:
            raise AuthenticationError("Invalid token payload")

        user = User(
            user_id=user_id,
            tenant_id=tenant_id,
            email=email,
            permissions=permissions
        )

        logger.debug(
            "User authenticated",
            user_id=user_id,
            tenant_id=tenant_id,
        )

        return user

    except AuthenticationError as e:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail=str(e),
            headers={"WWW-Authenticate": "Bearer"},
        )
    except Exception as e:
        logger.error("Authentication error", error=str(e))
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Could not validate credentials",
            headers={"WWW-Authenticate": "Bearer"},
        )


def require_permission(permission: str):
    """
    Dependency to require a specific permission.

    Usage:
        @router.post("/", dependencies=[Depends(require_permission("write"))])

    Args:
        permission: Required permission

    Returns:
        Dependency function
    """
    async def check_permission(user: User = Depends(get_current_user)):
        if not user.has_permission(permission):
            raise HTTPException(
                status_code=status.HTTP_403_FORBIDDEN,
                detail=f"Permission '{permission}' required"
            )
        return user

    return check_permission


class APIKey:
    """API key authentication (alternative to JWT)."""

    @staticmethod
    def verify_api_key(api_key: str) -> Optional[Dict[str, Any]]:
        """
        Verify an API key and return associated metadata.

        Args:
            api_key: API key to verify

        Returns:
            API key metadata if valid, None otherwise
        """
        # Hash the API key to compare with database
        key_hash = hash_password(api_key)

        # Query database for API key
        query = """
            SELECT ak.*, t.name as tenant_name
            FROM aethyme.api_keys ak
            JOIN aethyme.tenants t ON t.id = ak.tenant_id
            WHERE ak.key_hash = %s
              AND (ak.expires_at IS NULL OR ak.expires_at > NOW())
              AND ak.revoked_at IS NULL
        """

        result = db_pool.execute(query, (key_hash,))

        if result:
            key_data = result[0]

            # Update last used timestamp
            update_query = """
                UPDATE aethyme.api_keys
                SET last_used_at = NOW()
                WHERE id = %s
            """
            db_pool.execute(update_query, (key_data["id"],), fetch=False)

            return key_data

        return None


async def get_current_user_or_api_key(
    credentials: Optional[HTTPAuthorizationCredentials] = Depends(security)
) -> User:
    """
    Get current user from JWT token or API key.

    This allows both JWT and API key authentication.

    Args:
        credentials: HTTP authorization credentials

    Returns:
        Current user

    Raises:
        HTTPException: If authentication fails
    """
    if not credentials:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Authentication required",
            headers={"WWW-Authenticate": "Bearer"},
        )

    token = credentials.credentials

    # Check if it's an API key (longer than typical JWT)
    if len(token) > 200:  # JWTs are typically < 200 chars
        # Try JWT first
        try:
            return await get_current_user(credentials)
        except HTTPException:
            pass

    # Try as API key
    key_data = APIKey.verify_api_key(token)
    if key_data:
        return User(
            user_id=f"api_key_{key_data['id']}",
            tenant_id=str(key_data["tenant_id"]),
            email=f"{key_data['name']}@api-key",
            permissions=key_data.get("permissions", ["read"]),
        )

    # If neither JWT nor API key worked
    raise HTTPException(
        status_code=status.HTTP_401_UNAUTHORIZED,
        detail="Invalid authentication credentials",
        headers={"WWW-Authenticate": "Bearer"},
    )


async def get_current_user_optional(
    credentials: Optional[HTTPAuthorizationCredentials] = Depends(HTTPBearer(auto_error=False))
) -> Optional[Dict[str, Any]]:
    """
    Get the current user if authenticated, None otherwise.

    This is for endpoints that work with or without authentication.

    Args:
        credentials: HTTP authorization credentials (optional)

    Returns:
        User dictionary if authenticated, None otherwise
    """
    if not credentials:
        return None

    try:
        token = credentials.credentials
        payload = decode_token(token)
        return payload
    except Exception:
        # Silently ignore authentication errors for optional auth
        return None


# Dependency aliases for common use cases
jwt_required = get_current_user
jwt_or_api_key = get_current_user_or_api_key
write_permission = require_permission("write")
admin_permission = require_permission("admin")