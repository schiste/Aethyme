# Privacy Policy

**Effective Date:** 2025-11-22
**Last Updated:** 2025-11-22

---

## Data Collection

### What We Collect

**Repository Data:**
- Code structure (symbols, relationships)
- File names and paths
- Language metadata
- Repository metadata (name, URL)

**User Data:**
- Email address
- Name (optional)
- Organization affiliation
- Authentication credentials (hashed)

**Usage Data:**
- API requests (endpoints, timestamps)
- Query patterns
- Performance metrics
- Error logs

**We DO NOT collect:**
- Source code content
- Secrets or credentials from code
- Personal data from code comments
- Private communications

---

## How We Use Data

1. **Provide Services**: Code intelligence, search, analysis
2. **Improve Performance**: Optimize queries, caching
3. **Security**: Detect and prevent abuse
4. **Support**: Troubleshoot issues
5. **Analytics**: Aggregate usage statistics

---

## Data Retention

| Data Type | Retention Period |
|-----------|------------------|
| Code graphs | Until repository deleted |
| Audit logs | 90 days |
| Usage metrics | 1 year (aggregated) |
| Backups | 30 days |
| Deleted accounts | 30 days (hard delete) |

---

## Data Sharing

**We do NOT:**
- Sell your data
- Share with third parties for marketing
- Use code data for training AI models

**We MAY share with:**
- Service providers (hosting, monitoring)
- Legal authorities (if required by law)

---

## User Rights (GDPR)

### Right to Access

Export your data:

```bash
curl http://api.repograph.com/users/me/export \
  -H "Authorization: Bearer $TOKEN" \
  > my-data.json
```

### Right to Erasure

Delete your account and data:

```bash
curl -X DELETE http://api.repograph.com/users/me \
  -H "Authorization: Bearer $TOKEN"
```

**Note:** Deletion is permanent after 30-day grace period.

### Right to Rectification

Update your profile:

```bash
curl -X PATCH http://api.repograph.com/users/me \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"name": "New Name"}'
```

---

## Data Security

- **Encryption**: TLS 1.2+ for all data in transit
- **Database**: Encrypted at rest (AES-256)
- **Access Control**: Role-based permissions
- **Isolation**: Multi-tenant RLS policies
- **Monitoring**: 24/7 security monitoring

---

## Cookies

We use cookies for:
- **Authentication**: Session management (HttpOnly, Secure)
- **Preferences**: User settings (functional)

**We do NOT use:**
- Third-party tracking cookies
- Advertising cookies

---

## Contact

**Privacy Questions:** privacy@repograph.com
**Security Issues:** security@repograph.com
**Data Protection Officer:** dpo@aeptus.com

---

## Changes to This Policy

We will notify you of material changes:
- Email notification
- In-app announcement
- 30 days notice before taking effect

**Last Updated:** 2025-11-22
