# Runbook: Staleness Remediation

**Audience:** Operations, DevOps, Platform Engineers
**Severity:** MEDIUM
**Last Updated:** 2025-11-22
**Version:** 1.0

---

## Overview

This runbook provides procedures for detecting and remediating stale repository indexes in Aethyme. Stale indexes occur when the code has changed but the graph hasn't been updated, leading to incorrect query results and outdated AI-readiness analysis.

**SLA Impact:** Stale indexes impact data freshness SLO (target: indexes < 24 hours old).

---

## Symptoms

### Primary Indicators

- Repository `indexed_at` timestamp > 24 hours old
- Repository `freshness` status shows `stale` in API response
- Search results don't include recently added code
- Ego/impact queries show outdated relationships
- Metrics dashboard shows freshness violations > 5%

### User-Facing Symptoms

- AI assistant references old code that no longer exists
- Search doesn't find newly added functions/classes
- Impact analysis misses new dependencies
- Scorecard doesn't detect recently introduced issues

### Monitoring Alerts

```
ALERT: IndexFreshnessViolation > 5% of repositories
ALERT: IndexAge > 86400s (24 hours)
ALERT: StalenessDetectorFailed
```

---

## Detection Methods

### Method 1: API Endpoint

```bash
# Check freshness via API
curl http://localhost:8001/api/index/status/{repository_id} \
  -H "Authorization: Bearer $TOKEN" | jq '.freshness'

# Expected: "current" or "stale"
```

### Method 2: Database Query

```sql
-- Find all stale repositories
SELECT
  id,
  name,
  path,
  indexed_at,
  EXTRACT(EPOCH FROM (NOW() - indexed_at)) AS age_seconds,
  status
FROM repositories
WHERE
  indexed_at < NOW() - INTERVAL '24 hours'
  AND status = 'completed'
ORDER BY indexed_at ASC;
```

### Method 3: Metrics Dashboard

```bash
# Query Prometheus metrics
curl 'http://localhost:9090/api/v1/query?query=aethyme_index_age_seconds' | jq .

# Check freshness percentage
curl 'http://localhost:9090/api/v1/query?query=aethyme_index_freshness_ratio' | jq .
```

### Method 4: Automated Staleness Detector

```bash
# Run staleness detector script
cd packages/aethyme
python scripts/detect-stale-indexes.py --threshold 86400

# Output:
# Stale Repositories (3):
#   - my-project (age: 172800s, last indexed: 2 days ago)
#   - api-service (age: 129600s, last indexed: 1.5 days ago)
#   - frontend (age: 90000s, last indexed: 25 hours ago)
```

---

## Root Causes

### 1. Watch Service Disabled or Failed

**Symptoms:**
- File changes detected but re-indexing not triggered
- Watch service logs show errors
- Environment variable `WATCH_ENABLED=false`

**Detection:**
```bash
# Check watch service status
docker ps | grep watch
kubectl get pods -n aethyme | grep watch

# Check environment variable
docker exec aethyme-api env | grep WATCH_ENABLED

# Check logs
docker logs aethyme-watch --tail=100
```

### 2. Git Hook Not Installed

**Symptoms:**
- Manual git pushes don't trigger re-indexing
- Post-receive hook missing or failing

**Detection:**
```bash
# Check git hooks
ls -la /path/to/repo/.git/hooks/
cat /path/to/repo/.git/hooks/post-receive

# Expected: Script that calls Aethyme API
```

### 3. Scheduled Job Failed

**Symptoms:**
- Nightly re-index cron job didn't run
- Kubernetes CronJob in failed state

**Detection:**
```bash
# Check cron job status (Linux)
systemctl status cron
crontab -l

# Check Kubernetes CronJob
kubectl get cronjobs -n aethyme
kubectl get jobs -n aethyme --sort-by=.status.startTime
```

### 4. Resource Constraints

**Symptoms:**
- Re-indexing skipped due to high load
- Background workers exhausted

**Detection:**
```bash
# Check worker queue depth
redis-cli LLEN indexing_queue

# Check CPU/memory usage
top -bn1 | head -20
docker stats --no-stream
```

---

## Remediation Procedures

