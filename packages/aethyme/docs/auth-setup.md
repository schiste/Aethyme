# Aethyme Authentication Setup Guide

Complete guide for setting up authentication and authorization in Aethyme.

## Table of Contents

- [Overview](#overview)
- [OIDC Configuration](#oidc-configuration)
- [JWT Token Format](#jwt-token-format)
- [API Key Management](#api-key-management)
- [Rate Limiting](#rate-limiting)
- [Tenant Isolation](#tenant-isolation)
- [Security Best Practices](#security-best-practices)
- [Troubleshooting](#troubleshooting)

## Overview

Aethyme implements production-grade authentication and authorization with:

- **JWT tokens** for user authentication
- **OIDC integration** for enterprise SSO (Keycloak, Auth0, Okta)
- **API keys** for CI/CD and automation
- **Scoped permissions** (repo:read, repo:write, org:admin)
- **Row-Level Security (RLS)** for multi-tenant isolation
- **Rate limiting** to prevent abuse

## OIDC Configuration

### Supported Providers

Aethyme supports any OIDC-compliant provider:

- Keycloak
- Auth0
- Okta
- Google Identity Platform
- Azure AD
- AWS Cognito

### Environment Variables

Set these variables to enable OIDC:

```bash
# OIDC Provider Configuration
OIDC_ISSUER_URL=https://auth.example.com/realms/myrealm
OIDC_CLIENT_ID=aethyme-client
OIDC_CLIENT_SECRET=your-client-secret
OIDC_REDIRECT_URI=https://aethyme.example.com/auth/callback
```

### Keycloak Setup Example

1. **Create a Realm**

   ```bash
   # In Keycloak Admin Console
   # Create new realm: "aethyme"
   ```

2. **Create a Client**

   - Client ID: `aethyme-client`
   - Client Protocol: `openid-connect`
   - Access Type: `confidential`
   - Valid Redirect URIs: `https://your-domain.com/auth/callback`
   - Web Origins: `https://your-domain.com`

3. **Configure Client Scopes**

   Add custom scopes:
   - `repo:read`
   - `repo:write`
   - `org:admin`

4. **Create Roles**

   - `reader` → grants `repo:read`
   - `writer` → grants `repo:read`, `repo:write`
   - `admin` → grants `org:admin`

5. **Get Configuration**

   ```bash
   OIDC_ISSUER_URL=https://keycloak.example.com/realms/aethyme
   OIDC_CLIENT_ID=aethyme-client
   OIDC_CLIENT_SECRET=<from Keycloak>
   ```

### Auth0 Setup Example

1. **Create Application**

   - Go to Auth0 Dashboard → Applications → Create Application
   - Name: `Aethyme`
   - Type: `Regular Web Application`

2. **Configure Application**

   - Allowed Callback URLs: `https://your-domain.com/auth/callback`
   - Allowed Web Origins: `https://your-domain.com`

3. **Create API**

   - Go to Applications → APIs → Create API
   - Name: `Aethyme API`
   - Identifier: `https://api.aethyme.example.com`

4. **Define Permissions**

   Add permissions to the API:
   - `repo:read`
   - `repo:write`
   - `org:admin`

5. **Get Configuration**

   ```bash
   OIDC_ISSUER_URL=https://your-tenant.auth0.com/
   OIDC_CLIENT_ID=<your-client-id>
   OIDC_CLIENT_SECRET=<your-client-secret>
   ```

### Testing OIDC

```python
from src.auth.oidc import oidc_client

async def main() -> None:
    assert oidc_client.is_configured
    config = await oidc_client.discover_configuration()
    print(config["issuer"])
    print(config["authorization_endpoint"])
    auth_url = oidc_client.generate_authorization_url()
    print(f"Login at: {auth_url}")
```

## JWT Token Format

### Token Structure

Aethyme uses JWT tokens with the following claims:

```json
{
  "sub": "user_id_12345",
  "org": "org_uuid_67890",
  "scopes": ["repo:read", "repo:write"],
  "email": "user@example.com",
  "exp": 1234567890,
  "iat": 1234567800,
  "iss": "aethyme"
}
```

### Required Claims

- `sub` - Subject (user ID)
- `org` - Organization/tenant ID
- `scopes` - Array of permission scopes
- `exp` - Expiration timestamp
- `iat` - Issued at timestamp
- `iss` - Issuer

### Optional Claims

- `email` - User email address
- `name` - User display name

### Creating Tokens

```python
from src.auth.oidc import JWTTokenGenerator

# Create access token
token = JWTTokenGenerator.create_token(
    user_id="user_123",
    org_id="org_abc",
    scopes=["repo:read", "repo:write"],
    email="user@example.com"
)

# Create refresh token (30-day expiry)
refresh_token = JWTTokenGenerator.create_refresh_token(
    user_id="user_123",
    org_id="org_abc"
)
```

### Validating Tokens

```python
from src.auth.oidc import JWTTokenGenerator

# Decode and validate
try:
    payload = JWTTokenGenerator.decode_token(token)
    user_id = payload['sub']
    org_id = payload['org']
    scopes = payload['scopes']
except JWTError as e:
    print(f"Invalid token: {e}")
```

### Token Configuration

```bash
# JWT Settings
JWT_SECRET_KEY=your-super-secret-key-min-32-chars
JWT_ALGORITHM=HS256
JWT_EXPIRATION_DELTA=86400  # 24 hours in seconds
```

**Security Note:** Use a cryptographically secure secret key:

```bash
# Generate a secure secret
python -c "import secrets; print(secrets.token_urlsafe(32))"
```

## API Key Management

### Creating API Keys

API keys provide an alternative to JWT tokens for automation and CI/CD.

```python
from src.auth.api_keys import APIKeyManager

# Create API key
key_data = APIKeyManager.create(
    tenant_id="org_abc",
    name="CI/CD Pipeline",
    scopes=["repo:read", "repo:write"],
    expires_in_days=90  # Optional expiration
)

# Save this securely! It's only shown once.
api_key = key_data['api_key']  # rg_live_abc123...
key_id = key_data['key_id']
```

### API Key Format

```
rg_live_<64_hex_characters>
rg_test_<64_hex_characters>  # For testing/development
```

### Scoping to Repository

Scope an API key to a specific repository:

```python
key_data = APIKeyManager.create(
    tenant_id="org_abc",
    name="Repo-Specific Key",
    scopes=["repo:read"],
    repo_id="repo_xyz"  # Optional: restrict to specific repo
)
```

### Using API Keys

```bash
# In Authorization header
curl https://api.aethyme.example.com/api/search/ \
  -H "Authorization: Bearer rg_live_abc123..."

# Or in environment variable
export AETHYME_API_KEY=rg_live_abc123...
```

### Listing API Keys

```python
# List all keys for a tenant
keys = APIKeyManager.list_keys(tenant_id="org_abc")

for key in keys:
    print(f"{key['name']}: {key['status']}")
    print(f"  Scopes: {key['scopes']}")
    print(f"  Last used: {key['last_used_at']}")
```

### Revoking API Keys

```python
# Revoke a key
revoked = APIKeyManager.revoke(
    key_id="key_123",
    tenant_id="org_abc"
)

if revoked:
    print("Key revoked successfully")
```

### Rotating API Keys

```python
# Rotate a key (revokes old, creates new)
new_key = APIKeyManager.rotate(
    key_id="old_key_123",
    tenant_id="org_abc",
    expires_in_days=90
)

print(f"New API key: {new_key['api_key']}")
```

### Updating Scopes

```python
# Update scopes for an existing key
updated = APIKeyManager.update_scopes(
    key_id="key_123",
    tenant_id="org_abc",
    scopes=["repo:read", "repo:write", "audit:read"]
)
```

## Rate Limiting

### Default Limits

| Endpoint | Limit | Window |
|----------|-------|--------|
| `/api/search/` | 100/min | 60s |
| `/api/ego/` | 50/min | 60s |
| `/api/impact/` | 50/min | 60s |
| `/api/index` | 10/min | 60s |
| `/api/ai-ready` | 20/min | 60s |
| `/api/autofix` | 10/min | 60s |
| Default | 100/min | 60s |

### Configuration

```bash
# Rate Limiting
RATE_LIMIT_DEFAULT=100  # requests per minute
REDIS_URL=redis://localhost:6379/0
```

### Rate Limit Headers

Responses include rate limit information:

```
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 95
X-RateLimit-Reset: 1732273800
```

### Handling Rate Limits

When rate limited, you'll receive a 429 response:

```json
{
  "error": {
    "code": "RATE_LIMIT_EXCEEDED",
    "message": "Rate limit exceeded. Try again in 45 seconds."
  }
}
```

Response headers:

```
Retry-After: 45
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1732273845
```

### Best Practices

1. **Respect rate limits** - Check headers and implement backoff
2. **Cache responses** - Reduce unnecessary API calls
3. **Use webhooks** - For event-driven updates instead of polling
4. **Request higher limits** - For production use cases

Example with retry:

```python
import time
import requests

def call_api_with_retry(url, headers, max_retries=3):
    for attempt in range(max_retries):
        response = requests.get(url, headers=headers)

        if response.status_code == 429:
            retry_after = int(response.headers.get('Retry-After', 60))
            print(f"Rate limited. Waiting {retry_after}s...")
            time.sleep(retry_after)
            continue

        return response

    raise Exception("Max retries exceeded")
```

## Tenant Isolation

### How It Works

Aethyme uses PostgreSQL Row-Level Security (RLS) to enforce tenant isolation:

1. **Session Variables** - Each request sets `app.current_tenant`
2. **RLS Policies** - Database enforces isolation automatically
3. **No Query Changes** - Application code stays simple

### Setting Tenant Context

The middleware automatically sets the tenant context:

```python
from src.graph.store import GraphStore

store = GraphStore(
    org_id="org_abc",
    tenant_id="tenant_123",
    scopes=["repo:read"],
)

results = store.search("UserService")
```

### Verifying Isolation

Test tenant isolation:

```python
# As Tenant A
db_pool.execute("SET app.current_tenant = '<tenant_a_id>'")
repos = db_pool.execute("SELECT * FROM aethyme.repositories")
# Only sees Tenant A's repos

# As Tenant B
db_pool.execute("SET app.current_tenant = '<tenant_b_id>'")
repos = db_pool.execute("SELECT * FROM aethyme.repositories")
# Only sees Tenant B's repos
```

### Scope-Based Access

Scopes control what operations are allowed:

| Scope | Permissions |
|-------|-------------|
| `repo:read` | Read repositories, nodes, edges |
| `repo:write` | Read + create/update repositories, nodes, edges |
| `org:admin` | All permissions + manage users, API keys |
| `audit:read` | Read audit logs |

Example:

```python
# Set scopes in session
db_pool.execute("SET app.current_scopes = '[\"repo:read\"]'")

# This query works (read permission)
nodes = db_pool.execute("SELECT * FROM aethyme.nodes")

# This fails (no write permission)
db_pool.execute("INSERT INTO aethyme.nodes (...)")
# → Permission denied
```

## Security Best Practices

### 1. Secrets Management

**Never** commit secrets to version control:

```bash
# Use environment variables
export JWT_SECRET_KEY="$(python -c 'import secrets; print(secrets.token_urlsafe(32))')"

# Or use a secrets manager
# - AWS Secrets Manager
# - GCP Secret Manager
# - HashiCorp Vault
# - Azure Key Vault
```

### 2. Token Expiration

Use short-lived access tokens with refresh tokens:

```python
# Access token: 1 hour
access_token = JWTTokenGenerator.create_token(
    user_id=user_id,
    org_id=org_id,
    scopes=scopes,
    expires_delta=timedelta(hours=1)
)

# Refresh token: 30 days
refresh_token = JWTTokenGenerator.create_refresh_token(
    user_id=user_id,
    org_id=org_id
)
```

### 3. API Key Rotation

Rotate API keys regularly:

```bash
# Rotate every 90 days
0 0 1 */3 * /path/to/rotate_api_keys.sh
```

### 4. Audit Logging

Enable audit logging for security events:

```sql
-- Audit log is automatically populated
SELECT * FROM aethyme.audit_logs
WHERE action IN ('login', 'api_key_created', 'api_key_revoked')
ORDER BY created_at DESC;
```

### 5. HTTPS Only

**Always** use HTTPS in production:

```nginx
# Nginx configuration
server {
    listen 443 ssl http2;
    server_name api.aethyme.example.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    # Force HTTPS
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;
}
```

### 6. Rate Limiting

Enable rate limiting to prevent abuse:

```python
from src.middleware.rate_limit import RateLimitMiddleware

app.add_middleware(RateLimitMiddleware)
```

### 7. Input Validation

Validate all inputs (handled by Pydantic):

```python
from pydantic import BaseModel, EmailStr

class LoginRequest(BaseModel):
    email: EmailStr  # Validates email format
    password: str = Field(..., min_length=8)
```

## Troubleshooting

### Issue: "Invalid authentication token"

**Cause:** Token is expired or invalid

**Solution:**
```python
# Check token expiration
payload = jwt.decode(token, options={"verify_signature": False})
exp = payload.get('exp')
if exp < time.time():
    print("Token expired")
```

### Issue: "OIDC configuration failed"

**Cause:** Cannot reach OIDC provider

**Solution:**
```bash
# Test OIDC endpoint
curl https://auth.example.com/realms/myrealm/.well-known/openid-configuration

# Check network/firewall
ping auth.example.com
```

### Issue: "Permission denied" errors

**Cause:** Missing scopes or wrong tenant context

**Solution:**
```python
# Check user scopes
user = UserContext(...)
print(f"Scopes: {user.scopes}")
print(f"Has repo:write: {user.has_scope('repo:write')}")

# Check tenant context
db_pool.execute("SHOW app.current_tenant")
```

### Issue: Rate limited

**Cause:** Too many requests

**Solution:**
```python
# Implement exponential backoff
import time

def call_with_backoff(func, max_retries=3):
    for i in range(max_retries):
        try:
            return func()
        except RateLimitExceeded as e:
            if i < max_retries - 1:
                wait = 2 ** i
                time.sleep(wait)
            else:
                raise
```

### Issue: "Tenant isolation not working"

**Cause:** Session variable not set

**Solution:**
```python
# Ensure graph operations are created with request context
from src.graph.store import GraphStore

store = GraphStore(
    org_id=current_user.org_id,
    tenant_id=current_user.tenant_id,
    scopes=current_user.scopes,
)
```

## Migration Guide

### From No Auth to Full Auth

1. **Run migrations**

   ```bash
   psql -d aethyme -f migrations/002_add_rls_hardening.sql
   ```

2. **Create initial admin user**

   ```python
   from src.auth.api_keys import APIKeyManager

   # Create admin API key
   key = APIKeyManager.create(
       tenant_id=your_tenant_id,
       name="Initial Admin",
       scopes=["org:admin"]
   )

   print(f"Admin key: {key['api_key']}")
   ```

3. **Update application code**

   ```python
   # Add authentication to routes
   from src.auth.middleware import require_scope

   @router.post("/", dependencies=[Depends(require_scope("repo:write"))])
   async def create_resource(user: UserContext = Depends(get_current_user)):
       # User context is automatically available
       pass
   ```

4. **Test isolation**

   ```bash
   pytest tests/auth/test_isolation.py -v
   pytest tests/auth/test_rls.py -v
   ```

## Support

For issues and questions:

- GitHub Issues: [Report bugs](https://github.com/aeptus/aethyme/issues)
- Documentation: [Full docs](https://docs.aethyme.com)
- Security: security@aethyme.com (for security vulnerabilities)

---

**Last Updated:** 2025-11-22
**Version:** 1.0.0
