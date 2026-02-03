# Threat Model

**Version:** 1.0
**Last Updated:** 2025-11-22

---

## Assets

### Critical Assets

1. **Code Graphs**: Proprietary code structure data
2. **User Credentials**: Passwords, API keys, tokens
3. **Tenant Data**: Multi-tenant isolation integrity
4. **Database**: PostgreSQL data integrity
5. **Service Availability**: API uptime

---

## Threat Actors

| Actor | Motivation | Capability | Likelihood |
|-------|------------|------------|------------|
| **External Attacker** | Data theft, ransom | Medium-High | Medium |
| **Malicious User** | Privilege escalation | Medium | Low |
| **Insider Threat** | Data exfiltration | High | Very Low |
| **Automated Bots** | Resource abuse, spam | Low | High |
| **Nation State** | Espionage | Very High | Very Low |

---

## Attack Vectors

### 1. Authentication Bypass

**Threat:** Attacker gains unauthorized access

**Attack Methods:**
- Brute force passwords
- JWT token theft/replay
- Session hijacking
- Credential stuffing

**Mitigations:**
- ✅ Rate limiting (failed login attempts)
- ✅ JWT short expiration (24h)
- ✅ Token blacklist on logout
- ✅ HTTPS-only (prevents MITM)
- ✅ HttpOnly + Secure cookies
- 🔄 2FA (planned)
- 🔄 IP allowlisting (planned)

**Residual Risk:** LOW

---

### 2. Tenant Isolation Breach

**Threat:** Org A accesses Org B's data

**Attack Methods:**
- SQL injection to bypass RLS
- Parameter tampering (org_id manipulation)
- Shared cache poisoning

**Mitigations:**
- ✅ Three-layer isolation (app + ORM + RLS)
- ✅ SQL injection protection (SQLAlchemy ORM)
- ✅ Input validation (Pydantic)
- ✅ Tenant-scoped cache keys
- ✅ Isolation test suite

**Residual Risk:** VERY LOW

---

### 3. Denial of Service (DoS)

**Threat:** Service unavailable due to resource exhaustion

**Attack Methods:**
- API flooding
- Expensive queries
- Large repository indexing
- Connection pool exhaustion

**Mitigations:**
- ✅ Rate limiting per tenant
- ✅ Query timeouts
- ✅ Connection pool limits
- ✅ Request size limits
- 🔄 WAF (Web Application Firewall) - planned
- 🔄 DDoS protection (Cloudflare) - planned

**Residual Risk:** MEDIUM

---

### 4. Data Exfiltration

**Threat:** Sensitive code data stolen

**Attack Methods:**
- Bulk API queries
- Database dump access
- Backup theft
- Compromised credentials

**Mitigations:**
- ✅ Rate limiting (prevents bulk export)
- ✅ Audit logging (detect anomalies)
- ✅ Encrypted backups (AES-256)
- ✅ Access controls (RBAC)
- ✅ Database TDE (Transparent Data Encryption)
- 🔄 Data loss prevention (DLP) - planned

**Residual Risk:** LOW

---

### 5. Code Injection

**Threat:** Malicious code execution

**Attack Methods:**
- SQL injection
- Command injection (indexer)
- Stored XSS
- Template injection

**Mitigations:**
- ✅ ORM (prevents SQL injection)
- ✅ Input sanitization
- ✅ Output encoding (prevents XSS)
- ✅ Safe subprocess calls (indexer)
- ✅ Content Security Policy headers

**Residual Risk:** LOW

---

### 6. Privilege Escalation

**Threat:** User gains unauthorized permissions

**Attack Methods:**
- IDOR (Insecure Direct Object References)
- Role manipulation
- API key reuse

**Mitigations:**
- ✅ Authorization checks on all endpoints
- ✅ Scoped API keys
- ✅ Audit logging
- ✅ Least privilege by default

**Residual Risk:** LOW

---

