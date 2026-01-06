# RepoGraph Deployment Architecture

**Version:** 1.0
**Date:** 2025-11-22
**Stage:** Stage 1 (CLI/Service Backend)

---

## Overview

RepoGraph deployment architecture for **Kubernetes** with auto-scaling, high availability, and observability.

---

## Stage 1 Deployment Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                     Cloud Load Balancer                      │
│                  (HTTPS/TLS Termination)                     │
└──────────────────────────┬──────────────────────────────────┘
                           │
    ┌──────────────────────┼──────────────────────┐
    │                      │                      │
┌───▼────┐           ┌────▼───┐           ┌─────▼──┐
│ API-1  │           │ API-2  │           │ API-3  │  (HPA: 3-20 pods)
└───┬────┘           └───┬────┘           └───┬────┘
    │                     │                     │
    └──────────┬──────────┴──────────┬──────────┘
               │                     │
    ┌──────────▼──────────┐   ┌─────▼─────────┐
    │  PostgreSQL (RDS)   │   │ Redis Cluster │
    │  Primary + Replicas │   │   (Sentinel)  │
    └─────────────────────┘   └───────────────┘
               │
    ┌──────────▼──────────┐
    │   Celery Workers    │  (HPA: 2-10 pods)
    └─────────────────────┘
               │
    ┌──────────▼──────────┐
    │  Prometheus Stack   │
    │  (Metrics, Alerts)  │
    └─────────────────────┘
```

---

## Kubernetes Manifests

### 1. Namespace

```yaml
# k8s/namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: repograph
  labels:
    name: repograph
    environment: production
```

### 2. API Deployment

```yaml
# k8s/api-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: repograph-api
  namespace: repograph
spec:
  replicas: 3
  strategy:
    type: RollingUpdate
    rollingUpdate:
      maxSurge: 1
      maxUnavailable: 0
  selector:
    matchLabels:
      app: repograph-api
  template:
    metadata:
      labels:
        app: repograph-api
        version: v1.0.0
    spec:
      containers:
      - name: api
        image: repograph/api:1.0.0
        ports:
        - containerPort: 8000
          name: http
        - containerPort: 9090
          name: metrics
        env:
        - name: DATABASE_URL
          valueFrom:
            secretKeyRef:
              name: repograph-secrets
              key: database-url
        - name: REDIS_URL
          valueFrom:
            secretKeyRef:
              name: repograph-secrets
              key: redis-url
        - name: JWT_SECRET_KEY
          valueFrom:
            secretKeyRef:
              name: repograph-secrets
              key: jwt-secret
        resources:
          requests:
            cpu: 500m
            memory: 1Gi
          limits:
            cpu: 2000m
            memory: 4Gi
        livenessProbe:
          httpGet:
            path: /health
            port: 8000
          initialDelaySeconds: 30
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /health/ready
            port: 8000
          initialDelaySeconds: 10
          periodSeconds: 5
```

### 3. Worker Deployment

```yaml
# k8s/worker-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: repograph-worker
  namespace: repograph
spec:
  replicas: 2
  selector:
    matchLabels:
      app: repograph-worker
  template:
    metadata:
      labels:
        app: repograph-worker
    spec:
      containers:
      - name: worker
        image: repograph/worker:1.0.0
        command: ["celery", "-A", "repograph.worker", "worker", "-l", "info"]
        env:
        - name: DATABASE_URL
          valueFrom:
            secretKeyRef:
              name: repograph-secrets
              key: database-url
        - name: REDIS_URL
          valueFrom:
            secretKeyRef:
              name: repograph-secrets
              key: redis-url
        resources:
          requests:
            cpu: 1000m
            memory: 2Gi
          limits:
            cpu: 4000m
            memory: 8Gi
```

### 4. Horizontal Pod Autoscaler (HPA)

```yaml
# k8s/api-hpa.yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: repograph-api-hpa
  namespace: repograph
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: repograph-api
  minReplicas: 3
  maxReplicas: 20
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
  - type: Resource
    resource:
      name: memory
      target:
        type: Utilization
        averageUtilization: 80
  behavior:
    scaleUp:
      stabilizationWindowSeconds: 60
      policies:
      - type: Percent
        value: 100
        periodSeconds: 60
    scaleDown:
      stabilizationWindowSeconds: 300
      policies:
      - type: Percent
        value: 50
        periodSeconds: 60
