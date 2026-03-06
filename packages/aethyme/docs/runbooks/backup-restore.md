# Runbook: Backup and Restore

**Audience:** DevOps, Database Administrators, SRE
**Severity:** CRITICAL
**Last Updated:** 2025-11-22
**Version:** 1.0

---

## Overview

Procedures for backing up and restoring Aethyme data, including PostgreSQL database, Redis cache, and configuration. Essential for disaster recovery and data migration.

**RPO (Recovery Point Objective):** < 6 hours
**RTO (Recovery Time Objective):** < 30 minutes

---

## Symptoms

- PostgreSQL data loss or corruption
- Failed migration that requires rollback to a known-good snapshot
- Region outage requiring restore into a fresh environment
- Need to recover repositories, graph data, or user records after operator error

## Diagnostic

- Confirm the timestamp and integrity of the latest successful backup
- Identify whether recovery is logical restore, physical restore, or point-in-time recovery
- Verify target environment credentials, storage access, and available disk space
- Confirm the application is drained or stopped before restore begins

---

## Backup Strategy

### Backup Components

1. **PostgreSQL Database** (Critical)
   - Repositories, nodes, edges, users, tenants
   - Frequency: Every 6 hours + pre-migration
   - Retention: 30 days

2. **Redis Cache** (Optional)
   - Query cache, session data
   - Frequency: Daily snapshots
   - Retention: 7 days

3. **Configuration** (Important)
   - .env files, secrets, K8s manifests
   - Frequency: On change
   - Retention: Version controlled

### Backup Schedule

| Type | Frequency | Retention | Storage |
|------|-----------|-----------|---------|
| Full DB | Every 6h | 30 days | S3/GCS |
| Incremental | Hourly (WAL) | 7 days | S3/GCS |
| Redis Snapshot | Daily | 7 days | Local + S3 |
| Config | On change | Git history | GitHub |

---

## PostgreSQL Backup

### Method 1: pg_dump (Logical Backup)

**Advantages:** Portable, works across PostgreSQL versions
**Disadvantages:** Slower for large databases

```bash
#!/bin/bash
# scripts/backup-database.sh

BACKUP_DIR="/backups/aethyme"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
BACKUP_FILE="$BACKUP_DIR/aethyme-$TIMESTAMP.sql.gz"

# Create backup directory
mkdir -p $BACKUP_DIR

# Dump database with compression
pg_dump -U aethyme \
  -h localhost \
  -d aethyme \
  --verbose \
  --no-owner \
  --no-privileges \
  | gzip > $BACKUP_FILE

# Verify backup created
if [ -f "$BACKUP_FILE" ]; then
  SIZE=$(du -h $BACKUP_FILE | cut -f1)
  echo "Backup created: $BACKUP_FILE ($SIZE)"
else
  echo "ERROR: Backup failed"
  exit 1
fi

# Upload to S3
aws s3 cp $BACKUP_FILE s3://aethyme-backups/postgres/

# Clean old backups (keep last 30 days)
find $BACKUP_DIR -name "aethyme-*.sql.gz" -mtime +30 -delete

echo "Backup complete: $BACKUP_FILE"
```

**Run backup:**
```bash
bash scripts/backup-database.sh
```

### Method 2: pg_basebackup (Physical Backup)

**Advantages:** Faster, includes WAL for PITR
**Disadvantages:** Same PostgreSQL version required

```bash
#!/bin/bash
# scripts/backup-database-physical.sh

BACKUP_DIR="/backups/aethyme-base"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

pg_basebackup -U aethyme \
  -h localhost \
  -D $BACKUP_DIR/$TIMESTAMP \
  -F tar \
  -z \
  -P \
  -X stream

echo "Base backup complete: $BACKUP_DIR/$TIMESTAMP"
```

### Method 3: Automated Backups (Kubernetes CronJob)

