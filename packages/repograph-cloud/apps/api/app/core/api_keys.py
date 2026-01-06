"""API key generation and validation utilities."""

import secrets
from typing import Optional
from datetime import datetime, timedelta
from app.core.security import get_password_hash, verify_password


def generate_api_key() -> str:
    """
    Generate a secure API key.
    
    Format: rgph_live_<32 random hex characters>
    Example: rgph_live_a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6
    
    Returns:
        str: Full API key (only shown once)
    """
    # Generate 32 bytes of random data = 64 hex characters
    random_part = secrets.token_hex(32)
    
    # Format: rgph_live_{random}
    api_key = f"rgph_live_{random_part}"
    
    return api_key


def get_key_prefix(api_key: str) -> str:
    """
    Extract the prefix from an API key for display.
    
    Args:
        api_key: Full API key
        
    Returns:
        str: First 8 characters (e.g., "rgph_liv")
    """
    return api_key[:8] if len(api_key) >= 8 else api_key


def hash_api_key(api_key: str) -> str:
    """
    Hash an API key for secure storage.
    
    Uses bcrypt via the same utility as password hashing.
    
    Args:
        api_key: Full API key
        
    Returns:
        str: Bcrypt hash of the key
    """
    return get_password_hash(api_key)


def verify_api_key(plain_key: str, hashed_key: str) -> bool:
    """
    Verify an API key against its hash.
    
    Args:
        plain_key: Full API key from request
        hashed_key: Stored bcrypt hash
        
    Returns:
        bool: True if key matches hash
    """
    return verify_password(plain_key, hashed_key)


def is_key_expired(expires_at: Optional[datetime]) -> bool:
    """
    Check if an API key has expired.
    
    Args:
        expires_at: Expiration datetime (None = never expires)
        
    Returns:
        bool: True if key is expired
    """
    if expires_at is None:
        return False
    
    return datetime.utcnow() >= expires_at


def calculate_expiration(days: Optional[int] = None) -> Optional[datetime]:
    """
    Calculate expiration datetime.
    
    Args:
        days: Number of days until expiration (None = never expires)
        
    Returns:
        Optional[datetime]: Expiration datetime or None
    """
    if days is None:
        return None
    
    return datetime.utcnow() + timedelta(days=days)
