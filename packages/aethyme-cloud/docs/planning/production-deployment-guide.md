# Production Deployment Guide

**Quick reference for deploying Aethyme Cloud to production**

---

## Prerequisites

- Docker & Docker Compose
- Kubernetes cluster (recommended for production)
- PostgreSQL 14+
- Redis 6+
- Elasticsearch 8+
- Domain with SSL certificate

---

## Quick Start (Docker Compose)

### 1. Clone and Configure
```bash
cd packages/aethyme-cloud
cp .env.example .env
```

### 2. Set Environment Variables
Edit `.env`:
```bash
ENVIRONMENT=production
DATABASE_URL=postgresql+asyncpg://user:pass@postgres:5432/aethyme
REDIS_URL=redis://redis:6379/0
ELASTICSEARCH_URL=http://elasticsearch:9200
JWT_SECRET_KEY=$(openssl rand -hex 32)
REFRESH_TOKEN_SECRET_KEY=$(openssl rand -hex 32)
ENCRYPTION_KEY=$(python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())")
SENTRY_DSN=https://your-sentry-dsn
API_CORS_ORIGINS=https://app.yourdomain.com
```

### 3. Deploy
```bash
docker-compose -f docker-compose.production.yml up -d
```

### 4. Verify
```bash
# Check health
curl http://localhost:8000/health/detailed

# Should return all services as "healthy"
```

---

## Kubernetes Deployment

### 1. Create Namespace
```bash
kubectl create namespace aethyme-cloud
```

### 2. Create Secrets
```bash
# Database credentials
kubectl create secret generic db-credentials \
  --from-literal=url='postgresql+asyncpg://user:pass@postgres:5432/aethyme' \
  -n aethyme-cloud

# JWT secrets
kubectl create secret generic jwt-secrets \
  --from-literal=secret-key=$(openssl rand -hex 32) \
  --from-literal=refresh-secret=$(openssl rand -hex 32) \
  -n aethyme-cloud

# Encryption key
kubectl create secret generic encryption-key \
  --from-literal=key=$(python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())") \
  -n aethyme-cloud
```

### 3. Deploy Services

#### ConfigMap
```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: aethyme-config
  namespace: aethyme-cloud
data:
  ENVIRONMENT: "production"
  REDIS_URL: "redis://redis:6379/0"
  ELASTICSEARCH_URL: "http://elasticsearch:9200"
  API_HOST: "0.0.0.0"
  API_PORT: "8000"
  LOG_LEVEL: "INFO"
  SENTRY_ENVIRONMENT: "production"
  SENTRY_TRACES_SAMPLE_RATE: "0.1"
```

#### Deployment
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: aethyme-api
  namespace: aethyme-cloud
spec:
  replicas: 3
  selector:
    matchLabels:
      app: aethyme-api
  template:
    metadata:
      labels:
        app: aethyme-api
    spec:
      containers:
      - name: api
        image: aethyme/cloud-api:1.0.0
        ports:
        - containerPort: 8000
        envFrom:
        - configMapRef:
            name: aethyme-config
        env:
        - name: DATABASE_URL
          valueFrom:
            secretKeyRef:
              name: db-credentials
              key: url
        - name: JWT_SECRET_KEY
          valueFrom:
            secretKeyRef:
              name: jwt-secrets
              key: secret-key
        - name: REFRESH_TOKEN_SECRET_KEY
          valueFrom:
            secretKeyRef:
              name: jwt-secrets
              key: refresh-secret
        - name: ENCRYPTION_KEY
          valueFrom:
            secretKeyRef:
              name: encryption-key
              key: key
        livenessProbe:
          httpGet:
            path: /health
            port: 8000
          initialDelaySeconds: 30
          periodSeconds: 10
          timeoutSeconds: 5
          failureThreshold: 3
        readinessProbe:
          httpGet:
            path: /health/ready
            port: 8000
          initialDelaySeconds: 10
          periodSeconds: 5
          timeoutSeconds: 3
          failureThreshold: 3
        resources:
          requests:
            cpu: 500m
            memory: 512Mi
          limits:
            cpu: 2000m
            memory: 2Gi