```yaml
# k8s/cronjob-backup.yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: postgres-backup
  namespace: aethyme
spec:
  schedule: "0 */6 * * *"  # Every 6 hours
  successfulJobsHistoryLimit: 3
  failedJobsHistoryLimit: 1
  jobTemplate:
    spec:
      template:
        metadata:
          labels:
            app: postgres-backup
        spec:
          restartPolicy: OnFailure
          containers:
          - name: backup
            image: postgres:15
            env:
            - name: PGPASSWORD
              valueFrom:
                secretKeyRef:
                  name: aethyme-secrets
                  key: postgres-password
            command:
            - /bin/bash
            - -c
            - |
              TIMESTAMP=$(date +%Y%m%d-%H%M%S)
              BACKUP_FILE="/backups/aethyme-$TIMESTAMP.sql.gz"

              pg_dump -U aethyme -h postgres -d aethyme \
                | gzip > $BACKUP_FILE

              # Upload to cloud storage
              aws s3 cp $BACKUP_FILE s3://aethyme-backups/postgres/

              echo "Backup complete: $BACKUP_FILE"
            volumeMounts:
            - name: backup-storage
              mountPath: /backups
          volumes:
          - name: backup-storage
            persistentVolumeClaim:
              claimName: backup-pvc
```

Apply:
```bash
kubectl apply -f k8s/cronjob-backup.yaml
```

---

## PostgreSQL Restore

### Restore Procedure

```bash
#!/bin/bash
# scripts/restore-database.sh

BACKUP_FILE=$1

if [ -z "$BACKUP_FILE" ]; then
  echo "Usage: $0 <backup-file.sql.gz>"
  exit 1
fi

echo "WARNING: This will overwrite the existing database!"
read -p "Are you sure? (yes/no): " CONFIRM

if [ "$CONFIRM" != "yes" ]; then
  echo "Restore cancelled"
  exit 0
fi

# 1. Stop application (prevent writes during restore)
echo "Stopping application..."
docker-compose -f ops/docker-compose.yml stop api
# OR for Kubernetes
kubectl scale deployment aethyme-api --replicas=0 -n aethyme

# 2. Drop existing database
echo "Dropping existing database..."
psql -U postgres -h localhost <<EOF
DROP DATABASE IF EXISTS aethyme;
CREATE DATABASE aethyme;
GRANT ALL PRIVILEGES ON DATABASE aethyme TO aethyme;
EOF

# 3. Restore from backup
echo "Restoring from: $BACKUP_FILE"
gunzip < $BACKUP_FILE | psql -U aethyme -h localhost -d aethyme

# 4. Verify restoration
echo "Verifying restoration..."
ROW_COUNT=$(psql -U aethyme -h localhost -d aethyme -tAc "SELECT COUNT(*) FROM nodes;")
echo "Nodes restored: $ROW_COUNT"

# 5. Restart application
echo "Restarting application..."
docker-compose -f ops/docker-compose.yml start api
# OR for Kubernetes
kubectl scale deployment aethyme-api --replicas=3 -n aethyme

# 6. Run health check
sleep 10
curl http://localhost:8001/health/detailed

echo "Restore complete!"
```

**Usage:**
```bash
bash scripts/restore-database.sh /backups/aethyme-20251122-140000.sql.gz
```

### Point-in-Time Recovery (PITR)

For WAL-based recovery:

```bash
#!/bin/bash
# scripts/restore-pitr.sh

TARGET_TIME=$1  # Format: 2025-11-22 14:30:00

# 1. Stop PostgreSQL
systemctl stop postgresql

# 2. Move current data directory
mv /var/lib/postgresql/15/main /var/lib/postgresql/15/main.old

# 3. Restore base backup
tar -xzf /backups/base/20251122-120000.tar.gz -C /var/lib/postgresql/15/main

# 4. Create recovery configuration
cat > /var/lib/postgresql/15/main/recovery.conf <<EOF
restore_command = 'cp /backups/wal/%f %p'
recovery_target_time = '$TARGET_TIME'
recovery_target_action = 'promote'
EOF

# 5. Start PostgreSQL (will replay WAL)
systemctl start postgresql

# 6. Verify recovery
psql -U aethyme -d aethyme -c "SELECT NOW();"
```