### Procedure 1: Manual Re-indexing (Single Repository)

```bash
# Via CLI
cd packages/aethyme
python -m src.cli index /path/to/repo --name my-project

# Via API
curl -X POST http://localhost:8001/api/index \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "repo_path": "/path/to/repo",
    "repo_name": "my-project"
  }'
```

**Use Case:** Quick fix for a single stale repository

### Procedure 2: Batch Re-indexing (All Stale Repositories)

```bash
#!/bin/bash
# scripts/reindex-stale.sh

# Get all stale repository IDs
STALE_REPOS=$(psql -U aethyme -d aethyme -tAc "
  SELECT id, path FROM repositories
  WHERE indexed_at < NOW() - INTERVAL '24 hours'
    AND status = 'completed';
")

# Re-index each
while IFS='|' read -r repo_id repo_path; do
  echo "Re-indexing: $repo_path (ID: $repo_id)"

  curl -X POST http://localhost:8001/api/index \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "{
      \"repo_path\": \"$repo_path\"
    }"

  # Throttle to avoid overload
  sleep 10
done <<< "$STALE_REPOS"

echo "Batch re-indexing complete"
```

**Use Case:** Remediate multiple stale repositories at once

### Procedure 3: Enable Watch Service

```bash
# Update .env
echo "WATCH_ENABLED=true" >> /path/to/aethyme/.env
echo "WATCH_BATCH_INTERVAL=300" >> /path/to/aethyme/.env

# Restart services
docker-compose -f ops/docker-compose.yml restart api watch

# Verify watch service is running
docker ps | grep watch
docker logs aethyme-watch --tail=20
```

**Use Case:** Enable automatic staleness detection

### Procedure 4: Install Git Hooks

```bash
#!/bin/bash
# scripts/install-git-hooks.sh

REPO_PATH=$1
AETHYME_API_URL=${2:-"http://localhost:8001"}
AETHYME_TOKEN=$3

# Create post-receive hook
cat > "$REPO_PATH/.git/hooks/post-receive" <<'EOF'
#!/bin/bash
# Aethyme auto-reindex on push

REPO_PATH=$(git rev-parse --show-toplevel)
REPO_NAME=$(basename "$REPO_PATH")

curl -X POST ${AETHYME_API_URL}/api/index \
  -H "Authorization: Bearer ${AETHYME_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{
    \"repo_path\": \"$REPO_PATH\",
    \"repo_name\": \"$REPO_NAME\"
  }" \
  > /dev/null 2>&1 &

echo "Aethyme re-index triggered"
EOF

# Make executable
chmod +x "$REPO_PATH/.git/hooks/post-receive"

echo "Git hook installed at $REPO_PATH/.git/hooks/post-receive"
```

**Usage:**
```bash
./scripts/install-git-hooks.sh /path/to/repo http://localhost:8001 $TOKEN
```

### Procedure 5: Schedule Periodic Re-indexing

#### Option A: Cron Job (Linux)

```bash
# Add to crontab
crontab -e

# Add line (re-index daily at 2 AM)
0 2 * * * /path/to/aethyme/scripts/reindex-stale.sh >> /var/log/aethyme-reindex.log 2>&1
```

#### Option B: Kubernetes CronJob

```yaml
# k8s/cronjob-reindex.yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: aethyme-reindex-stale
  namespace: aethyme
spec:
  schedule: "0 2 * * *"  # Daily at 2 AM
  jobTemplate:
    spec:
      template:
        spec:
          containers:
          - name: reindex
            image: aethyme/cli:latest
            command:
            - /bin/bash
            - -c
            - |
              #!/bin/bash
              # Get stale repos and re-index
              python /app/scripts/reindex-stale.py --threshold 86400
          restartPolicy: OnFailure
```

Apply:
```bash
kubectl apply -f k8s/cronjob-reindex.yaml
```

#### Option C: GitHub Actions

```yaml
# .github/workflows/reindex.yml
name: Daily Re-index

on:
  schedule:
    - cron: '0 2 * * *'  # Daily at 2 AM UTC
  workflow_dispatch:  # Manual trigger

jobs:
  reindex:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v3

      - name: Re-index stale repositories
        env:
          AETHYME_TOKEN: ${{ secrets.AETHYME_TOKEN }}
        run: |
          cd packages/aethyme
          python scripts/reindex-stale.py --threshold 86400
```