```

#### Service
```yaml
apiVersion: v1
kind: Service
metadata:
  name: aethyme-api
  namespace: aethyme-cloud
spec:
  selector:
    app: aethyme-api
  ports:
  - port: 80
    targetPort: 8000
  type: LoadBalancer
```

#### Ingress (with TLS)
```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: aethyme-api
  namespace: aethyme-cloud
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
    nginx.ingress.kubernetes.io/rate-limit: "100"
spec:
  tls:
  - hosts:
    - api.yourdomain.com
    secretName: aethyme-tls
  rules:
  - host: api.yourdomain.com
    http:
      paths:
      - path: /
        pathType: Prefix
        backend:
          service:
            name: aethyme-api
            port:
              number: 80
```

### 4. Deploy
```bash
kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml
kubectl apply -f k8s/ingress.yaml
```

### 5. Verify
```bash
# Check pods
kubectl get pods -n aethyme-cloud

# Check logs
kubectl logs -f deployment/aethyme-api -n aethyme-cloud

# Check health
curl https://api.yourdomain.com/health/detailed
```

---

## Monitoring Setup

### 1. OpenTelemetry Collector

#### Deploy Collector
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: otel-collector
  namespace: aethyme-cloud
spec:
  replicas: 1
  selector:
    matchLabels:
      app: otel-collector
  template:
    spec:
      containers:
      - name: collector
        image: otel/opentelemetry-collector:latest
        ports:
        - containerPort: 4317  # OTLP gRPC
        - containerPort: 4318  # OTLP HTTP
        volumeMounts:
        - name: config
          mountPath: /etc/otel
      volumes:
      - name: config
        configMap:
          name: otel-collector-config
```

#### Collector Config
```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: otel-collector-config
  namespace: aethyme-cloud
data:
  config.yaml: |
    receivers:
      otlp:
        protocols:
          grpc:
            endpoint: 0.0.0.0:4317
          http:
            endpoint: 0.0.0.0:4318

    processors:
      batch:
        timeout: 1s
        send_batch_size: 1024

      memory_limiter:
        check_interval: 1s
        limit_mib: 512

    exporters:
      jaeger:
        endpoint: jaeger:14250
        tls:
          insecure: true

      prometheus:
        endpoint: 0.0.0.0:8889

    service:
      pipelines:
        traces:
          receivers: [otlp]
          processors: [memory_limiter, batch]
          exporters: [jaeger]
        metrics:
          receivers: [otlp]
          processors: [memory_limiter, batch]
          exporters: [prometheus]
```

### 2. Prometheus

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: prometheus-config
data:
  prometheus.yml: |
    global:
      scrape_interval: 15s

    scrape_configs:
    - job_name: 'aethyme-api'
      kubernetes_sd_configs:
      - role: pod
        namespaces:
          names:
          - aethyme-cloud
      relabel_configs:
      - source_labels: [__meta_kubernetes_pod_label_app]
        action: keep
        regex: aethyme-api

    - job_name: 'otel-collector'
      static_configs:
      - targets: ['otel-collector:8889']
```

### 3. Grafana Dashboard

Import dashboard for:
- Request rate, latency, errors
- Database connection pool status
- Redis operations
- Rate limit hits
- Celery worker status

---

## Backup Strategy

### Database Backups
```bash
# Daily backup
0 2 * * * pg_dump -h postgres -U user aethyme | gzip > /backups/aethyme-$(date +\%Y\%m\%d).sql.gz

# Retention: 7 days
find /backups -name "aethyme-*.sql.gz" -mtime +7 -delete
```

### Redis Backups
```bash
# Enable RDB snapshots
save 900 1
save 300 10
save 60 10000
```

### Elasticsearch Snapshots
```bash
# Register snapshot repository
PUT /_snapshot/backups
{
  "type": "fs",
  "settings": {
    "location": "/usr/share/elasticsearch/backups"
  }
}

