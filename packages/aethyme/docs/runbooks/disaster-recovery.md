# Disaster Recovery Runbook

## Overview

This runbook provides procedures for recovering Aethyme from various disaster scenarios.

**Recovery Objectives:**
- **RTO (Recovery Time Objective):** 1 hour
- **RPO (Recovery Point Objective):** 1 hour (based on backup frequency)

## Symptoms

- Primary database or region unavailable
- Persistent 5xx responses across the platform
- Storage corruption or missing graph data
- Control plane unavailable after infrastructure loss

## Diagnostic

- Identify the failed subsystem: database, region, storage, or complete platform loss
- Confirm backup freshness and replica availability
- Verify networking, DNS, and infrastructure-control access before recovery actions

## Table of Contents

1. [Database Failure](#database-failure)
2. [Region Failure](#region-failure)
3. [Data Corruption](#data-corruption)
4. [Complete Infrastructure Loss](#complete-infrastructure-loss)
5. [DR Drill Checklist](#dr-drill-checklist)

---

## Database Failure

### Scenario
PostgreSQL database is down or corrupted.

### Detection
- Alert: `PostgreSQLDown`
- Symptoms: 503 errors from API, readiness probe failures

### Recovery Steps

#### 1. Assess Damage
```bash
# Check PostgreSQL pod status
kubectl get pods -n production -l app.kubernetes.io/component=database

# Check logs
kubectl logs -n production aethyme-postgres-0 --tail=100

# Attempt connection
kubectl exec -n production aethyme-postgres-0 -- psql -U postgres -c "SELECT 1"
```

#### 2. Promote Read Replica (If Available)
```bash
# Identify healthy replica
kubectl get pods -n production -l app.kubernetes.io/component=database

# Promote replica to primary
kubectl exec -n production aethyme-postgres-1 -- \
  /usr/local/bin/pg_ctl promote -D /var/lib/postgresql/data

# Update service to point to new primary
kubectl patch service aethyme-postgres -n production \
  -p '{"spec":{"selector":{"statefulset.kubernetes.io/pod-name":"aethyme-postgres-1"}}}'
```

**Time Required:** 5-10 minutes

#### 3. Restore from Backup (If No Replica)
```bash
# List available backups
aws s3 ls s3://aethyme-backups/postgres/ | tail -20

# Download latest backup
BACKUP_FILE=postgres_aethyme_YYYYMMDD_HHMMSS.sql.gz
aws s3 cp s3://aethyme-backups/postgres/$BACKUP_FILE /tmp/

# Run restore script
cd /path/to/aethyme
BACKUP_FILE=$BACKUP_FILE \
NAMESPACE=production \
./scripts/backup/restore_postgres.sh
```

**Time Required:** 20-40 minutes (depending on database size)

#### 4. Verify Recovery
```bash
# Check database connectivity
kubectl exec -n production aethyme-postgres-0 -- \
  psql -U postgres -d aethyme -c "SELECT COUNT(*) FROM repositories;"

# Restart application pods
kubectl rollout restart deployment/aethyme -n production

# Monitor health
kubectl get pods -n production -w
```

#### 5. Post-Recovery
- [ ] Verify all services are healthy
- [ ] Check application logs for errors
- [ ] Run smoke tests
- [ ] Document incident in post-mortem
- [ ] Schedule backup verification

---

## Region Failure

### Scenario
Entire cloud region is unavailable.

### Detection
- Multiple service alerts
- Unable to access Kubernetes API
- Cloud provider status page confirms outage

### Recovery Steps

#### 1. Activate DR Region
```bash
# Switch to DR region context
kubectl config use-context aethyme-dr

# Verify cluster is healthy
kubectl get nodes
kubectl get pods -n production
```

#### 2. Restore Latest Data
```bash
# Backups are in multi-region bucket
# Restore PostgreSQL
BACKUP_FILE=$(aws s3 ls s3://aethyme-backups/postgres/ | tail -1 | awk '{print $4}')
NAMESPACE=production \
BACKUP_FILE=$BACKUP_FILE \
./scripts/backup/restore_postgres.sh

# Restore Redis
BACKUP_FILE=$(aws s3 ls s3://aethyme-backups/redis/ | tail -1 | awk '{print $4}')
# Redis restore procedure...
```

#### 3. Update DNS
```bash
# Update DNS to point to DR region
# Example using Route53
aws route53 change-resource-record-sets \
  --hosted-zone-id Z1234567890ABC \
  --change-batch file://dns-failover.json
```

**DNS TTL:** Ensure TTL is set low (60s) for faster failover

#### 4. Verify Services
```bash
# Test health endpoints
curl https://api.aethyme.com/health/detailed

# Monitor metrics
kubectl port-forward -n production svc/prometheus 9090:9090
# Visit http://localhost:9090
```

**Time Required:** 30-60 minutes

#### 5. Failback Procedure (When Primary Region Recovers)
```bash
# Sync data from DR to primary
# Create backup of DR region
NAMESPACE=production ./scripts/backup/backup_postgres.sh

# Restore to primary region
kubectl config use-context aethyme-primary
# ... restore procedure ...

# Switch DNS back to primary
aws route53 change-resource-record-sets ...
```

---

## Data Corruption

### Scenario
Data corruption detected (bad migration, application bug, etc.)

### Detection
- Data validation alerts
- User reports
- Anomalous query results

### Recovery Steps

#### 1. Identify Corruption Scope
```bash
# Check when corruption started
# Review recent deployments
kubectl rollout history deployment/aethyme -n production

# Check recent database migrations
kubectl exec -n production aethyme-postgres-0 -- \
  psql -U postgres -d aethyme -c "SELECT * FROM alembic_version;"
```

#### 2. Stop Writes
```bash
# Scale down API and workers to prevent further corruption
kubectl scale deployment aethyme -n production --replicas=0
kubectl scale deployment aethyme-indexer -n production --replicas=0
```

#### 3. Point-in-Time Recovery
```bash
# Identify last good backup before corruption
aws s3 ls s3://aethyme-backups/postgres/ | grep "YYYYMMDD"

# Restore from that backup
BACKUP_FILE=postgres_aethyme_YYYYMMDD_HHMMSS.sql.gz \
NAMESPACE=production \
./scripts/backup/restore_postgres.sh
```

#### 4. Verify Data Integrity
```bash
# Run data validation queries
kubectl exec -n production aethyme-postgres-0 -- \
  psql -U postgres -d aethyme -f /path/to/validation.sql

# Check record counts
# Compare against known good state
```

#### 5. Resume Operations
```bash
# Scale up services
kubectl scale deployment aethyme -n production --replicas=5
kubectl scale deployment aethyme-indexer -n production --replicas=3

# Monitor for issues
kubectl logs -n production -l app.kubernetes.io/name=aethyme --tail=100 -f
```

**Time Required:** 30-60 minutes

---

## Complete Infrastructure Loss

### Scenario
Complete loss of infrastructure (e.g., account compromise, catastrophic failure)

### Prerequisites
- Infrastructure as Code (IaC) in git repository
- Backups in separate account/region
- Runbooks and recovery procedures accessible offline

### Recovery Steps

#### 1. Provision New Infrastructure
```bash
# Clone infrastructure repository
git clone https://github.com/your-org/aethyme-infra.git
cd aethyme-infra

# Provision Kubernetes cluster (example using Terraform)
terraform init
terraform plan -out=tfplan
terraform apply tfplan
```

**Time Required:** 20-40 minutes

#### 2. Deploy Kubernetes Operators
```bash
# Install required operators
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.13.0/cert-manager.yaml
kubectl apply -f https://github.com/external-secrets/external-secrets/releases/download/v0.9.0/external-secrets.yaml
kubectl apply -f https://github.com/prometheus-operator/prometheus-operator/releases/download/v0.70.0/bundle.yaml

# Wait for operators to be ready
kubectl wait --for=condition=available --timeout=300s \
  deployment/cert-manager -n cert-manager
```

#### 3. Restore Secrets
```bash
# Recreate external secrets configuration
kubectl apply -f k8s/secrets/

# Verify secrets are populated
kubectl get secrets -n production
```

#### 4. Deploy Application
```bash
# Deploy using Helm
cd k8s/helm/aethyme
helm install aethyme . \
  --namespace production \
  --create-namespace \
  --values values-production.yaml \
  --wait
```

#### 5. Restore Data
```bash
# Restore PostgreSQL
NAMESPACE=production \
BACKUP_FILE=$(latest backup from S3) \
./scripts/backup/restore_postgres.sh

# Restore Redis
NAMESPACE=production \
./scripts/backup/restore_redis.sh
```

#### 6. Verify and Resume
```bash
# Run smoke tests
./scripts/test/smoke-tests.sh

# Update DNS
# ... DNS update procedure ...

# Monitor all systems
kubectl get pods -n production
kubectl top nodes
```

**Total Time Required:** 2-3 hours

---

## DR Drill Checklist

### Annual DR Drill

Perform annually to validate recovery procedures and train team.

#### Pre-Drill
- [ ] Schedule drill date (announce to team)
- [ ] Notify stakeholders
- [ ] Ensure all runbooks are up-to-date
- [ ] Verify backup locations and access
- [ ] Prepare DR environment

#### During Drill
- [ ] Simulate failure scenario
- [ ] Follow recovery runbook exactly
- [ ] Document all issues and deviations
- [ ] Time each recovery step
- [ ] Test communication channels

#### Post-Drill
- [ ] Calculate actual RTO/RPO
- [ ] Document lessons learned
- [ ] Update runbooks with improvements
- [ ] Schedule fixes for identified issues
- [ ] Share results with stakeholders

### Quarterly Backup Verification

- [ ] Restore latest backup to test environment
- [ ] Verify data integrity
- [ ] Test application functionality
- [ ] Document results
- [ ] Fix any issues found

---

## Emergency Contacts

| Role | Contact | Escalation |
|------|---------|------------|
| On-Call Engineer | PagerDuty | Immediate |
| Database Admin | [email] | Within 15 min |
| Platform Lead | [email] | Within 30 min |
| Cloud Provider Support | [support number] | As needed |

---

## References

- Backup scripts: `scripts/backup/`
- Monitoring dashboards: https://grafana.aethyme.com
- Alert definitions: `monitoring/alerts/`
- Infrastructure code: https://github.com/your-org/aethyme-infra

---

**Last Updated:** 2024-11-22
**Next Review:** 2025-02-22
**Owner:** Platform Team