---

## Redis Backup

### Save Snapshot

```bash
#!/bin/bash
# scripts/backup-redis.sh

BACKUP_DIR="/backups/redis"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

mkdir -p $BACKUP_DIR

# Trigger Redis save
redis-cli BGSAVE

# Wait for save to complete
while [ "$(redis-cli LASTSAVE)" -eq "$(redis-cli LASTSAVE)" ]; do
  sleep 1
done

# Copy dump file
cp /var/lib/redis/dump.rdb $BACKUP_DIR/dump-$TIMESTAMP.rdb

# Compress
gzip $BACKUP_DIR/dump-$TIMESTAMP.rdb

echo "Redis backup complete: $BACKUP_DIR/dump-$TIMESTAMP.rdb.gz"
```

### Restore Redis

```bash
#!/bin/bash
# scripts/restore-redis.sh

BACKUP_FILE=$1

# 1. Stop Redis
systemctl stop redis

# 2. Restore dump file
gunzip < $BACKUP_FILE > /var/lib/redis/dump.rdb
chown redis:redis /var/lib/redis/dump.rdb

# 3. Start Redis (will load from dump.rdb)
systemctl start redis

# 4. Verify
redis-cli DBSIZE

echo "Redis restore complete"
```

---

## Configuration Backup

### Backup Secrets and Config

```bash
#!/bin/bash
# scripts/backup-config.sh

BACKUP_DIR="/backups/config"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

mkdir -p $BACKUP_DIR

# 1. Backup .env file (ENCRYPTED)
gpg --encrypt --recipient ops@aeptus.com \
  .env > $BACKUP_DIR/env-$TIMESTAMP.gpg

# 2. Backup Kubernetes secrets
kubectl get secret aethyme-secrets -n aethyme -o yaml \
  > $BACKUP_DIR/k8s-secrets-$TIMESTAMP.yaml

# Encrypt secrets file
gpg --encrypt --recipient ops@aeptus.com \
  $BACKUP_DIR/k8s-secrets-$TIMESTAMP.yaml

rm $BACKUP_DIR/k8s-secrets-$TIMESTAMP.yaml

# 3. Backup manifests
tar -czf $BACKUP_DIR/manifests-$TIMESTAMP.tar.gz k8s/ ops/

echo "Config backup complete: $BACKUP_DIR"
```

### Restore Configuration

```bash
#!/bin/bash
# scripts/restore-config.sh

BACKUP_DIR=$1

# 1. Decrypt and restore .env
gpg --decrypt $BACKUP_DIR/env-*.gpg > .env

# 2. Restore Kubernetes secrets
gpg --decrypt $BACKUP_DIR/k8s-secrets-*.yaml.gpg \
  | kubectl apply -f -

# 3. Restore manifests
tar -xzf $BACKUP_DIR/manifests-*.tar.gz

echo "Config restore complete"
```

---

## Disaster Recovery Procedure

### Full System Recovery

```bash
#!/bin/bash
# scripts/disaster-recovery.sh

echo "=== Aethyme Disaster Recovery ==="

# 1. Restore PostgreSQL
echo "Step 1/5: Restoring PostgreSQL..."
bash scripts/restore-database.sh /backups/aethyme-latest.sql.gz

# 2. Restore Redis (optional)
echo "Step 2/5: Restoring Redis cache..."
bash scripts/restore-redis.sh /backups/redis/dump-latest.rdb.gz

# 3. Restore configuration
echo "Step 3/5: Restoring configuration..."
bash scripts/restore-config.sh /backups/config/latest/

# 4. Deploy application
echo "Step 4/5: Deploying application..."
kubectl apply -f k8s/
kubectl rollout status deployment/aethyme-api -n aethyme

# 5. Verify system health
echo "Step 5/5: Verifying system health..."
bash scripts/smoke-tests.sh

if [ $? -eq 0 ]; then
  echo "✓ Disaster recovery complete!"
else
  echo "✗ Recovery verification failed!"
  exit 1
fi
```

