"""Token encryption utilities for secure storage of sensitive data."""

from cryptography.fernet import Fernet
from app.core.config import settings


def get_cipher() -> Fernet:
    """
    Get Fernet cipher instance.

    Returns:
        Fernet: Cipher instance for encryption/decryption
    """
    # Use encryption key from settings
    # In production, this should be stored securely (e.g., AWS Secrets Manager, Vault)
    encryption_key = settings.ENCRYPTION_KEY.encode() if isinstance(settings.ENCRYPTION_KEY, str) else settings.ENCRYPTION_KEY
    return Fernet(encryption_key)


def encrypt_token(token: str) -> str:
    """
    Encrypt a token for secure storage.

    Uses Fernet (symmetric authenticated encryption) to encrypt tokens.
    The encrypted token includes:
    - Encrypted data
    - Timestamp (for verification)
    - HMAC signature (prevents tampering)

    Args:
        token: Plain text token to encrypt

    Returns:
        str: Encrypted token (base64-encoded)

    Example:
        >>> encrypted = encrypt_token("ghp_abcdef123456")
        >>> print(encrypted)
        'gAAAAABh...'
    """
    if not token:
        raise ValueError("Token cannot be empty")

    cipher = get_cipher()
    encrypted_bytes = cipher.encrypt(token.encode('utf-8'))
    return encrypted_bytes.decode('utf-8')


def decrypt_token(encrypted_token: str) -> str:
    """
    Decrypt a stored token.

    Decrypts a token that was encrypted using encrypt_token().
    Verifies HMAC signature and timestamp to ensure data integrity.

    Args:
        encrypted_token: Encrypted token (base64-encoded)

    Returns:
        str: Decrypted plain text token

    Raises:
        cryptography.fernet.InvalidToken: If token is invalid or tampered with

    Example:
        >>> token = decrypt_token('gAAAAABh...')
        >>> print(token)
        'ghp_abcdef123456'
    """
    if not encrypted_token:
        raise ValueError("Encrypted token cannot be empty")

    cipher = get_cipher()
    decrypted_bytes = cipher.decrypt(encrypted_token.encode('utf-8'))
    return decrypted_bytes.decode('utf-8')


def generate_encryption_key() -> str:
    """
    Generate a new Fernet encryption key.

    Use this to generate a new encryption key for ENCRYPTION_KEY setting.
    Store the key securely in environment variables or secrets management system.

    ⚠️  WARNING: If you change the encryption key, all previously encrypted
    data will become unrecoverable. Only change the key if you're starting fresh.

    Returns:
        str: Base64-encoded encryption key (44 characters)

    Example:
        >>> key = generate_encryption_key()
        >>> print(f"ENCRYPTION_KEY={key}")
        ENCRYPTION_KEY=X3dweGhlbWpw...
    """
    return Fernet.generate_key().decode('utf-8')