### Procedure 6: Increase Re-indexing Frequency

For critical repositories that change frequently:

```bash
# Set shorter freshness threshold
psql -U aethyme -d aethyme <<EOF
UPDATE repositories
SET metadata = jsonb_set(
  COALESCE(metadata, '{}'::jsonb),
  '{freshness_threshold}',
  '3600'  -- 1 hour instead of 24 hours
)
WHERE name IN ('critical-api', 'core-services');
EOF

# Adjust watch service interval
# In .env
WATCH_BATCH_INTERVAL=60  # Check every minute instead of 5 minutes

# Restart watch service
docker-compose -f ops/docker-compose.yml restart watch
```

---

## Monitoring Improvements

### 1. Add Freshness Dashboard

```yaml
# grafana/dashboards/freshness.json
{
  "dashboard": {
    "title": "Index Freshness",
    "panels": [
      {
        "title": "Repositories by Freshness",
        "targets": [{
          "expr": "sum(aethyme_repositories_total) by (freshness)"
        }]
      },
      {
        "title": "Average Index Age",
        "targets": [{
          "expr": "avg(aethyme_index_age_seconds)"
        }]
      },
      {
        "title": "Stale Repositories (> 24h)",
        "targets": [{
          "expr": "count(aethyme_index_age_seconds > 86400)"
        }]
      }
    ]
  }
}
```

### 2. Configure Alerts

```yaml
# prometheus/alerts/freshness.yml
groups:
  - name: freshness
    rules:
      - alert: HighStalenessRate
        expr: |
          sum(aethyme_repositories_total{freshness="stale"})
          / sum(aethyme_repositories_total) > 0.05
        for: 30m
        labels:
          severity: warning
        annotations:
          summary: "More than 5% of repositories are stale"
          description: "{{ $value | humanizePercentage }} of repositories have stale indexes"

      - alert: CriticalRepositoryStale
        expr: |
          aethyme_index_age_seconds{priority="critical"} > 3600
        for: 10m
        labels:
          severity: critical
        annotations:
          summary: "Critical repository index is stale"
          description: "{{ $labels.repository }} has not been indexed in {{ $value | humanizeDuration }}"

      - alert: WatchServiceDown
        expr: up{job="aethyme-watch"} == 0
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Watch service is down"
```

### 3. Implement Staleness Detector Script

```python
#!/usr/bin/env python3
# scripts/detect-stale-indexes.py

import argparse
import sys
from datetime import datetime, timedelta
from typing import List, Dict
import psycopg2
from tabulate import tabulate

def detect_stale_repositories(
    threshold_seconds: int = 86400,
    db_url: str = "postgresql://aethyme:password@localhost:5432/aethyme"
) -> List[Dict]:
    """Detect repositories with stale indexes."""
    conn = psycopg2.connect(db_url)
    cur = conn.cursor()

    query = """
        SELECT
            id,
            name,
            path,
            indexed_at,
            EXTRACT(EPOCH FROM (NOW() - indexed_at))::int AS age_seconds,
            status
        FROM repositories
        WHERE
            indexed_at < NOW() - INTERVAL '%s seconds'
            AND status = 'completed'
        ORDER BY indexed_at ASC;
    """

    cur.execute(query, (threshold_seconds,))
    rows = cur.fetchall()

    results = []
    for row in rows:
        repo_id, name, path, indexed_at, age_seconds, status = row
        results.append({
            "id": repo_id,
            "name": name,
            "path": path,
            "indexed_at": indexed_at,
            "age_seconds": age_seconds,
            "age_human": str(timedelta(seconds=age_seconds)),
            "status": status
        })

    cur.close()
    conn.close()

    return results

def main():
    parser = argparse.ArgumentParser(description="Detect stale repository indexes")
    parser.add_argument("--threshold", type=int, default=86400,
                       help="Staleness threshold in seconds (default: 86400 = 24h)")
    parser.add_argument("--db-url", default="postgresql://aethyme:password@localhost:5432/aethyme",
                       help="Database connection URL")
    parser.add_argument("--format", choices=["table", "json"], default="table",
                       help="Output format")

    args = parser.parse_args()

    stale_repos = detect_stale_repositories(args.threshold, args.db_url)

    if not stale_repos:
        print(f"✓ No stale repositories found (threshold: {args.threshold}s)")
        sys.exit(0)

    if args.format == "json":
        import json
        print(json.dumps(stale_repos, indent=2, default=str))
    else:
        print(f"\n⚠ Stale Repositories ({len(stale_repos)}):\n")
        table_data = [
            [r["name"], r["age_human"], r["indexed_at"].strftime("%Y-%m-%d %H:%M:%S")]
            for r in stale_repos
        ]
        print(tabulate(table_data, headers=["Repository", "Age", "Last Indexed"], tablefmt="grid"))

    # Exit with error if stale repos found
    sys.exit(1)

if __name__ == "__main__":
    main()
```