```

### 5. Service

```yaml
# k8s/api-service.yaml
apiVersion: v1
kind: Service
metadata:
  name: repograph-api
  namespace: repograph
spec:
  type: LoadBalancer
  ports:
  - port: 80
    targetPort: 8000
    name: http
  selector:
    app: repograph-api
```

### 6. Ingress (HTTPS)

```yaml
# k8s/ingress.yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: repograph-ingress
  namespace: repograph
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
    nginx.ingress.kubernetes.io/rate-limit: "100"
spec:
  tls:
  - hosts:
    - repograph.aeptus.com
    secretName: repograph-tls
  rules:
  - host: repograph.aeptus.com
    http:
      paths:
      - path: /
        pathType: Prefix
        backend:
          service:
            name: repograph-api
            port:
              number: 80
```

---

## Database (PostgreSQL on RDS/Cloud SQL)

**Managed Service Configuration:**

```yaml
# Cloud provider: AWS RDS, GCP Cloud SQL, or Azure Database
Instance Type: db.r6g.xlarge  (4 vCPU, 32 GB RAM)
Storage: 500 GB SSD (Auto-scaling enabled)
Multi-AZ: Enabled (Primary + Standby)
Read Replicas: 2 (for read scaling)
Backup: Automated daily backups (30-day retention)
Encryption: At-rest + in-transit (TLS 1.2+)
```

**Connection Pooling (PgBouncer):**

```yaml
# k8s/pgbouncer-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: pgbouncer
  namespace: repograph
spec:
  replicas: 2
  selector:
    matchLabels:
      app: pgbouncer
  template:
    metadata:
      labels:
        app: pgbouncer
    spec:
      containers:
      - name: pgbouncer
        image: edoburu/pgbouncer:latest
        ports:
        - containerPort: 5432
        env:
        - name: DB_HOST
          value: "postgres-primary.rds.amazonaws.com"
        - name: DB_PORT
          value: "5432"
        - name: POOL_MODE
          value: "transaction"
        - name: MAX_CLIENT_CONN
          value: "1000"
        - name: DEFAULT_POOL_SIZE
          value: "25"
        resources:
          requests:
            cpu: 200m
            memory: 256Mi
```

---

## Redis (Managed or Self-Hosted)

**Option A: Managed Redis (AWS ElastiCache, GCP Memorystore)**

```yaml
Instance: cache.r6g.large (2 vCPU, 13 GB RAM)
Multi-AZ: Enabled
Cluster Mode: Disabled (Sentinel mode)
Replicas: 2
Backup: Daily snapshots
```

**Option B: Self-Hosted Redis (Kubernetes)**

```yaml
# k8s/redis-deployment.yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: redis
  namespace: repograph
spec:
  serviceName: redis
  replicas: 3  # 1 master + 2 replicas
  selector:
    matchLabels:
      app: redis
  template:
    metadata:
      labels:
        app: redis
    spec:
      containers:
      - name: redis
        image: redis:8.0-alpine
        ports:
        - containerPort: 6379
        volumeMounts:
        - name: redis-data
          mountPath: /data
        resources:
          requests:
            cpu: 500m
            memory: 2Gi
  volumeClaimTemplates:
  - metadata:
      name: redis-data
    spec:
      accessModes: ["ReadWriteOnce"]
      resources:
        requests:
          storage: 50Gi
```

---

## Monitoring Stack (Prometheus + Grafana)

```yaml
# Install via Helm
helm repo add prometheus-community https://prometheus-community.github.io/helm-charts
helm install prometheus prometheus-community/kube-prometheus-stack \
  --namespace monitoring \
  --create-namespace \
  --set grafana.adminPassword=${GRAFANA_PASSWORD}
```

**ServiceMonitor for API:**

```yaml
# k8s/servicemonitor.yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: repograph-api
  namespace: repograph
spec:
  selector:
    matchLabels:
      app: repograph-api
  endpoints:
  - port: metrics
    interval: 30s
    path: /metrics
