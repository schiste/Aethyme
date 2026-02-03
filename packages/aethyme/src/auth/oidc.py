"""OIDC (OpenID Connect) authentication for Aethyme.

This module provides OIDC integration for enterprise deployments,
compatible with Keycloak, Auth0, and other OIDC providers.
"""

from typing import Optional, Dict, Any, List
from datetime import datetime, timedelta
import secrets
import structlog
from urllib.parse import urljoin

import httpx
from jose import JWTError, jwt
from jose.exceptions import ExpiredSignatureError, JWTClaimsError

from ..config import settings

logger = structlog.get_logger(__name__)


class OIDCError(Exception):
    """Base exception for OIDC errors."""
    pass


class OIDCConfigurationError(OIDCError):
    """OIDC configuration error."""
    pass


class OIDCValidationError(OIDCError):
    """OIDC token validation error."""
    pass


class OIDCClient:
    """OIDC client for authentication and token management.

    Supports standard OIDC providers like Keycloak, Auth0, Okta, etc.
    """

    def __init__(
        self,
        issuer_url: Optional[str] = None,
        client_id: Optional[str] = None,
        client_secret: Optional[str] = None,
        redirect_uri: Optional[str] = None,
    ):
        """Initialize OIDC client.

        Args:
            issuer_url: OIDC issuer URL (e.g., https://auth.example.com/realms/myrealm)
            client_id: OAuth2 client ID
            client_secret: OAuth2 client secret
            redirect_uri: Redirect URI after authentication
        """
        self.issuer_url = issuer_url or getattr(settings, 'oidc_issuer_url', None)
        self.client_id = client_id or getattr(settings, 'oidc_client_id', None)
        self.client_secret = client_secret or getattr(settings, 'oidc_client_secret', None)
        self.redirect_uri = redirect_uri or getattr(settings, 'oidc_redirect_uri', None)

        self._discovery_cache: Optional[Dict[str, Any]] = None
        self._jwks_cache: Optional[Dict[str, Any]] = None
        self._cache_expires_at: Optional[datetime] = None

    @property
    def is_configured(self) -> bool:
        """Check if OIDC is properly configured."""
        return bool(self.issuer_url and self.client_id)

    async def discover_configuration(self) -> Dict[str, Any]:
        """Discover OIDC provider configuration.

        Uses the .well-known/openid-configuration endpoint.

        Returns:
            OIDC provider configuration

        Raises:
            OIDCConfigurationError: If discovery fails
        """
        if not self.issuer_url:
            raise OIDCConfigurationError("OIDC issuer URL not configured")

        # Check cache
        if self._discovery_cache and self._cache_expires_at:
            if datetime.utcnow() < self._cache_expires_at:
                return self._discovery_cache

        discovery_url = urljoin(
            self.issuer_url.rstrip('/') + '/',
            '.well-known/openid-configuration'
        )

        try:
            async with httpx.AsyncClient() as client:
                response = await client.get(discovery_url, timeout=10.0)
                response.raise_for_status()

                config = response.json()

                # Cache for 1 hour
                self._discovery_cache = config
                self._cache_expires_at = datetime.utcnow() + timedelta(hours=1)

                logger.info(
                    "OIDC configuration discovered",
                    issuer=config.get('issuer'),
                    authorization_endpoint=config.get('authorization_endpoint'),
                )

                return config

        except httpx.HTTPError as e:
            logger.error("Failed to discover OIDC configuration", error=str(e))
            raise OIDCConfigurationError(f"Failed to discover OIDC configuration: {e}")

    async def get_jwks(self) -> Dict[str, Any]:
        """Get JSON Web Key Set (JWKS) from the provider.

        Returns:
            JWKS (public keys for token verification)

        Raises:
            OIDCConfigurationError: If JWKS retrieval fails
        """
        config = await self.discover_configuration()
        jwks_uri = config.get('jwks_uri')

        if not jwks_uri:
            raise OIDCConfigurationError("jwks_uri not found in OIDC configuration")

        # Check cache
        if self._jwks_cache and self._cache_expires_at:
            if datetime.utcnow() < self._cache_expires_at:
                return self._jwks_cache

        try:
            async with httpx.AsyncClient() as client:
                response = await client.get(jwks_uri, timeout=10.0)
                response.raise_for_status()

                jwks = response.json()

                # Cache for 1 hour
                self._jwks_cache = jwks
                self._cache_expires_at = datetime.utcnow() + timedelta(hours=1)

                return jwks

        except httpx.HTTPError as e:
            logger.error("Failed to get JWKS", error=str(e))
            raise OIDCConfigurationError(f"Failed to get JWKS: {e}")

    def generate_authorization_url(self, state: Optional[str] = None) -> str:
        """Generate authorization URL for OIDC login flow.

        Args:
            state: Optional state parameter for CSRF protection

        Returns:
            Authorization URL
        """
        if not self.is_configured:
            raise OIDCConfigurationError("OIDC not configured")

        # This should be called after discover_configuration
        # For simplicity, constructing the URL directly
        state = state or secrets.token_urlsafe(32)

        params = {
            'client_id': self.client_id,
            'redirect_uri': self.redirect_uri,
            'response_type': 'code',
            'scope': 'openid profile email',
            'state': state,
        }

        # Construct URL
        from urllib.parse import urlencode
        query_string = urlencode(params)

        # Typically: {issuer}/protocol/openid-connect/auth
        auth_endpoint = f"{self.issuer_url.rstrip('/')}/protocol/openid-connect/auth"

        return f"{auth_endpoint}?{query_string}"

    async def exchange_code_for_tokens(self, code: str) -> Dict[str, Any]:
        """Exchange authorization code for tokens.

        Args:
            code: Authorization code from OIDC provider

        Returns:
            Token response (access_token, id_token, refresh_token)

        Raises:
            OIDCError: If token exchange fails
        """
        if not self.is_configured:
            raise OIDCConfigurationError("OIDC not configured")

        config = await self.discover_configuration()
        token_endpoint = config.get('token_endpoint')

        if not token_endpoint:
            raise OIDCConfigurationError("token_endpoint not found in OIDC configuration")

        data = {
            'grant_type': 'authorization_code',
            'code': code,
            'redirect_uri': self.redirect_uri,
            'client_id': self.client_id,
            'client_secret': self.client_secret,
        }

        try:
            async with httpx.AsyncClient() as client:
                response = await client.post(
                    token_endpoint,
                    data=data,
                    timeout=10.0,
                )
                response.raise_for_status()

                tokens = response.json()

                logger.info("Successfully exchanged code for tokens")

                return tokens

        except httpx.HTTPError as e:
            logger.error("Failed to exchange code for tokens", error=str(e))
            raise OIDCError(f"Failed to exchange code for tokens: {e}")

    async def refresh_access_token(self, refresh_token: str) -> Dict[str, Any]:
        """Refresh access token using refresh token.

        Args:
            refresh_token: Refresh token

        Returns:
            New token response

        Raises:
            OIDCError: If token refresh fails
        """
        if not self.is_configured:
            raise OIDCConfigurationError("OIDC not configured")

        config = await self.discover_configuration()
        token_endpoint = config.get('token_endpoint')

        if not token_endpoint:
            raise OIDCConfigurationError("token_endpoint not found in OIDC configuration")

        data = {
            'grant_type': 'refresh_token',
            'refresh_token': refresh_token,
            'client_id': self.client_id,
            'client_secret': self.client_secret,
        }

        try:
            async with httpx.AsyncClient() as client:
                response = await client.post(
                    token_endpoint,
                    data=data,
                    timeout=10.0,
                )
                response.raise_for_status()

                tokens = response.json()

                logger.info("Successfully refreshed access token")

                return tokens

        except httpx.HTTPError as e:
            logger.error("Failed to refresh access token", error=str(e))
            raise OIDCError(f"Failed to refresh access token: {e}")

    async def verify_token(self, token: str) -> Dict[str, Any]:
        """Verify and decode an OIDC token (ID token or access token).

        Args:
            token: JWT token to verify

        Returns:
            Token payload

        Raises:
            OIDCValidationError: If token verification fails
        """
        try:
            # Get JWKS for verification
            jwks = await self.get_jwks()

            # Decode header to get key ID
            unverified_header = jwt.get_unverified_header(token)
            kid = unverified_header.get('kid')

            if not kid:
                raise OIDCValidationError("Token missing 'kid' in header")

            # Find matching key in JWKS
            key = None
            for jwk in jwks.get('keys', []):
                if jwk.get('kid') == kid:
                    key = jwk
                    break

            if not key:
                raise OIDCValidationError(f"Key with kid '{kid}' not found in JWKS")

            # Verify and decode token
            payload = jwt.decode(
                token,
                key,
                algorithms=['RS256', 'RS384', 'RS512'],
                audience=self.client_id,
                issuer=self.issuer_url,
            )

            logger.debug("Token verified successfully", sub=payload.get('sub'))

            return payload

        except ExpiredSignatureError:
            raise OIDCValidationError("Token has expired")
        except JWTClaimsError as e:
            raise OIDCValidationError(f"Invalid token claims: {e}")
        except JWTError as e:
            logger.warning("Token verification failed", error=str(e))
            raise OIDCValidationError(f"Token verification failed: {e}")


