"""Security utilities for authentication and authorization."""

from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any, Dict, Optional

import bcrypt
from fastapi import WebSocket, Depends, HTTPException, status
from jose import JWTError, jwt
from fastapi.security import OAuth2PasswordBearer
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.config import settings

if TYPE_CHECKING:
    from app.models.user import User

# OAuth2 password bearer for token extraction
oauth2_scheme = OAuth2PasswordBearer(tokenUrl="/api/auth/login")


def verify_password(plain_password: str, hashed_password: str) -> bool:
    """Verify a password against its hash."""
    # Truncate to 72 bytes for bcrypt compatibility
    password_bytes = plain_password.encode('utf-8')[:72]
    return bcrypt.checkpw(
        password_bytes,
        hashed_password.encode('utf-8')
    )


def get_password_hash(password: str) -> str:
    """Hash a password for storing."""
    # Truncate password to 72 bytes for bcrypt compatibility
    password_bytes = password.encode('utf-8')[:72]
    salt = bcrypt.gensalt()
    hashed = bcrypt.hashpw(password_bytes, salt)
    return hashed.decode('utf-8')


def build_user_access_claims(user: "User") -> Dict[str, Any]:
    """Build a bearer token payload that Aethyme core can enforce.

    Cloud does not yet model a separate tenant entity, so the organization id
    temporarily serves as both the `org` and `tenant_id` claim.
    """
    scopes = ["repo:read", "repo:write"]
    if user.is_superuser:
        scopes.append("org:admin")

    claims: Dict[str, Any] = {
        "sub": user.id,
        "email": user.email,
        "scopes": scopes,
    }

    if user.organization_id:
        claims["org"] = user.organization_id
        claims["org_id"] = user.organization_id
        claims["tenant_id"] = user.organization_id

    return claims


def create_access_token(data: Dict[str, Any], expires_delta: Optional[timedelta] = None) -> str:
    """
    Create a JWT access token.

    Args:
        data: The payload data to encode in the token
        expires_delta: Optional custom expiration time

    Returns:
        Encoded JWT token string
    """
    to_encode = data.copy()

    now = datetime.now(UTC)
    if expires_delta:
        expire = now + expires_delta
    else:
        expire = now + timedelta(minutes=settings.JWT_EXPIRATION_MINUTES)

    to_encode.update(
        {
            "exp": expire,
            "iat": int(now.timestamp()),
            "iss": "aethyme",
            "type": "access",
        }
    )
    encoded_jwt = jwt.encode(to_encode, settings.JWT_SECRET_KEY, algorithm=settings.JWT_ALGORITHM)
    return encoded_jwt


def create_refresh_token(data: Dict[str, Any]) -> str:
    """
    Create a JWT refresh token.

    Args:
        data: The payload data to encode in the token

    Returns:
        Encoded JWT refresh token string
    """
    to_encode = data.copy()
    now = datetime.now(UTC)
    expire = now + timedelta(days=settings.REFRESH_TOKEN_EXPIRATION_DAYS)

    to_encode.update(
        {
            "exp": expire,
            "iat": int(now.timestamp()),
            "iss": "aethyme",
            "type": "refresh",
        }
    )
    encoded_jwt = jwt.encode(
        to_encode,
        settings.REFRESH_TOKEN_SECRET_KEY,
        algorithm=settings.JWT_ALGORITHM
    )
    return encoded_jwt


def decode_access_token(token: str) -> Optional[Dict[str, Any]]:
    """
    Decode and verify a JWT access token.

    Args:
        token: The JWT token to decode

    Returns:
        The decoded token payload, or None if invalid
    """
    try:
        payload = jwt.decode(
            token,
            settings.JWT_SECRET_KEY,
            algorithms=[settings.JWT_ALGORITHM]
        )

        # Verify it's an access token
        if payload.get("type") != "access":
            return None

        return payload
    except JWTError:
        return None


def decode_refresh_token(token: str) -> Optional[Dict[str, Any]]:
    """
    Decode and verify a JWT refresh token.

    Args:
        token: The JWT refresh token to decode

    Returns:
        The decoded token payload, or None if invalid
    """
    try:
        payload = jwt.decode(
            token,
            settings.REFRESH_TOKEN_SECRET_KEY,
            algorithms=[settings.JWT_ALGORITHM]
        )

        # Verify it's a refresh token
        if payload.get("type") != "refresh":
            return None

        return payload
    except JWTError:
        return None


async def get_current_user(
    token: str = Depends(oauth2_scheme),
    db: AsyncSession = None,
):
    """
    Get current authenticated user from JWT token (for REST API).

    Args:
        token: JWT token from Authorization header
        db: Database session (injected by FastAPI)

    Returns:
        User object if authenticated

    Raises:
        HTTPException: If authentication fails
    """
    credentials_exception = HTTPException(
        status_code=status.HTTP_401_UNAUTHORIZED,
        detail="Could not validate credentials",
        headers={"WWW-Authenticate": "Bearer"},
    )

    # Decode token
    payload = decode_access_token(token)
    if not payload:
        raise credentials_exception

    user_id = payload.get("sub")
    if not user_id:
        raise credentials_exception

    # Get user from database
    from app.core.database import AsyncSessionLocal, get_db
    from app.models.user import User

    # Use provided db session or create new one
    if db is None:
        async with AsyncSessionLocal() as db:
            result = await db.execute(select(User).where(User.id == user_id))
            user = result.scalar_one_or_none()
    else:
        result = await db.execute(select(User).where(User.id == user_id))
        user = result.scalar_one_or_none()

    if not user or not user.is_active:
        raise credentials_exception

    return user


async def get_current_user_ws(websocket: WebSocket, token: Optional[str] = None):
    """
    Authenticate WebSocket connection using JWT token.

    Args:
        websocket: FastAPI WebSocket instance
        token: JWT token from query parameter

    Returns:
        User object if authenticated, None otherwise
    """
    if not token:
        return None

    # Decode token
    payload = decode_access_token(token)
    if not payload:
        return None

    user_id = payload.get("sub")
    if not user_id:
        return None

    # Get user from database
    from app.core.database import AsyncSessionLocal
    from app.models.user import User

    async with AsyncSessionLocal() as db:
        result = await db.execute(select(User).where(User.id == user_id))
        user = result.scalar_one_or_none()

        if not user or not user.is_active:
            return None

        return user
