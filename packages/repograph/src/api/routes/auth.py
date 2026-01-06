"""Authentication API routes."""

from typing import Dict, Any
from datetime import timedelta
from fastapi import APIRouter, Depends, HTTPException, status
from pydantic import BaseModel, EmailStr, Field
import uuid

from ...graph.connection_pool import db_pool
from ..auth import (
    create_access_token,
    hash_password,
    verify_password,
    User,
    get_current_user,
)

router = APIRouter()


class LoginRequest(BaseModel):
    """Request model for login."""

    email: EmailStr
    password: str = Field(..., min_length=8)


class LoginResponse(BaseModel):
    """Response model for login."""

    access_token: str
    token_type: str = "bearer"
    expires_in: int
    user: Dict[str, Any]


class RegisterRequest(BaseModel):
    """Request model for registration."""

    email: EmailStr
    password: str = Field(..., min_length=8)
    tenant_name: str = Field(..., min_length=1, max_length=255)


class CreateAPIKeyRequest(BaseModel):
    """Request model for API key creation."""

    name: str = Field(..., min_length=1, max_length=255)
    permissions: list = Field(default=["read"])
    expires_in_days: int = Field(default=None, ge=1, le=365)


class APIKeyResponse(BaseModel):
    """Response model for API key creation."""

    api_key: str
    key_id: str
    name: str
    expires_at: str = None


@router.post("/register", response_model=LoginResponse)
async def register(request: RegisterRequest) -> LoginResponse:
    """
    Register a new user and tenant.

    Creates a new tenant and user account.
    """
    # Check if email already exists
    check_query = """
        SELECT COUNT(*) as count
        FROM repograph.users
        WHERE email = %s
    """
    result = db_pool.execute(check_query, (request.email,))

    if result and result[0]["count"] > 0:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Email already registered",
        )

    # Create tenant
    tenant_id = str(uuid.uuid4())
    tenant_query = """
        INSERT INTO repograph.tenants (id, name, description)
        VALUES (%s, %s, %s)
        RETURNING id
    """

    try:
        db_pool.execute(
            tenant_query,
            (tenant_id, request.tenant_name, f"Tenant for {request.email}"),
            fetch=False,
        )
    except Exception as e:
        # Handle unique constraint violation
        if "unique" in str(e).lower():
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Tenant name already exists",
            )
        raise

    # Create user (simplified - in production, use proper user table)
    user_id = str(uuid.uuid4())

    # Create access token
    access_token = create_access_token(
        data={
            "sub": user_id,
            "email": request.email,
            "tenant_id": tenant_id,
            "permissions": ["read", "write"],
        }
    )

    return LoginResponse(
        access_token=access_token,
        token_type="bearer",
        expires_in=86400,  # 24 hours
        user={
            "user_id": user_id,
            "email": request.email,
            "tenant_id": tenant_id,
            "tenant_name": request.tenant_name,
        },
    )


@router.post("/login", response_model=LoginResponse)
async def login(request: LoginRequest) -> LoginResponse:
    """
    Login with email and password.

    Returns a JWT access token.
    """
    # This is a simplified login - in production, check against user table
    # For demo, we'll create a token for the default tenant

    # Get default tenant
    tenant_query = """
        SELECT id, name FROM repograph.tenants
        WHERE name = 'aeptus'
        LIMIT 1
    """
    result = db_pool.execute(tenant_query)

    if not result:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Default tenant not found",
        )

    tenant = result[0]
    user_id = str(uuid.uuid4())

    # Create access token
    access_token = create_access_token(
        data={
            "sub": user_id,
            "email": request.email,
            "tenant_id": str(tenant["id"]),
            "permissions": ["read", "write"],
        }
    )

    return LoginResponse(
        access_token=access_token,
        token_type="bearer",
        expires_in=86400,
        user={
            "user_id": user_id,
            "email": request.email,
            "tenant_id": str(tenant["id"]),
            "tenant_name": tenant["name"],
        },
    )


@router.post("/api-key", response_model=APIKeyResponse)
async def create_api_key(
    request: CreateAPIKeyRequest,
    current_user: User = Depends(get_current_user),
) -> APIKeyResponse:
    """
    Create a new API key.

    API keys provide an alternative authentication method to JWT tokens.
    """
    # Generate API key
    api_key = f"rgk_{uuid.uuid4().hex}_{uuid.uuid4().hex}"  # rgk = repograph key
    key_hash = hash_password(api_key)

    # Calculate expiration
    expires_at = None
    if request.expires_in_days:
        from datetime import datetime

        expires_at = datetime.utcnow() + timedelta(days=request.expires_in_days)

    # Store in database
    key_id = str(uuid.uuid4())
    query = """
        INSERT INTO repograph.api_keys (
            id, tenant_id, name, key_hash, permissions, expires_at
        ) VALUES (%s, %s, %s, %s, %s, %s)
    """

    db_pool.execute(
        query,
        (
            key_id,
            current_user.tenant_id,
            request.name,
            key_hash,
            request.permissions,
            expires_at,
        ),
        fetch=False,
    )

    return APIKeyResponse(
        api_key=api_key,
        key_id=key_id,
        name=request.name,
        expires_at=expires_at.isoformat() if expires_at else None,
    )


@router.get("/me")
async def get_current_user_info(
    current_user: User = Depends(get_current_user),
) -> Dict[str, Any]:
    """
    Get current user information.

    Returns the authenticated user's details.
    """
    return current_user.to_dict()