# Create snapshot
PUT /_snapshot/backups/snapshot_$(date +\%Y\%m\%d)
```

---

## Scaling

### Horizontal Scaling (API)
```bash
# Scale to 5 replicas
kubectl scale deployment aethyme-api --replicas=5 -n aethyme-cloud
```

### Vertical Scaling (Resources)
```yaml
resources:
  requests:
    cpu: 1000m
    memory: 1Gi
  limits:
    cpu: 4000m
    memory: 4Gi
```

### Auto-Scaling
```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: aethyme-api-hpa
  namespace: aethyme-cloud
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: aethyme-api
  minReplicas: 3
  maxReplicas: 10
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
```

---

## Maintenance

### Database Migrations
```bash
# Run migrations
kubectl exec -it deployment/aethyme-api -n aethyme-cloud -- \
  python -m alembic upgrade head
```

### View Logs
```bash
# API logs
kubectl logs -f deployment/aethyme-api -n aethyme-cloud

# Celery worker logs
kubectl logs -f deployment/aethyme-worker -n aethyme-cloud

# Follow logs from all pods
kubectl logs -f -l app=aethyme-api -n aethyme-cloud --all-containers
```

### Restart Services
```bash
# Rolling restart
kubectl rollout restart deployment/aethyme-api -n aethyme-cloud

# Check rollout status
kubectl rollout status deployment/aethyme-api -n aethyme-cloud
```

---

## Troubleshooting

### 1. API Not Responding
```bash
# Check pod status
kubectl get pods -n aethyme-cloud

# Check pod logs
kubectl logs -f deployment/aethyme-api -n aethyme-cloud

# Check events
kubectl get events -n aethyme-cloud --sort-by='.lastTimestamp'

# Describe pod
kubectl describe pod <pod-name> -n aethyme-cloud
```

### 2. Database Connection Issues
```bash
# Check database health from pod
kubectl exec -it deployment/aethyme-api -n aethyme-cloud -- \
  curl http://localhost:8000/health/detailed

# Test database connection
kubectl exec -it deployment/aethyme-api -n aethyme-cloud -- \
  python -c "from app.core.database import init_db; import asyncio; asyncio.run(init_db())"
```

### 3. High Memory Usage
```bash
# Check resource usage
kubectl top pods -n aethyme-cloud

# Increase memory limits
kubectl set resources deployment aethyme-api \
  --limits=memory=4Gi \
  -n aethyme-cloud
```

### 4. Rate Limit Issues
```bash
# Check Redis
kubectl exec -it deployment/aethyme-api -n aethyme-cloud -- \
  redis-cli -h redis ping

# Clear rate limits (emergency)
kubectl exec -it deployment/aethyme-api -n aethyme-cloud -- \
  redis-cli -h redis FLUSHDB
```

---

## Security Checklist

- [ ] TLS/SSL configured for all endpoints
- [ ] Secrets stored in Kubernetes secrets (not ConfigMaps)
- [ ] Network policies configured
- [ ] RBAC enabled and configured
- [ ] Pod security policies enabled
- [ ] Image vulnerability scanning enabled
- [ ] Database encryption at rest enabled
- [ ] Backup encryption enabled
- [ ] Rate limiting configured
- [ ] DDoS protection enabled (Cloudflare/AWS Shield)
- [ ] Security headers verified
- [ ] CORS origins restricted
- [ ] Sentry DSN kept secret
- [ ] Database credentials rotated regularly
- [ ] JWT secrets rotated regularly

---

## Performance Optimization

### Database
- [ ] Indexes created for frequent queries
- [ ] Connection pool sized appropriately
- [ ] Query timeout configured
- [ ] Slow query logging enabled

### API
- [ ] GZip compression enabled
- [ ] Response caching configured
- [ ] Static assets CDN configured
- [ ] Keep-alive configured

### Redis
- [ ] Persistence configured (RDB/AOF)
- [ ] Maxmemory policy set
- [ ] Connection pool configured

---

## Monitoring Alerts

Configure alerts for:
- API response time > 1s
- Error rate > 1%
- Database connection pool > 80%
- Memory usage > 80%
- CPU usage > 80%
- Disk usage > 80%
- Health check failures
- Rate limit excessive hits
- Celery queue backlog

---

**Last Updated**: 2025-10-05
**Version**: 1.0.0
