"""API key management for Aethyme.

API keys provide a secure way for CI/CD systems and automated tools
to authenticate with Aethyme without using user credentials.
"""

import secrets
import hashlib
import uuid
from typing import Optional, List, Dict, Any
from datetime import datetime, timedelta
import structlog

from ..graph.connection_pool import db_pool

logger = structlog.get_logger(__name__)


class APIKeyError(Exception):
    """Base exception for API key errors."""
    pass


class APIKeyNotFoundError(APIKeyError):
    """API key not found."""
    pass


class APIKeyRevokedError(APIKeyError):
    """API key has been revoked."""
    pass


class APIKeyExpiredError(APIKeyError):
    """API key has expired."""
    pass


def generate_api_key(prefix: str = "rg_live") -> str:
    """Generate a cryptographically secure API key.

    Format: {prefix}_{random_hex}

    Args:
        prefix: Key prefix (rg_live for production, rg_test for testing)

    Returns:
        Generated API key
    """
    # Generate 32 bytes of random data = 64 hex characters
    random_part = secrets.token_hex(32)

    return f"{prefix}_{random_part}"


def hash_api_key(api_key: str) -> str:
    """Hash an API key for secure storage.

    Uses SHA-256 for hashing. The key should never be stored in plain text.

    Args:
        api_key: API key to hash

    Returns:
        Hex digest of the hash
    """
    return hashlib.sha256(api_key.encode()).hexdigest()


def verify_api_key(api_key: str) -> Optional[Dict[str, Any]]:
    """Verify an API key and return its metadata.

    Args:
        api_key: API key to verify

    Returns:
        API key metadata if valid, None otherwise

    Raises:
        APIKeyRevokedError: If key has been revoked
        APIKeyExpiredError: If key has expired
    """
    key_hash = hash_api_key(api_key)

    query = """
        SELECT ak.*, t.name as tenant_name
        FROM aethyme.api_keys ak
        JOIN aethyme.tenants t ON t.id = ak.tenant_id
        WHERE ak.key_hash = %s
    """

    result = db_pool.execute(query, (key_hash,))

    if not result:
        return None

    key_data = result[0]

    # Check if revoked
    if key_data['revoked_at']:
        raise APIKeyRevokedError(f"API key has been revoked at {key_data['revoked_at']}")

    # Check if expired
    if key_data['expires_at'] and key_data['expires_at'] < datetime.utcnow():
        raise APIKeyExpiredError(f"API key expired at {key_data['expires_at']}")

    # Update last used timestamp
    update_query = """
        UPDATE aethyme.api_keys
        SET last_used_at = NOW()
        WHERE id = %s
    """
    db_pool.execute(update_query, (key_data['id'],), fetch=False)

    return key_data


