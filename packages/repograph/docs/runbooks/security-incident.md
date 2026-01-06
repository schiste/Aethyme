# Runbook: Security Incident Response

**Audience:** Security Team, SRE, Management
**Severity:** CRITICAL
**Last Updated:** 2025-11-22

---

## Overview

Procedures for detecting, containing, and recovering from security incidents.

**Response Time:** < 15 minutes for critical incidents

---

## Incident Classification

| Severity | Examples | Response Time |
|----------|----------|---------------|
| **P0 - Critical** | Data breach, active attack, RCE | < 15 min |
| **P1 - High** | Auth bypass, privilege escalation | < 1 hour |
| **P2 - Medium** | Brute force, DoS attempt | < 4 hours |
| **P3 - Low** | Suspicious activity, failed scans | < 24 hours |

---

## Detection

### Monitoring Alerts

```
ALERT: FailedLoginAttempts > 10 (5min)
ALERT: UnauthorizedAPIAccess
ALERT: SQLInjectionAttempt
ALERT: SuspiciousIPActivity
ALERT: DataExfiltrationSuspected
```

### Log Analysis

```bash
# Check auth failures
docker logs repograph-api | grep "401\|403" | tail -100

# Check suspicious SQL
docker logs repograph-api | grep -i "SQL\|injection" | tail -50

# Check rate limit violations
curl 'http://prometheus:9090/api/v1/query?query=rate(repograph_rate_limit_exceeded_total[5m])'
```

---

## Containment Procedures

### Step 1: Immediate Actions (< 5 minutes)

```bash
#!/bin/bash
# scripts/security-lockdown.sh

# 1. Block suspicious IP
IP_ADDRESS=$1
kubectl exec -it repograph-api-pod -- iptables -A INPUT -s $IP_ADDRESS -j DROP

# 2. Revoke compromised API key
KEY_ID=$2
psql -U repograph -d repograph -c \
  "UPDATE api_keys SET is_active = false WHERE id = '$KEY_ID';"

# 3. Blacklist JWT token
TOKEN_ID=$3
redis-cli SETEX "blacklist:$TOKEN_ID" 86400 "1"

# 4. Enable rate limiting (strict mode)
redis-cli SET "rate_limit:strict" "1"

echo "Lockdown complete"
```

### Step 2: Isolate Affected Systems

```bash
# Scale down affected pods
kubectl scale deployment repograph-api --replicas=0 -n repograph

# OR isolate specific tenant
psql -U repograph -d repograph -c \
  "UPDATE tenants SET suspended = true WHERE id = '$TENANT_ID';"
```

### Step 3: Preserve Evidence

```bash
#!/bin/bash
# scripts/collect-evidence.sh

INCIDENT_ID=$1
EVIDENCE_DIR="/var/security/incidents/$INCIDENT_ID"

mkdir -p $EVIDENCE_DIR

# Collect logs
docker logs repograph-api > $EVIDENCE_DIR/api.log
docker logs repograph-postgres > $EVIDENCE_DIR/postgres.log

# Collect database snapshot
pg_dump -U repograph -d repograph | gzip > $EVIDENCE_DIR/db-snapshot.sql.gz

# Collect metrics
curl http://prometheus:9090/api/v1/query_range?query=repograph_requests_total \
  > $EVIDENCE_DIR/metrics.json

# Collect network traffic (if available)
tcpdump -i eth0 -w $EVIDENCE_DIR/traffic.pcap

echo "Evidence collected: $EVIDENCE_DIR"
```

---

## Investigation

### Analyze Audit Logs

```sql
-- Find all actions by suspicious user
SELECT
  timestamp,
  user_id,
  action,
  resource_type,
  resource_id,
  ip_address,
  user_agent
FROM audit_logs
WHERE user_id = '{suspicious_user_id}'
  AND timestamp > NOW() - INTERVAL '24 hours'
ORDER BY timestamp DESC;

-- Find data access patterns
SELECT
  resource_type,
  COUNT(*) as access_count
FROM audit_logs
WHERE user_id = '{suspicious_user_id}'
GROUP BY resource_type
ORDER BY access_count DESC;
```

