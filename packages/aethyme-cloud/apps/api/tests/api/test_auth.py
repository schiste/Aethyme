"""Authentication API tests."""

from app.core.security import decode_access_token


def test_dev_token_mints_core_compatible_claims(
    authenticated_client,
) -> None:
    """The development token endpoint should mint the claims core enforces."""
    response = authenticated_client.post(
        "/api/auth/dev/token",
        json={"email": "test@example.com"},
    )

    assert response.status_code == 200
    payload = response.json()

    assert payload["token_type"] == "bearer"
    assert payload["user"]["email"] == "test@example.com"

    decoded = decode_access_token(payload["access_token"])
    assert decoded is not None
    assert decoded["sub"] == "test-user-123"
    assert decoded["org"] == "test-org-123"
    assert decoded["org_id"] == "test-org-123"
    assert decoded["tenant_id"] == "test-org-123"
    assert "repo:read" in decoded["scopes"]
    assert "repo:write" in decoded["scopes"]
