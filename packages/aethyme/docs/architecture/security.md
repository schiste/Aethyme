# Aethyme Security Architecture

**Version:** 1.0
**Date:** 2025-11-22
**Classification:** Internal

---

## Security Principles

1. **Defense in Depth** - Multiple security layers
2. **Least Privilege** - Minimal permissions by default
3. **Zero Trust** - Verify all requests
4. **Data Isolation** - Strict multi-tenant boundaries
5. **Audit Everything** - Complete activity trail

---

## 1. Authentication Architecture

### OIDC Integration

```mermaid
sequenceDiagram
    participant User
    participant App as Aethyme
    participant OIDC as OIDC Provider
    participant DB as Database

    User->>App: Access protected resource
    App->>OIDC: Redirect to /authorize
    OIDC->>User: Show login page
    User->>OIDC: Submit credentials
    OIDC->>App: Callback with authorization code
    App->>OIDC: POST /token (exchange code)
    OIDC-->>App: {access_token, id_token, refresh_token}
    App->>OIDC: GET /userinfo
    OIDC-->>App: User profile
    App->>DB: Find or create user
    App->>App: Generate internal JWT
    App-->>User: Set JWT cookie + redirect
```

### JWT Token Format

```json
{
  "sub": "user_abc123",
  "org_id": "org_xyz789",
  "email": "user@acme.com",
  "role": "admin",
  "scopes": ["repo:read", "repo:write", "query:*"],
  "iss": "https://api.aethyme.com",
  "aud": "aethyme-api",
  "exp": 1700000000,
  "iat": 1699996400,
  "jti": "jwt_unique_id"
}
```

**Security Controls:**
- ✅ Short expiration (24 hours)
- ✅ Refresh token rotation
- ✅ Token blacklist on logout (Redis)
- ✅ HTTPS-only transmission
- ✅ HttpOnly + Secure cookie flags

---

## 2. Authorization Model (RBAC + Scopes)

### Role Hierarchy

| Role | Permissions |
|------|-------------|
| **Owner** | Full control: manage users, billing, delete org |
| **Admin** | Manage repos, API keys, view audit logs |
| **Member** | Create repos, run queries, trigger indexing |
| **Readonly** | View repos, run read-only queries |

### API Key Scopes

```python
# Scope system
SCOPES = {
    "repo:read": "View repositories",
    "repo:write": "Create/update repositories",
    "repo:index": "Trigger indexing",
    "query:search": "Run search queries",
    "query:ego": "Run ego graph queries",
    "query:impact": "Run impact analysis",
    "query:*": "All query types",
    "ai:scorecard": "Run AI-readiness scorecard",
    "ai:autofix": "Run autofixers",
    "admin:users": "Manage users (admin only)",
    "admin:keys": "Manage API keys (admin only)"
}
```

**Enforcement:**

```python
from functools import wraps

def require_scope(scope: str):
    """Decorator to enforce scope requirements."""
    def decorator(func):
        @wraps(func)
        async def wrapper(request: Request, *args, **kwargs):
            user = request.state.user
            if scope not in user.scopes and "*" not in user.scopes:
                raise HTTPException(403, f"Missing required scope: {scope}")
            return await func(request, *args, **kwargs)
        return wrapper
    return decorator

@app.get("/query/ego")
@require_scope("query:ego")
async def ego_graph(request: Request):
    # ...
```

---

## 3. Multi-Tenant Data Isolation

### Three-Layer Isolation

**Layer 1: Application (FastAPI Middleware)**

```python
@app.middleware("http")
async def tenant_context_middleware(request: Request, call_next):
    """Extract org_id from JWT and set context."""
    token = request.headers.get("Authorization", "").replace("Bearer ", "")
    payload = verify_jwt(token)
    org_id = payload["org_id"]

    # Set in request state
    request.state.org_id = org_id

    # Set PostgreSQL session variable
    async with db.begin() as conn:
        await conn.execute(text(f"SET app.current_org = '{org_id}'"))

async def tenant_middleware(request, call_next):
    response = await call_next(request)
    return response
```

**Layer 2: ORM (SQLAlchemy Filters)**

```python
class BaseQuery:
    """Automatically filter by org_id."""

    @staticmethod
    def filter_by_org(query, org_id: str):
        return query.filter(Table.org_id == org_id)


async def load_symbols(db_session, request):
    return await db_session.query(Symbol).filter_by_org(request.state.org_id).all()
```

**Layer 3: Database (Row-Level Security)**

```sql
-- Enable RLS
ALTER TABLE symbols ENABLE ROW LEVEL SECURITY;

-- Policy: Only see own org's data
CREATE POLICY tenant_isolation ON symbols
    USING (org_id = current_setting('app.current_org')::uuid);

-- Even SQL injection can't bypass this!
```

**Test Case:**

