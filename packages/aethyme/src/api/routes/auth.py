import json
import re
import uuid
from typing import Any

from fastapi import APIRouter, HTTPException, status
from pydantic import BaseModel, EmailStr, Field

from ...auth import (
    APIKeyManager,
    CurrentUser,
    OrgAdminUser,
    create_access_token,
    hash_password,
    verify_password,
)
from ...graph.connection_pool import db_pool

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
    user: dict[str, Any]


class RegisterRequest(BaseModel):
    """Request model for registration."""

    email: EmailStr
    password: str = Field(..., min_length=8)
    org_name: str = Field(..., min_length=1, max_length=255)
    tenant_name: str = Field(default="default", min_length=1, max_length=255)


class CreateAPIKeyRequest(BaseModel):
    """Request model for API key creation."""

    name: str = Field(..., min_length=1, max_length=255)
    scopes: list[str] = Field(default=["repo:read"])
    expires_in_days: int | None = Field(default=None, ge=1, le=365)
    repo_id: str | None = Field(default=None)


class APIKeyResponse(BaseModel):
    """Response model for API key creation."""

    api_key: str
    key_id: str
    name: str
    expires_at: str | None = None


@router.post("/register", response_model=LoginResponse)
async def register(request: RegisterRequest) -> LoginResponse:
    """
    Register a new user and tenant.

    Creates a new org, tenant, and user account.
    """
    # Check if email already exists
    check_query = """
        SELECT COUNT(*) as count
        FROM aethyme.users
        WHERE email = %s
    """
    result = db_pool.execute(check_query, (request.email,))

    if result and result[0]["count"] > 0:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Email already registered",
        )

    org_slug = re.sub(r"[^a-z0-9]+", "-", request.org_name.lower()).strip("-") or "org"
    tenant_slug = re.sub(r"[^a-z0-9]+", "-", request.tenant_name.lower()).strip("-") or "tenant"

    org_id = str(uuid.uuid4())
    tenant_id = str(uuid.uuid4())
    create_org_query = """
        INSERT INTO aethyme.orgs (id, name, slug, description)
        VALUES (%s, %s, %s, %s)
    """
    create_tenant_query = """
        INSERT INTO aethyme.tenants (id, org_id, name, slug, description)
        VALUES (%s, %s, %s, %s, %s)
    """
    create_user_query = """
        INSERT INTO aethyme.users (id, org_id, tenant_id, email, password_hash, scopes)
        VALUES (%s, %s, %s, %s, %s, %s)
    """
    user_id = str(uuid.uuid4())
    password_hash = hash_password(request.password)
    scopes = ["org:admin", "repo:read", "repo:write"]

    try:
        with db_pool.transaction() as conn:
            with conn.cursor() as cur:
                cur.execute(
                    create_org_query,
                    (org_id, request.org_name, org_slug, f"Organization for {request.email}"),
                )
                cur.execute(
                    create_tenant_query,
                    (tenant_id, org_id, request.tenant_name, tenant_slug, f"Tenant for {request.email}"),
                )
                cur.execute(
                    create_user_query,
                    (user_id, org_id, tenant_id, request.email, password_hash, json.dumps(scopes)),
                )
    except Exception as err:
        if "unique" in str(err).lower():
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Organization, tenant, or email already exists",
            ) from err
        raise

    access_token = create_access_token(
        data={
            "sub": user_id,
            "email": request.email,
            "org": org_id,
            "tenant_id": tenant_id,
            "scopes": scopes,
        }
    )

    return LoginResponse(
        access_token=access_token,
        token_type="bearer",
        expires_in=86400,  # 24 hours
        user={
            "user_id": user_id,
            "email": request.email,
            "org_id": org_id,
            "org_name": request.org_name,
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
    user_query = """
        SELECT
            u.id,
            u.email,
            u.password_hash,
            u.scopes,
            u.org_id,
            u.tenant_id,
            o.name AS org_name,
            t.name AS tenant_name
        FROM aethyme.users u
        JOIN aethyme.orgs o ON o.id = u.org_id
        JOIN aethyme.tenants t ON t.id = u.tenant_id
        WHERE u.email = %s
          AND u.is_active = TRUE
        LIMIT 1
    """
    result = db_pool.execute(user_query, (request.email,))

    if not result:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid credentials",
        )

    user = result[0]
    if not user["password_hash"] or not verify_password(request.password, user["password_hash"]):
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Invalid credentials",
        )

    scopes = user["scopes"]
    if isinstance(scopes, str):
        import json
        scopes = json.loads(scopes)

    access_token = create_access_token(
        data={
            "sub": str(user["id"]),
            "email": user["email"],
            "org": str(user["org_id"]),
            "tenant_id": str(user["tenant_id"]),
            "scopes": scopes,
        }
    )

    return LoginResponse(
        access_token=access_token,
        token_type="bearer",
        expires_in=86400,
        user={
            "user_id": str(user["id"]),
            "email": user["email"],
            "org_id": str(user["org_id"]),
            "org_name": user["org_name"],
            "tenant_id": str(user["tenant_id"]),
            "tenant_name": user["tenant_name"],
        },
    )


@router.post("/api-key", response_model=APIKeyResponse)
async def create_api_key(
    request: CreateAPIKeyRequest,
    current_user: OrgAdminUser,
) -> APIKeyResponse:
    """
    Create a new API key.

    API keys provide an alternative authentication method to JWT tokens.
    """
    created = APIKeyManager.create(
        tenant_id=current_user.tenant_id,
        name=request.name,
        scopes=request.scopes,
        expires_in_days=request.expires_in_days,
        repo_id=request.repo_id,
    )

    return APIKeyResponse(
        api_key=created["api_key"],
        key_id=created["key_id"],
        name=request.name,
        expires_at=created["expires_at"],
    )


@router.get("/me")
async def get_current_user_info(
    current_user: CurrentUser,
) -> dict[str, Any]:
    """
    Get current user information.

    Returns the authenticated user's details.
    """
    return current_user.to_dict()