---

## Verification

After remediation, verify freshness is restored:

### 1. Check Repository Freshness

```bash
# Via API
curl http://localhost:8001/api/index/status/{repository_id} \
  -H "Authorization: Bearer $TOKEN" | jq '{freshness, indexed_at}'

# Expected:
# {
#   "freshness": "current",
#   "indexed_at": "2025-11-22T15:30:00Z"
# }
```

### 2. Run Test Query

```bash
# Search for recently added code
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"query": "NewlyAddedClass"}' | jq '.results | length'

# Expected: > 0 results
```

### 3. Check Metrics

```bash
# Verify freshness metrics improved
curl 'http://localhost:9090/api/v1/query?query=aethyme_index_freshness_ratio' | jq .

# Expected: > 0.95 (95% fresh)
```

### 4. Verify Watch Service

```bash
# Make a change to repository
echo "# Test change" >> /path/to/repo/README.md
git -C /path/to/repo add README.md
git -C /path/to/repo commit -m "Test staleness detection"

# Wait for watch service interval (default: 5 minutes)
sleep 300

# Check if re-indexing was triggered
docker logs aethyme-watch --tail=50 | grep "Re-index triggered"
```

---

## Prevention Measures

### 1. Default Watch Service Enabled

```bash
# In .env (production default)
WATCH_ENABLED=true
WATCH_BATCH_INTERVAL=300  # 5 minutes
```

### 2. Git Hooks in Repository Templates

```bash
# Include git hooks in repository scaffolding
# .aethyme/templates/post-receive
#!/bin/bash
# Auto-generated by Aethyme
# Triggers re-indexing on git push

# ... hook content ...
```

### 3. Scheduled Health Checks

```bash
# Daily health check that triggers alerts
#!/bin/bash
# scripts/health-check-freshness.sh

STALE_COUNT=$(python scripts/detect-stale-indexes.py --format json | jq 'length')

if [ "$STALE_COUNT" -gt 5 ]; then
  echo "WARNING: $STALE_COUNT stale repositories detected"
  # Send alert to Slack/PagerDuty
  curl -X POST https://hooks.slack.com/services/YOUR/WEBHOOK/URL \
    -d "{\"text\": \"⚠ $STALE_COUNT stale repositories in Aethyme\"}"
  exit 1
fi

echo "✓ Freshness check passed"
```

### 4. Repository Priority Levels

```sql
-- Tag critical repositories for higher priority monitoring
UPDATE repositories
SET metadata = jsonb_set(
  COALESCE(metadata, '{}'::jsonb),
  '{priority}',
  '"critical"'
)
WHERE name IN ('core-api', 'auth-service');

-- Adjust alert thresholds based on priority
```

---

## Related Runbooks

- [Indexing Failure](index-failure.md) - If re-indexing fails
- [Performance Degradation](performance-degradation.md) - If re-indexing is slow
- [Backup & Restore](backup-restore.md) - For data recovery

---

## Change Log

| Date | Version | Changes | Author |
|------|---------|---------|--------|
| 2025-11-22 | 1.0 | Initial runbook | Aethyme Team |

---

**Next Review Date:** 2026-02-22
**Runbook Owner:** Platform Engineering Team
**Approval Status:** APPROVED