---

## Backup Verification

### Monthly Restore Test

```bash
#!/bin/bash
# scripts/test-restore.sh

echo "=== Monthly Backup Restoration Test ==="

# 1. Create test namespace
kubectl create namespace aethyme-restore-test

# 2. Deploy PostgreSQL in test namespace
kubectl apply -f k8s/postgres.yaml -n aethyme-restore-test

# 3. Restore latest backup to test database
LATEST_BACKUP=$(ls -t /backups/aethyme-*.sql.gz | head -1)
gunzip < $LATEST_BACKUP | kubectl exec -i postgres-0 -n aethyme-restore-test -- \
  psql -U aethyme -d aethyme

# 4. Verify data integrity
kubectl exec -it postgres-0 -n aethyme-restore-test -- \
  psql -U aethyme -d aethyme -c "SELECT COUNT(*) FROM nodes;"

# 5. Cleanup
kubectl delete namespace aethyme-restore-test

echo "Restore test complete"
```

**Schedule:** Run on 1st of every month

---

## Backup Retention Policy

### Retention Rules

```bash
#!/bin/bash
# scripts/cleanup-old-backups.sh

BACKUP_DIR="/backups/aethyme"
AWS_BUCKET="s3://aethyme-backups"

# Keep daily backups for 7 days
find $BACKUP_DIR -name "aethyme-*.sql.gz" -mtime +7 -delete

# Keep weekly backups for 30 days
# (Backups from Sunday)
find $BACKUP_DIR -name "aethyme-*-00*.sql.gz" -mtime +30 -delete

# Keep monthly backups for 1 year
# (Backups from 1st of month)
aws s3 ls $AWS_BUCKET/postgres/ | grep "aethyme-....01-" \
  | awk '{print $4}' | while read file; do
  AGE=$(( ($(date +%s) - $(date -d $(echo $file | cut -d- -f2) +%s)) / 86400 ))
  if [ $AGE -gt 365 ]; then
    aws s3 rm $AWS_BUCKET/postgres/$file
  fi
done

echo "Old backups cleaned"
```

---

## Monitoring Backups

### Backup Health Checks

```bash
#!/bin/bash
# scripts/check-backup-health.sh

# 1. Check latest backup age
LATEST=$(ls -t /backups/aethyme-*.sql.gz | head -1)
AGE=$(( ($(date +%s) - $(stat -c %Y $LATEST)) / 3600 ))

if [ $AGE -gt 12 ]; then
  echo "WARNING: Latest backup is $AGE hours old!"
  # Send alert
  exit 1
fi

# 2. Check backup size
SIZE=$(du -m $LATEST | cut -f1)
if [ $SIZE -lt 10 ]; then
  echo "WARNING: Backup size too small: ${SIZE}MB"
  exit 1
fi

# 3. Test backup file integrity
gunzip -t $LATEST
if [ $? -ne 0 ]; then
  echo "ERROR: Backup file corrupted!"
  exit 1
fi

echo "✓ Backup health check passed"
```

### Prometheus Metrics

```yaml
# prometheus/backup-exporter.yml
- name: backup_age_seconds
  help: Age of latest backup in seconds
  type: gauge

- name: backup_size_bytes
  help: Size of latest backup
  type: gauge

- name: backup_success_total
  help: Total successful backups
  type: counter

- name: backup_failures_total
  help: Total failed backups
  type: counter
```

---

## Related Runbooks

- [Rollback Procedures](rollback.md) - Application rollbacks
- [Indexing Failure](index-failure.md) - Restore if indexing corrupted data
- [Security Incident](security-incident.md) - Restore after security breach

---

## Change Log

| Date | Version | Changes | Author |
|------|---------|---------|--------|
| 2025-11-22 | 1.0 | Initial runbook | Aethyme Team |

---

**Next Review Date:** 2026-02-22
**Runbook Owner:** Database Administration Team
**Approval Status:** APPROVED