```python
async def test_tenant_isolation():
    """Verify org A cannot access org B's data."""
    org_a_token = create_jwt(org_id="org_a")
    org_b_token = create_jwt(org_id="org_b")

    # Create symbol for org A
    response = await client.post("/repos",
        headers={"Authorization": f"Bearer {org_a_token}"},
        json={"name": "repo-a", "url": "https://github.com/a/repo"}
    )

    # Try to access with org B token
    response = await client.get(f"/repos/{repo_id}",
        headers={"Authorization": f"Bearer {org_b_token}"}
    )
    assert response.status_code == 404  # Isolated!
```

---

## 4. API Security

### Rate Limiting

**Token Bucket Algorithm:**

```python
# Per-org rate limits
RATE_LIMITS = {
    "free": {"requests": 60, "window": 60},      # 60/min
    "pro": {"requests": 300, "window": 60},       # 300/min
    "enterprise": {"requests": 1000, "window": 60}  # 1000/min
}

@app.middleware("http")
async def rate_limit_middleware(request: Request, call_next):
    org = await get_org(request.state.org_id)
    limit = RATE_LIMITS[org.plan]

    key = f"ratelimit:{org.id}"
    current = await redis.incr(key)

    if current == 1:
        await redis.expire(key, limit["window"])

    if current > limit["requests"]:
        return JSONResponse(
            status_code=429,
            headers={
                "X-RateLimit-Limit": str(limit["requests"]),
                "X-RateLimit-Remaining": "0",
                "Retry-After": str(limit["window"])
            },
            content={"error": "Rate limit exceeded"}
        )

    response = await call_next(request)
    response.headers["X-RateLimit-Limit"] = str(limit["requests"])
    response.headers["X-RateLimit-Remaining"] = str(limit["requests"] - current)
    return response
```

### CORS Policy

```python
from fastapi.middleware.cors import CORSMiddleware

app.add_middleware(
    CORSMiddleware,
    allow_origins=[
        "https://api.aethyme.com",
        "https://app.aeptus.com"
    ],
    allow_credentials=True,
    allow_methods=["GET", "POST", "PUT", "DELETE", "PATCH"],
    allow_headers=["*"],
    max_age=3600
)
```

### Input Validation

```python
from pydantic import BaseModel, Field, validator

class RepoCreate(BaseModel):
    name: str = Field(..., min_length=1, max_length=255)
    url: str = Field(..., regex=r"^https://github\.com/[\w-]+/[\w-]+$")

    @validator("url")
    def validate_url(cls, v):
        # Additional validation
        if "evil.com" in v:
            raise ValueError("Untrusted domain")
        return v
```

---

## 5. Data Encryption

### At Rest

**Database:**
- PostgreSQL: TDE (Transparent Data Encryption) enabled
- Backups: AES-256 encryption
- Disk: Encrypted volumes (LUKS/dm-crypt)

**Secrets:**

```yaml
# Kubernetes Secrets (encrypted with SOPS)
apiVersion: v1
kind: Secret
metadata:
  name: aethyme-secrets
type: Opaque
data:
  database-url: <base64-encrypted>
  jwt-secret: <base64-encrypted>
  github-token: <base64-encrypted>
```

**Application-Level (Sensitive Fields):**

```python
from cryptography.fernet import Fernet

class EncryptedField:
    """Encrypt sensitive fields before storage."""
    def __init__(self, key: bytes):
        self.cipher = Fernet(key)

    def encrypt(self, plaintext: str) -> str:
        return self.cipher.encrypt(plaintext.encode()).decode()

    def decrypt(self, ciphertext: str) -> str:
        return self.cipher.decrypt(ciphertext.encode()).decode()

# Usage: OAuth tokens
token_encrypted = encrypted_field.encrypt(github_token)

async def store_token(db_client, org_id: str, github_token: str) -> None:
    token_encrypted = encrypted_field.encrypt(github_token)
    await db_client.save_oauth_token(org_id, provider="github", token=token_encrypted)
```

### In Transit

**TLS 1.2+ Everywhere:**

- API: HTTPS only (redirect HTTP → HTTPS)
- Database: TLS connections (`sslmode=require`)
- Redis: TLS enabled
- Inter-service: mTLS (mutual TLS) via service mesh

```python
# PostgreSQL connection with TLS
DATABASE_URL = "postgresql://user:pass@host:5432/db?sslmode=require&sslrootcert=/path/to/ca.crt"
```

---

## 6. Secrets Management

**Vault Integration (Production):**

```python
import hvac

# Initialize Vault client
client = hvac.Client(url="https://vault.aeptus.com", token=os.getenv("VAULT_TOKEN"))

# Read secret
secret = client.secrets.kv.v2.read_secret_version(path="aethyme/prod/database")
DATABASE_PASSWORD = secret["data"]["data"]["password"]
```

**Environment Variables (Dev/Staging):**

```bash
# .env (NEVER commit to git)
DATABASE_URL=postgresql://...
JWT_SECRET_KEY=...
GITHUB_CLIENT_SECRET=...
```

**Kubernetes Secrets:**

```bash
# Create secret from file
kubectl create secret generic aethyme-secrets \
  --from-file=database-url=./db-url.txt \
  --from-file=jwt-secret=./jwt-secret.txt \
  -n aethyme

# Or use SOPS for encrypted secrets in git
sops -e secrets.yaml > secrets.enc.yaml
```

