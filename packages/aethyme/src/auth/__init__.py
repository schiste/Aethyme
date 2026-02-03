"""Authentication and authorization module for Aethyme."""

from .oidc import (
    OIDCClient,
    OIDCError,
    OIDCConfigurationError,
    OIDCValidationError,
    JWTTokenGenerator,
    oidc_client,
)
from .middleware import (
    get_current_user,
    require_scope,
    require_scopes,
    set_tenant_context,
    UserContext,
)
from .api_keys import (
    APIKeyManager,
    generate_api_key,
    hash_api_key,
    verify_api_key,
)

__all__ = [
    # OIDC
    'OIDCClient',
    'OIDCError',
    'OIDCConfigurationError',
    'OIDCValidationError',
    'JWTTokenGenerator',
    'oidc_client',
    # Middleware
    'get_current_user',
    'require_scope',
    'require_scopes',
    'set_tenant_context',
    'UserContext',
    # API Keys
    'APIKeyManager',
    'generate_api_key',
    'hash_api_key',
    'verify_api_key',
]
