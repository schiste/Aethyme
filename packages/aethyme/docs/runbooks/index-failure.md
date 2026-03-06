# Index Failure Triage Runbook

## Overview

Triage and recovery procedure for failed repository indexing jobs in Aethyme.

## Symptoms

- Indexing jobs failing repeatedly
- Repositories stuck in `pending` or `failed`
- Search or graph results stale after code changes

## Quick Reference

**Symptoms:** Indexing jobs failing, stale indexes, high error rates in indexer pods

**Common Causes:**
1. Out of memory
2. Unsupported language/file
3. Repository access denied
4. Disk space exhaustion
5. SCIP parser crash

---

## Detection

### Alerts
- `IndexerPodCrashLooping`
- `IndexStaleness` 
- `HighIndexFailureRate`

### Manual Check
```bash
# Check indexer pod status
kubectl get pods -n production -l app.kubernetes.io/component=indexer

# Check recent logs
kubectl logs -n production -l app.kubernetes.io/component=indexer --tail=100

# Check failed jobs
kubectl get jobs -n production | grep -i failed
```

---

## Triage Steps

### Step 1: Identify Failed Repositories
```bash
# Query database for failed indexes
kubectl exec -n production aethyme-postgres-0 -- psql -U postgres -d aethyme -c \
  "SELECT repository, error_message, updated_at FROM index_status WHERE status='failed' ORDER BY updated_at DESC LIMIT 20;"
```

### Step 2: Check Pod Resources
```bash
# Check memory usage
kubectl top pods -n production -l app.kubernetes.io/component=indexer

# Check disk usage
kubectl exec -n production aethyme-indexer-0 -- df -h

# Check for OOM kills
kubectl describe pod -n production aethyme-indexer-0 | grep -i oom
```

### Step 3: Review Error Logs
```bash
# Get detailed error logs
kubectl logs -n production aethyme-indexer-0 --tail=500 | grep -i error

# Check for specific error patterns
kubectl logs -n production -l app.kubernetes.io/component=indexer | \
  grep -E "(OutOfMemory|timeout|permission denied|not supported)"
```

---

## Common Issues and Fixes

### Issue 1: Out of Memory
**Symptoms:** OOMKilled status, memory limit errors

**Fix:**
```bash
# Increase memory limit temporarily
kubectl set resources deployment aethyme-indexer -n production \
  --limits=memory=16Gi --requests=memory=8Gi

# Or edit HPA
kubectl edit hpa aethyme-indexer -n production
```

**Permanent Fix:** Update `values-production.yaml` with higher limits

### Issue 2: Unsupported Language/Repository
**Symptoms:** "Language not supported" or "Parser failed" errors

**Fix:**
```bash
# Add to language allowlist
kubectl set env deployment/aethyme-indexer -n production \
  SUPPORTED_LANGUAGES="python,javascript,typescript,go,rust,java"

# Or skip problematic repository
# Update database to mark as skipped
kubectl exec -n production aethyme-postgres-0 -- psql -U postgres -d aethyme -c \
  "UPDATE repositories SET index_enabled=false WHERE name='problematic/repo';"
```

### Issue 3: Repository Access Denied
**Symptoms:** 401/403 errors, authentication failures

**Fix:**
```bash
# Check GitHub token
kubectl get secret aethyme-secret -n production -o jsonpath='{.data.GITHUB_TOKEN}' | base64 -d

# Verify token permissions
curl -H "Authorization: token $GITHUB_TOKEN" https://api.github.com/user

# Rotate token if expired
kubectl edit secret aethyme-secret -n production
```

### Issue 4: Disk Space Exhaustion
**Symptoms:** "No space left on device" errors

**Fix:**
```bash
# Check disk usage
kubectl exec -n production aethyme-indexer-0 -- du -sh /app/workspace/*

# Clean up temporary files
kubectl exec -n production aethyme-indexer-0 -- rm -rf /app/workspace/tmp/*

# Restart pod to clear ephemeral storage
kubectl delete pod -n production aethyme-indexer-0
```

### Issue 5: SCIP Parser Crash
**Symptoms:** Segmentation fault, parser core dumps

**Fix:**
```bash
# Check SCIP version
kubectl exec -n production aethyme-indexer-0 -- scip --version

# Try with fallback parser
kubectl set env deployment/aethyme-indexer -n production \
  USE_FALLBACK_PARSER=true

# Report issue to SCIP project
# https://github.com/sourcegraph/scip/issues
```

---

## Recovery Procedures

### Restart Failed Indexing Job
```bash
# Re-queue failed repository
kubectl exec -n production aethyme-postgres-0 -- psql -U postgres -d aethyme -c \
  "UPDATE index_queue SET status='pending' WHERE repository_id=123;"

# Trigger manual index via API
curl -X POST https://api.aethyme.com/v1/index \
  -H "Authorization: Bearer $API_TOKEN" \
  -d '{"repository": "owner/repo"}'
```

### Full Indexer Restart
```bash
# Rolling restart
kubectl rollout restart deployment/aethyme-indexer -n production

# Watch progress
kubectl rollout status deployment/aethyme-indexer -n production
```

### Clear Index Cache
```bash
# Clear Redis cache
kubectl exec -n production aethyme-redis-0 -- redis-cli FLUSHDB

# Clear index metadata
kubectl exec -n production aethyme-postgres-0 -- psql -U postgres -d aethyme -c \
  "TRUNCATE TABLE index_cache;"
```

---

## Escalation

If issue persists after triage:

1. **Page on-call engineer** if SLO is at risk
2. **Create incident** in PagerDuty
3. **Engage platform team** in #platform-incidents Slack channel
4. **Document findings** in incident tracking system

---

## Post-Incident

- [ ] Document root cause
- [ ] Update runbook with new patterns
- [ ] File bugs for permanent fixes
- [ ] Review and update resource limits
- [ ] Schedule post-mortem if P0/P1

---

**Last Updated:** 2024-11-22
**Owner:** Platform Team