### 7. Supply Chain Attack

**Threat:** Compromised dependencies

**Attack Methods:**
- Malicious PyPI package
- Vulnerable npm package
- Compromised Docker image

**Mitigations:**
- ✅ Dependency scanning (Snyk, Dependabot)
- ✅ Lock files (requirements.txt, package-lock.json)
- ✅ Image scanning (Trivy)
- ✅ Automated security updates
- 🔄 SBOM (Software Bill of Materials) - planned

**Residual Risk:** MEDIUM

---

### 8. Insider Threat

**Threat:** Malicious or negligent employee

**Attack Methods:**
- Database access abuse
- Credential sharing
- Data deletion
- Backdoor insertion

**Mitigations:**
- ✅ Audit logging (all DB access)
- ✅ Code review (all changes)
- ✅ Least privilege (role-based access)
- ✅ Background checks (hiring)
- 🔄 Privileged Access Management (PAM) - planned

**Residual Risk:** LOW

---

## Risk Matrix

| Threat | Likelihood | Impact | Risk Score | Priority |
|--------|------------|--------|------------|----------|
| Authentication Bypass | Medium | High | Medium | P1 |
| Tenant Isolation Breach | Low | Critical | Medium | P0 |
| Denial of Service | High | Medium | Medium | P2 |
| Data Exfiltration | Low | High | Medium | P1 |
| Code Injection | Low | High | Low | P2 |
| Privilege Escalation | Low | Medium | Low | P3 |
| Supply Chain Attack | Medium | High | Medium | P1 |
| Insider Threat | Very Low | High | Low | P2 |

---

## Security Controls

### Detective Controls

- Audit logging
- Intrusion detection
- Anomaly detection (usage patterns)
- Security scanning (Snyk, Trivy)

### Preventive Controls

- Authentication (JWT, OIDC)
- Authorization (RBAC, RLS)
- Encryption (TLS, AES-256)
- Input validation
- Rate limiting

### Corrective Controls

- Incident response runbook
- Automated backups
- Rollback procedures
- Security patches

---

## Attack Scenarios

### Scenario 1: Credential Compromise

**Sequence:**
1. Attacker obtains user password (phishing)
2. Logs in with valid credentials
3. Attempts to export large amounts of data

**Defense:**
- Rate limiting prevents bulk export
- Audit logs detect anomalous activity
- Alert triggered to security team
- Account suspended, user notified

### Scenario 2: SQL Injection Attempt

**Sequence:**
1. Attacker sends malicious query: `'; DROP TABLE nodes; --`
2. SQLAlchemy ORM escapes input
3. Query fails safely
4. Attempt logged

**Defense:**
- ORM prevents SQL injection
- WAF blocks malicious patterns (future)
- Alert on repeated injection attempts

### Scenario 3: Tenant Isolation Attack

**Sequence:**
1. Attacker in Org A tries: `?org_id=org_b`
2. Middleware extracts org_id from JWT (ignores parameter)
3. RLS policy enforces isolation
4. Query returns 0 results

**Defense:**
- Three-layer isolation prevents bypass
- Audit log records attempt

---

## Future Enhancements

### Planned Security Features

- [ ] **2FA/MFA**: Multi-factor authentication
- [ ] **WAF**: Web Application Firewall
- [ ] **DDoS Protection**: Cloudflare/AWS Shield
- [ ] **SIEM Integration**: Security Information and Event Management
- [ ] **Penetration Testing**: Annual third-party assessment
- [ ] **Bug Bounty Program**: Responsible disclosure incentives
- [ ] **SOC 2 Certification**: Compliance audit

---

## Related Documentation

- [Security Overview](security-overview.md)
- [Privacy Policy](privacy-policy.md)
- [Security Incident Runbook](../runbooks/security-incident.md)

---

**Document Owner:** Security Team
**Review Cycle:** Quarterly
**Next Review:** 2026-02-22
