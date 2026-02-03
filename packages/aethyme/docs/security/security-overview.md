# Security Overview

**Version:** 1.0
**Last Updated:** 2025-11-22

---

## Security Principles

Aethyme follows security best practices:

1. **Defense in Depth**: Multiple security layers
2. **Least Privilege**: Minimal permissions by default
3. **Zero Trust**: Verify all requests
4. **Data Isolation**: Strict multi-tenant boundaries
5. **Audit Everything**: Complete activity logging

---

## Authentication

### JWT-Based Authentication

- Short-lived tokens (24 hours)
- Refresh token rotation
- HTTPS-only transmission
- HttpOnly + Secure cookies

### OIDC Integration

Enterprise deployments support OpenID Connect:
- Google Workspace
- Azure AD
- Okta
- Auth0

### API Keys

For CI/CD and automation:
- Scoped permissions
- Revocable
- Rate limited
- Audit logged

---

## Authorization

### Role-Based Access Control (RBAC)

| Role | Permissions |
|------|-------------|
| **Owner** | Full control |
| **Admin** | Manage repos, API keys, view audit logs |
| **Member** | Create repos, run queries |
| **Readonly** | View repos, run read-only queries |

### Scoped Tokens

```json
{
  "scopes": [
    "repo:read",
    "repo:write",
    "query:search",
    "query:ego",
    "query:impact"
  ]
}
```

---

## Multi-Tenant Isolation

### Three-Layer Security

1. **Application Layer**: FastAPI middleware filters by org_id
2. **ORM Layer**: SQLAlchemy automatic filters
3. **Database Layer**: PostgreSQL Row-Level Security (RLS)

### RLS Policies

```sql
-- Enable RLS on all tables
ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;

-- Policy: users only see their org's data
CREATE POLICY tenant_isolation ON nodes
  USING (org_id = current_setting('app.current_org')::uuid);
```

**Result:** Even SQL injection cannot bypass tenant isolation.

---

## Data Encryption

### At Rest

- **Database**: PostgreSQL TDE enabled
- **Backups**: AES-256 encryption
- **Secrets**: Vault/K8s Secrets encrypted

### In Transit

- **HTTPS**: TLS 1.2+ only
- **Database**: TLS connections required
- **Redis**: TLS enabled
- **Service Mesh**: mTLS for inter-service

---

## Secrets Management

### Production

- **HashiCorp Vault** for secret storage
- **Kubernetes Secrets** with SOPS encryption
- **Automated rotation** for credentials

### Development

- `.env` files (never committed to Git)
- `.env.example` for templates
- Local-only test credentials

---

## Rate Limiting

Per-tenant rate limits:

- **Free**: 60 requests/minute
- **Pro**: 300 requests/minute
- **Enterprise**: 1000 requests/minute

Prevents abuse and DoS attacks.

---

## Audit Logging

All security-relevant actions logged:

- User login/logout
- API key creation/revocation
- Repository access
- Permission changes
- Failed authentication attempts

**Retention**: 90 days (configurable)

---

## Vulnerability Management

### Dependency Scanning

- **Automated**: GitHub Dependabot
- **CI/CD**: Snyk/Trivy in pipeline
- **Frequency**: Every commit + weekly scan

### Penetration Testing

- **Internal**: Quarterly
- **External**: Annually (third-party)

### Responsible Disclosure

Security issues: security@aethyme.com

---

## Compliance

### GDPR

- Right to access (data export)
- Right to erasure (account deletion)
- Data minimization
- Audit trails

### SOC 2 (Planned)

- Access controls
- Encryption
- Monitoring
- Incident response

---

## Security Checklist

- [x] HTTPS/TLS everywhere
- [x] JWT with short expiration
- [x] Multi-tenant RLS policies
- [x] Rate limiting
- [x] Input validation
- [x] SQL injection protection (ORM)
- [x] XSS protection (auto-escaping)
- [x] CSRF protection
- [x] Secrets management
- [x] Audit logging
- [x] Dependency scanning

---

## Related Documentation

- [Architecture Security](../architecture/security.md) - Technical implementation
- [Privacy Policy](privacy-policy.md) - Data handling
- [Threat Model](threat-model.md) - Risk analysis
- [Security Incident Runbook](../runbooks/security-incident.md)

---

**Document Owner:** Security Team
**Review Cycle:** Quarterly