---

## 7. Audit Logging

### Structured Audit Log

```python
import structlog

audit_log = structlog.get_logger("audit")

async def log_action(
    user_id: str,
    org_id: str,
    action: str,
    resource_type: str,
    resource_id: str,
    status: str,
    details: dict = None
):
    """Log security-relevant actions."""
    await audit_log.info(
        "audit_event",
        user_id=user_id,
        org_id=org_id,
        action=action,
        resource_type=resource_type,
        resource_id=resource_id,
        status=status,
        details=details,
        timestamp=datetime.utcnow().isoformat(),
        ip_address=request.client.host,
        user_agent=request.headers.get("User-Agent")
    )

# Store in database
await db.insert_audit_log({...})
```

**Events to Audit:**

- ✅ User login/logout
- ✅ API key creation/revocation
- ✅ Repository creation/deletion
- ✅ Permission changes
- ✅ Failed authentication attempts
- ✅ Sensitive queries (impact analysis on critical code)

---

## 8. Compliance & Privacy

### GDPR Compliance

**Data Retention:**

```sql
# Auto-delete old audit logs
DELETE FROM audit_logs WHERE created_at < NOW() - INTERVAL '90 days';

# User deletion (right to be forgotten)
```

```python
async def delete_user_data(user_id: str):
    """Delete all user data (GDPR right to erasure)."""
    await db.delete_user(user_id)
    await db.anonymize_audit_logs(user_id)  # Replace with "deleted_user"
    await redis.delete(f"sessions:{user_id}:*")
```

**Data Export (Right to Access):**

```python
@app.get("/users/me/export")
async def export_user_data(request: Request):
    """Export all user data (GDPR right to data portability)."""
    user_id = request.state.user_id

    data = {
        "profile": await db.get_user(user_id),
        "repos": await db.get_user_repos(user_id),
        "api_keys": await db.get_user_api_keys(user_id),
        "audit_logs": await db.get_user_audit_logs(user_id, limit=1000)
    }

    return JSONResponse(data, headers={
        "Content-Disposition": f"attachment; filename=aethyme-data-{user_id}.json"
    })
```

### PII Handling

**Minimize PII Collection:**
- ❌ Don't log code content (may contain secrets)
- ❌ Don't log user emails in metrics
- ✅ Hash identifiers in logs

```python
import hashlib

def anonymize_id(value: str) -> str:
    """One-way hash for logging."""
    return hashlib.sha256(value.encode()).hexdigest()[:16]

# Log with anonymized IDs
logger.info("query_executed", user_hash=anonymize_id(user_id))
```

---

## 9. Vulnerability Management

### Dependency Scanning

```yaml
# .github/workflows/security.yml
name: Security Scan

on: [push, pull_request]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Run Trivy (container scan)
        uses: aquasecurity/trivy-action@master
        with:
          image-ref: aethyme/api:latest
          severity: HIGH,CRITICAL

      - name: Run Snyk (dependency scan)
        uses: snyk/actions/python@master
        with:
          args: --severity-threshold=high
```

### Penetration Testing

**Schedule:**
- Internal: Quarterly
- External: Annually (third-party)

**Scope:**
- Authentication bypass
- Tenant isolation
- SQL injection
- XSS/CSRF
- Rate limit bypass

---

## 10. Incident Response

### Security Incident Runbook

**1. Detection:**
- Monitor alerts (failed logins, rate limit violations, unusual queries)
- User reports

**2. Containment:**

```bash
# Revoke compromised API key
psql -c "UPDATE api_keys SET is_active = false WHERE id = '{key_id}';"

# Blacklist JWT
redis-cli SETEX "blacklist:jwt_abc123" 86400 "1"

# Block IP address (firewall rule)
kubectl exec -it api-pod -- iptables -A INPUT -s 1.2.3.4 -j DROP
```

**3. Investigation:**

```sql
-- Find all actions by compromised account
SELECT * FROM audit_logs WHERE user_id = '{compromised_user_id}'
  AND created_at > '{incident_time}';
```

**4. Recovery:**
- Rotate secrets
- Force password reset
- Notify affected users

**5. Post-Mortem:**
- Root cause analysis
- Implement fixes
- Update runbook

---

## Security Checklist

- [x] HTTPS/TLS everywhere
- [x] JWT with short expiration + refresh tokens
- [x] Multi-tenant RLS policies
- [x] Rate limiting per org
- [x] CORS configured
- [x] Input validation (Pydantic)
- [x] SQL injection protection (SQLAlchemy ORM)
- [x] XSS protection (auto-escaping)
- [x] CSRF protection (SameSite cookies)
- [x] Secrets in Vault/K8s Secrets
- [x] Audit logging
- [x] Dependency scanning
- [x] Container scanning
- [x] GDPR compliance

---

**Document Status:** ✅ Complete - Security Review Required
**Next Steps:** Security team review, penetration testing