### Check for Data Exfiltration

```sql
-- Large query results
SELECT
  user_id,
  COUNT(*) as query_count,
  SUM(result_size) as total_bytes
FROM query_logs
WHERE timestamp > NOW() - INTERVAL '24 hours'
GROUP BY user_id
HAVING SUM(result_size) > 100000000  -- > 100MB
ORDER BY total_bytes DESC;
```

---

## Recovery

### Step 1: Patch Vulnerability

```bash
# Apply security patch
git checkout security-patch-v1.2.1
docker build -t repograph/api:v1.2.1-security .
kubectl set image deployment/repograph-api api=repograph/api:v1.2.1-security
```

### Step 2: Rotate Secrets

```bash
# scripts/rotate-secrets.sh

# Generate new JWT secret
NEW_JWT_SECRET=$(openssl rand -hex 32)

# Update in K8s secret
kubectl create secret generic repograph-secrets \
  --from-literal=jwt-secret=$NEW_JWT_SECRET \
  --dry-run=client -o yaml | kubectl apply -f -

# Restart pods
kubectl rollout restart deployment/repograph-api -n repograph

# Force users to re-authenticate
redis-cli FLUSHDB
```

### Step 3: Force Password Reset

```sql
-- For compromised accounts
UPDATE users
SET password_reset_required = true
WHERE id IN ('{user1}', '{user2}', '{user3}');
```

---

## Communication

### Internal Notification

```bash
# Slack incident channel
curl -X POST https://hooks.slack.com/services/YOUR/WEBHOOK \
  -d '{
    "text": "🚨 SECURITY INCIDENT P0",
    "attachments": [{
      "color": "danger",
      "fields": [
        {"title": "Incident ID", "value": "SEC-2025-001"},
        {"title": "Type", "value": "Unauthorized API access"},
        {"title": "Status", "value": "Contained"},
        {"title": "Channel", "value": "#incident-sec-001"}
      ]
    }]
  }'
```

### Customer Notification (if required)

Template for data breach notification:

```
Subject: Security Incident Notification - RepoGraph

Dear [Customer],

We are writing to inform you of a security incident involving your RepoGraph account.

What Happened:
[Brief description]

What Data Was Affected:
[Specific data types]

What We're Doing:
- Patched vulnerability
- Reset credentials
- Enhanced monitoring

What You Should Do:
- Change your password immediately
- Review audit logs for suspicious activity
- Enable 2FA if not already enabled

Contact: security@repograph.com

Sincerely,
RepoGraph Security Team
```

---

## Post-Incident Review

### Incident Report Template

```markdown
# Security Incident Report: SEC-2025-001

## Summary
Brief description of incident

## Timeline
- 10:00 - Initial detection
- 10:05 - Containment started
- 10:15 - Threat neutralized
- 10:30 - Recovery started
- 11:00 - Normal operations resumed

## Root Cause
Technical explanation

## Impact
- Affected users: 15
- Data accessed: Repository metadata
- Duration: 1 hour

## Response Actions
1. Blocked IP address
2. Revoked API keys
3. Applied security patch

## Lessons Learned
1. Need better rate limiting
2. Add IP allowlist feature
3. Improve alerting sensitivity

## Action Items
- [ ] Implement IP allowlist (Owner: Security, Due: 2025-12-01)
- [ ] Add SIEM integration (Owner: DevOps, Due: 2025-12-15)
- [ ] Security training for team (Owner: Management, Due: 2026-01-01)
```

---

## Prevention

### Security Hardening Checklist

- [ ] Enable WAF (Web Application Firewall)
- [ ] Implement IP allowlisting
- [ ] Add SIEM integration
- [ ] Enable 2FA for all users
- [ ] Regular security audits
- [ ] Penetration testing (quarterly)
- [ ] Security training (annual)
- [ ] Incident response drills (bi-annual)

---

## Related Documentation

- [Security Architecture](../architecture/security.md)
- [Security Overview](../security/security-overview.md)
- [Rollback Procedures](rollback.md)

---

**Runbook Owner:** Security Team
**Approval Status:** APPROVED