```

---

## CI/CD Pipeline (GitHub Actions)

```yaml
# .github/workflows/deploy.yml
name: Deploy to Kubernetes

on:
  push:
    branches: [main]
    tags: ['v*']

jobs:
  build-and-deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Build Docker image
        run: |
          docker build -t repograph/api:${{ github.sha }} .
          docker tag repograph/api:${{ github.sha }} repograph/api:latest

      - name: Push to registry
        run: |
          echo ${{ secrets.DOCKER_PASSWORD }} | docker login -u ${{ secrets.DOCKER_USERNAME }} --password-stdin
          docker push repograph/api:${{ github.sha }}
          docker push repograph/api:latest

      - name: Deploy to Kubernetes
        uses: azure/k8s-deploy@v4
        with:
          manifests: |
            k8s/api-deployment.yaml
            k8s/api-service.yaml
          images: repograph/api:${{ github.sha }}
          kubectl-version: 'latest'
```

---

## Environments

| Environment | Namespace | Database | Redis | Replicas | Auto-Scale |
|-------------|-----------|----------|-------|----------|------------|
| **Dev** | repograph-dev | Single instance | Single node | 1 | No |
| **Staging** | repograph-staging | RDS (Small) | ElastiCache (Small) | 2 | No |
| **Production** | repograph | RDS (Multi-AZ) | ElastiCache (HA) | 3-20 | Yes (HPA) |

---

## Rollback Strategy

**Blue-Green Deployment:**

```bash
# Deploy new version (green)
kubectl apply -f k8s/api-deployment-green.yaml

# Test green deployment
kubectl port-forward svc/repograph-api-green 8000:80

# Switch traffic to green
kubectl patch service repograph-api -p '{"spec":{"selector":{"version":"v1.1.0"}}}'

# Rollback if needed
kubectl patch service repograph-api -p '{"spec":{"selector":{"version":"v1.0.0"}}}'
```

**Canary Deployment:**

```yaml
# k8s/canary-deployment.yaml
# Deploy canary with 10% traffic
apiVersion: v1
kind: Service
metadata:
  name: repograph-api-canary
spec:
  selector:
    app: repograph-api
    version: v1.1.0-canary
  # ... (10% traffic via Istio/Linkerd)
```

---

## Disaster Recovery

**Backup Strategy:**

1. **Database:** Automated daily backups (30-day retention)
2. **Redis:** Daily snapshots (7-day retention)
3. **Configuration:** Git-versioned (k8s manifests)

**Recovery Time Objective (RTO):** 1 hour
**Recovery Point Objective (RPO):** 15 minutes

**DR Runbook:**

```bash
# 1. Restore database from backup
aws rds restore-db-instance-from-db-snapshot \
  --db-instance-identifier repograph-restored \
  --db-snapshot-identifier repograph-snapshot-2025-11-22

# 2. Update database URL in secrets
kubectl edit secret repograph-secrets -n repograph

# 3. Restart pods
kubectl rollout restart deployment/repograph-api -n repograph
```

---

## Resource Budgets

| Component | CPU (Request/Limit) | Memory (Request/Limit) | Storage |
|-----------|---------------------|------------------------|---------|
| **API Pod** | 500m / 2000m | 1Gi / 4Gi | - |
| **Worker Pod** | 1000m / 4000m | 2Gi / 8Gi | - |
| **PostgreSQL** | 4 vCPU | 32 GB RAM | 500 GB SSD |
| **Redis** | 2 vCPU | 13 GB RAM | 50 GB SSD |

**Total (Production - Min):**
- 3 API pods + 2 Workers = ~7 vCPU, 11 GB RAM
- PostgreSQL + Redis = 6 vCPU, 45 GB RAM
- **Total:** ~13 vCPU, 56 GB RAM, 550 GB storage

**Estimated Monthly Cost (AWS):**
- EKS Cluster: $73/month
- EC2 Instances (3 x t3.xlarge): $150/month
- RDS (db.r6g.xlarge Multi-AZ): $600/month
- ElastiCache (cache.r6g.large): $150/month
- Data Transfer: $50/month
- **Total:** ~$1,023/month

---

**Document Status:** ✅ Complete - Ready for Implementation