class JWTTokenGenerator:
    """Generate JWT tokens with scoped claims for Aethyme."""

    @staticmethod
    def create_token(
        user_id: str,
        org_id: str,
        scopes: List[str],
        email: Optional[str] = None,
        expires_delta: Optional[timedelta] = None,
    ) -> str:
        """Create a JWT token with scoped claims.

        Args:
            user_id: User identifier
            org_id: Organization/tenant identifier
            scopes: List of scopes (e.g., ['repo:read', 'repo:write'])
            email: User email (optional)
            expires_delta: Token expiration time (default: 24 hours)

        Returns:
            Encoded JWT token
        """
        if expires_delta is None:
            expires_delta = timedelta(seconds=settings.jwt_expiration_delta)

        expire = datetime.utcnow() + expires_delta

        payload = {
            "sub": user_id,
            "org": org_id,
            "scopes": scopes,
            "exp": int(expire.timestamp()),
            "iat": int(datetime.utcnow().timestamp()),
            "iss": "aethyme",
        }

        if email:
            payload["email"] = email

        token = jwt.encode(
            payload,
            settings.jwt_secret_key,
            algorithm=settings.jwt_algorithm,
        )

        logger.debug(
            "JWT token created",
            user_id=user_id,
            org_id=org_id,
            scopes=scopes,
            expires=expire.isoformat(),
        )

        return token

    @staticmethod
    def decode_token(token: str) -> Dict[str, Any]:
        """Decode and validate a Aethyme JWT token.

        Args:
            token: JWT token to decode

        Returns:
            Token payload

        Raises:
            JWTError: If token is invalid or expired
        """
        try:
            payload = jwt.decode(
                token,
                settings.jwt_secret_key,
                algorithms=[settings.jwt_algorithm],
            )

            # Validate required claims
            required_claims = ['sub', 'org', 'scopes']
            for claim in required_claims:
                if claim not in payload:
                    raise JWTError(f"Missing required claim: {claim}")

            return payload

        except ExpiredSignatureError:
            raise JWTError("Token has expired")
        except JWTClaimsError as e:
            raise JWTError(f"Invalid token claims: {e}")
        except JWTError as e:
            logger.warning("Token validation failed", error=str(e))
            raise

    @staticmethod
    def create_refresh_token(
        user_id: str,
        org_id: str,
    ) -> str:
        """Create a refresh token.

        Refresh tokens are long-lived tokens used to obtain new access tokens.

        Args:
            user_id: User identifier
            org_id: Organization/tenant identifier

        Returns:
            Encoded refresh token
        """
        # Refresh tokens are valid for 30 days
        expires_delta = timedelta(days=30)
        expire = datetime.utcnow() + expires_delta

        payload = {
            "sub": user_id,
            "org": org_id,
            "token_type": "refresh",
            "exp": int(expire.timestamp()),
            "iat": int(datetime.utcnow().timestamp()),
            "iss": "aethyme",
        }

        token = jwt.encode(
            payload,
            settings.jwt_secret_key,
            algorithm=settings.jwt_algorithm,
        )

        logger.debug(
            "Refresh token created",
            user_id=user_id,
            org_id=org_id,
            expires=expire.isoformat(),
        )

        return token


# Global OIDC client instance
oidc_client = OIDCClient()