class APIKeyManager:
    """Manager for API key operations."""

    @staticmethod
    def create(
        tenant_id: str,
        name: str,
        scopes: List[str],
        expires_in_days: Optional[int] = None,
        repo_id: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Create a new API key.

        Args:
            tenant_id: Tenant ID the key belongs to
            name: Human-readable name for the key
            scopes: List of scopes (e.g., ['repo:read', 'repo:write'])
            expires_in_days: Number of days until expiration (None = no expiration)
            repo_id: Optional repository ID to scope the key to

        Returns:
            Dictionary with api_key, key_id, and metadata
        """
        # Generate API key
        api_key = generate_api_key()
        key_hash = hash_api_key(api_key)

        # Calculate expiration
        expires_at = None
        if expires_in_days:
            expires_at = datetime.utcnow() + timedelta(days=expires_in_days)

        # Store in database
        key_id = str(uuid.uuid4())

        query = """
            INSERT INTO aethyme.api_keys (
                id, tenant_id, name, key_hash, scopes, expires_at, repo_id
            ) VALUES (%s, %s, %s, %s, %s, %s, %s)
            RETURNING id, created_at
        """

        import json
        result = db_pool.execute(
            query,
            (
                key_id,
                tenant_id,
                name,
                key_hash,
                json.dumps(scopes),
                expires_at,
                repo_id,
            ),
        )

        logger.info(
            "API key created",
            key_id=key_id,
            tenant_id=tenant_id,
            name=name,
            scopes=scopes,
            repo_id=repo_id,
        )

        return {
            'api_key': api_key,  # Only returned once!
            'key_id': key_id,
            'name': name,
            'scopes': scopes,
            'expires_at': expires_at.isoformat() if expires_at else None,
            'created_at': result[0]['created_at'].isoformat(),
        }

    @staticmethod
    def list_keys(tenant_id: str) -> List[Dict[str, Any]]:
        """List all API keys for a tenant.

        Note: The actual key values are not returned, only metadata.

        Args:
            tenant_id: Tenant ID

        Returns:
            List of API key metadata
        """
        query = """
            SELECT
                id, name, scopes, last_used_at, expires_at,
                created_at, revoked_at, repo_id
            FROM aethyme.api_keys
            WHERE tenant_id = %s
            ORDER BY created_at DESC
        """

        results = db_pool.execute(query, (tenant_id,))

        if not results:
            return []

        import json
        keys = []
        for row in results:
            scopes = row['scopes']
            if isinstance(scopes, str):
                scopes = json.loads(scopes)

            keys.append({
                'key_id': str(row['id']),
                'name': row['name'],
                'scopes': scopes,
                'last_used_at': row['last_used_at'].isoformat() if row['last_used_at'] else None,
                'expires_at': row['expires_at'].isoformat() if row['expires_at'] else None,
                'created_at': row['created_at'].isoformat(),
                'revoked_at': row['revoked_at'].isoformat() if row['revoked_at'] else None,
                'repo_id': str(row['repo_id']) if row['repo_id'] else None,
                'status': 'revoked' if row['revoked_at'] else (
                    'expired' if row['expires_at'] and row['expires_at'] < datetime.utcnow() else 'active'
                ),
            })

        return keys

    @staticmethod
    def revoke(key_id: str, tenant_id: str) -> bool:
        """Revoke an API key.

        Args:
            key_id: API key ID to revoke
            tenant_id: Tenant ID (for security check)

        Returns:
            True if revoked, False if not found

        Raises:
            APIKeyError: If key doesn't belong to tenant
        """
        # Check ownership
        check_query = """
            SELECT tenant_id FROM aethyme.api_keys
            WHERE id = %s
        """
        result = db_pool.execute(check_query, (key_id,))

        if not result:
            return False

        if str(result[0]['tenant_id']) != tenant_id:
            raise APIKeyError("API key does not belong to this tenant")

        # Revoke
        query = """
            UPDATE aethyme.api_keys
            SET revoked_at = NOW()
            WHERE id = %s AND revoked_at IS NULL
            RETURNING id
        """

        result = db_pool.execute(query, (key_id,))

        if result:
            logger.info("API key revoked", key_id=key_id, tenant_id=tenant_id)
            return True

        return False

    @staticmethod
    def rotate(
        key_id: str,
        tenant_id: str,
        expires_in_days: Optional[int] = None,
    ) -> Dict[str, Any]:
        """Rotate an API key by revoking the old one and creating a new one.

        Args:
            key_id: API key ID to rotate
            tenant_id: Tenant ID
            expires_in_days: Expiration for new key

        Returns:
            New API key data

        Raises:
            APIKeyNotFoundError: If key not found
            APIKeyError: If key doesn't belong to tenant
        """
        # Get existing key metadata
        query = """
            SELECT tenant_id, name, scopes, repo_id
            FROM aethyme.api_keys
            WHERE id = %s
        """
        result = db_pool.execute(query, (key_id,))

        if not result:
            raise APIKeyNotFoundError(f"API key {key_id} not found")

        key_data = result[0]

        if str(key_data['tenant_id']) != tenant_id:
            raise APIKeyError("API key does not belong to this tenant")

        # Extract scopes
        import json
        scopes = key_data['scopes']
        if isinstance(scopes, str):
            scopes = json.loads(scopes)

        # Revoke old key
        APIKeyManager.revoke(key_id, tenant_id)

        # Create new key with same metadata
        new_key = APIKeyManager.create(
            tenant_id=tenant_id,
            name=f"{key_data['name']} (rotated)",
            scopes=scopes,
            expires_in_days=expires_in_days,
            repo_id=key_data['repo_id'],
        )

        logger.info(
            "API key rotated",
            old_key_id=key_id,
            new_key_id=new_key['key_id'],
            tenant_id=tenant_id,
        )

        return new_key

    @staticmethod
    def update_scopes(
        key_id: str,
        tenant_id: str,
        scopes: List[str],
    ) -> bool:
        """Update scopes for an API key.

        Args:
            key_id: API key ID
            tenant_id: Tenant ID (for security check)
            scopes: New scopes list

        Returns:
            True if updated, False if not found

        Raises:
            APIKeyError: If key doesn't belong to tenant
        """
        # Check ownership
        check_query = """
            SELECT tenant_id FROM aethyme.api_keys
            WHERE id = %s
        """
        result = db_pool.execute(check_query, (key_id,))

        if not result:
            return False

        if str(result[0]['tenant_id']) != tenant_id:
            raise APIKeyError("API key does not belong to this tenant")

        # Update scopes
        import json
        query = """
            UPDATE aethyme.api_keys
            SET scopes = %s
            WHERE id = %s
            RETURNING id
        """

        result = db_pool.execute(query, (json.dumps(scopes), key_id))

        if result:
            logger.info(
                "API key scopes updated",
                key_id=key_id,
                tenant_id=tenant_id,
                scopes=scopes,
            )
            return True

        return False
