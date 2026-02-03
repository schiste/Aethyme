# Troubleshooting Guide

Common issues and solutions for Aethyme.

---

## Installation Issues

### Python Version Mismatch

**Error:** `python: command not found` or wrong version

**Solution:**
```bash
# Install Python 3.11+
brew install python@3.11  # macOS
sudo apt install python3.11  # Ubuntu

# Verify
python3 --version
```

### PostgreSQL Connection Failed

**Error:** `could not connect to server`

**Diagnostic:**
```bash
# Check if PostgreSQL is running
pg_isready -h localhost -p 5432

# Check logs
tail -f /usr/local/var/log/postgres.log  # macOS
sudo journalctl -u postgresql  # Linux
```

**Solutions:**
```bash
# Start PostgreSQL
brew services start postgresql@15  # macOS
sudo systemctl start postgresql  # Linux

# Check connection
psql -h localhost -U aethyme -d aethyme

# Reset password if needed
psql postgres -c "ALTER USER aethyme PASSWORD 'newpassword';"
```

### Redis Connection Failed

**Error:** `Redis connection refused`

**Solution:**
```bash
# Start Redis
brew services start redis  # macOS
sudo systemctl start redis  # Linux

# Verify
redis-cli ping
# Expected: PONG
```

---

## Indexing Issues

### SCIP Binary Not Found

**Error:** `SCIP binary not found, falling back to regex indexer`

**Solution:**
```bash
# Install SCIP for TypeScript
npm install -g @sourcegraph/scip-typescript

# Install SCIP for Python
wget https://github.com/sourcegraph/scip-python/releases/latest/download/scip-python-linux
chmod +x scip-python-linux
sudo mv scip-python-linux /usr/local/bin/scip-python

# Verify
which scip-typescript
which scip-python
```

### Out of Memory During Indexing

**Error:** Container/process killed

**Diagnostic:**
```bash
# Check memory usage
free -h
docker stats --no-stream
```

**Solutions:**
```bash
# Increase Docker memory
docker update --memory="4g" aethyme-api

# Reduce batch size
export INDEXING_BATCH_SIZE=500
export INDEXING_CONCURRENCY=2

# Use fallback indexer (lighter)
python -m src.cli index /path/to/repo --use-fallback
```

### Slow Indexing

**Problem:** Indexing takes > 10 minutes

**Solutions:**
```bash
# Enable parallel processing
export INDEXING_CONCURRENCY=8

# Increase timeout
export INDEXING_TIMEOUT=1800

# Check disk I/O
iostat -x 1 10

# Use SSD for database
# Move PostgreSQL data directory to SSD
```

---

## Query Issues

### Empty Search Results

**Problem:** Search returns no results for known symbols

**Diagnostic:**
```bash
# Check if repository is indexed
python -m src.cli stats

# Verify nodes exist
psql -U aethyme -d aethyme -c "SELECT COUNT(*) FROM nodes;"

# Check specific repository
psql -U aethyme -d aethyme -c \
  "SELECT COUNT(*) FROM nodes WHERE repository_id = 'your-repo-id';"
```

**Solutions:**
```bash
# Re-index repository
python -m src.cli index /path/to/repo

# Clear cache
redis-cli FLUSHDB

# Check tenant isolation
echo $AETHYME_TENANT_ID
```

### Slow Queries

**Problem:** Queries taking > 5 seconds

**Diagnostic:**
```bash
# Check query metrics
curl 'http://prometheus:9090/api/v1/query?query=histogram_quantile(0.95,aethyme_request_duration_seconds)'

# Enable query logging
LOG_LEVEL=DEBUG python -m src.cli search "query"

# Check database performance
psql -U aethyme -d aethyme -c \
  "SELECT * FROM pg_stat_statements ORDER BY mean_exec_time DESC LIMIT 5;"
```

**Solutions:**
```bash
# Add database indexes (see performance runbook)
psql -U aethyme -d aethyme < scripts/add-indexes.sql

# Enable Redis caching
export REDIS_URL=redis://localhost:6379/0
export REDIS_CACHE_TTL=600

# Increase database resources
# Edit docker-compose.yml or k8s manifests
```

---

## Authentication Issues

### Invalid Token

**Error:** `401 Unauthorized` or `Invalid token`

**Diagnostic:**
```bash
# Check token expiration
echo $TOKEN | cut -d. -f2 | base64 -d | jq .exp

# Compare with current time
date +%s
```

**Solutions:**
```bash
# Get new token
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -d '{"email":"test@example.com","password":"test1234"}' \
  | jq -r .access_token)

# Use API key instead
export AETHYME_API_KEY="rg_live_..."
curl -H "X-API-Key: $AETHYME_API_KEY" ...
```

### Permission Denied

**Error:** `403 Forbidden`

**Diagnostic:**
```bash
# Check user permissions
curl http://localhost:8001/api/auth/me \
  -H "Authorization: Bearer $TOKEN" | jq .permissions
```

**Solution:**
```bash
# Contact admin to grant permissions
# Or use token with correct scopes
```

---

## API Issues

### Rate Limit Exceeded

**Error:** `429 Too Many Requests`

**Response:**
```json
{
  "error": "Rate limit exceeded",
  "retry_after": 45
}
```

**Solutions:**
```bash
# Wait and retry
sleep 45
# Retry request

# Implement exponential backoff
for i in {1..5}; do
  response=$(curl -s -w "%{http_code}" ...)
  if [ $response -ne 429 ]; then
    break
  fi
  sleep $((2**i))
done

# Request higher rate limit (enterprise)
```

### Service Unavailable

**Error:** `503 Service Unavailable`

**Diagnostic:**
```bash
# Check service health
curl http://localhost:8001/health/detailed

# Check if API is running
docker ps | grep aethyme-api
kubectl get pods -n aethyme
```

**Solutions:**
```bash
# Restart services
docker-compose -f ops/docker-compose.yml restart
kubectl rollout restart deployment/aethyme-api

# Check logs
docker logs aethyme-api --tail=100
kubectl logs -n aethyme deployment/aethyme-api
```

---

## Database Issues

### Migration Failed

**Error:** Migration fails during `alembic upgrade head`

**Diagnostic:**
```bash
# Check current revision
alembic current

# Show migration history
alembic history
```

**Solutions:**
```bash
# Rollback failed migration
alembic downgrade -1

# Fix migration script
# Edit alembic/versions/xxx_migration.py

# Try again
alembic upgrade head

# If corrupted, restore from backup
bash scripts/restore-database.sh /backups/latest.sql.gz
```

### Connection Pool Exhausted

**Error:** `connection pool exhausted`

**Diagnostic:**
```bash
# Check active connections
psql -U aethyme -d aethyme -c \
  "SELECT count(*) FROM pg_stat_activity WHERE state = 'active';"

# Check max connections
psql -U aethyme -d aethyme -c "SHOW max_connections;"
```

**Solutions:**
```bash
# Increase pool size
DB_POOL_MAX_SIZE=50

# Increase PostgreSQL max connections
psql postgres -c "ALTER SYSTEM SET max_connections = 200;"
pg_ctl restart

# Find and kill long-running queries
psql -U aethyme -d aethyme -c \
  "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
   WHERE state = 'active' AND query_start < NOW() - INTERVAL '5 minutes';"
```

---

## CLI Issues

### Command Not Found

**Error:** `python -m src.cli: command not found`

**Solutions:**
```bash
# Ensure virtual environment activated
source venv/bin/activate

# Verify installation
pip list | grep aethyme

# Reinstall in development mode
pip install -e ".[dev]"

# Check Python path
python -c "import sys; print(sys.path)"
```

### Import Errors

**Error:** `ModuleNotFoundError: No module named 'src'`

**Solutions:**
```bash
# Run from project root
cd /path/to/aethyme/packages/aethyme

# Set PYTHONPATH
export PYTHONPATH="/path/to/aethyme/packages/aethyme:$PYTHONPATH"

# Reinstall dependencies
pip install -e ".[dev]"
```

---

## Docker Issues

### Container Exits Immediately

**Diagnostic:**
```bash
# Check container logs
docker logs aethyme-api

# Check container status
docker ps -a | grep aethyme
```

**Solutions:**
```bash
# Run in foreground to see errors
docker run -it --rm aethyme/api:latest

# Check environment variables
docker exec aethyme-api env

# Rebuild image
docker-compose -f ops/docker-compose.yml build --no-cache
```

### Volume Mount Issues

**Error:** Permission denied or files not found

**Solutions:**
```bash
# Check volume mounts
docker inspect aethyme-api | jq '.[0].Mounts'

# Fix permissions
chmod -R a+rX /path/to/repo

# Use absolute paths
docker run -v /absolute/path:/repos ...
```

---

## Kubernetes Issues

### Pod CrashLoopBackOff

**Diagnostic:**
```bash
# Check pod status
kubectl get pods -n aethyme

# Check logs
kubectl logs -n aethyme aethyme-api-xxx

# Describe pod
kubectl describe pod -n aethyme aethyme-api-xxx
```

**Solutions:**
```bash
# Check resource limits
kubectl describe pod aethyme-api-xxx | grep -A 5 Resources

# Increase resources
kubectl set resources deployment aethyme-api \
  --limits=memory=4Gi,cpu=2 \
  --requests=memory=2Gi,cpu=1

# Check secrets
kubectl get secrets -n aethyme
```

### Image Pull Errors

**Error:** `ImagePullBackOff`

**Solutions:**
```bash
# Check image exists
docker images | grep aethyme

# Verify image tag
kubectl describe pod aethyme-api-xxx | grep Image

# Check registry credentials
kubectl get secrets -n aethyme

# Use correct image registry
# Edit deployment.yaml
```

---

## Performance Issues

See the dedicated [Performance Degradation Runbook](../runbooks/performance-degradation.md) for:
- Slow queries
- High CPU/memory
- Database tuning
- Scaling procedures

---

## Getting Help

### Self-Service Resources

1. **Documentation**: Check relevant docs
2. **GitHub Issues**: Search existing issues
3. **Logs**: Always check logs first

### Escalation Path

1. **Slack**: #aethyme-support (internal)
2. **GitHub Discussions**: Public Q&A
3. **Email**: support@aethyme.com
4. **Emergency**: On-call via PagerDuty

### Providing Information

When asking for help, include:

```bash
# System info
uname -a
python --version
psql --version
docker --version

# Service status
curl http://localhost:8001/health/detailed

# Logs (last 50 lines)
docker logs aethyme-api --tail=50

# Database connection
psql -U aethyme -d aethyme -c "SELECT COUNT(*) FROM nodes;"

# Configuration (redact secrets!)
env | grep AETHYME
```

---

## Related Documentation

- [Runbooks](../runbooks/) - Operational procedures
- [CLI Reference](../reference/cli.md) - Command documentation
- [API Reference](../reference/api.md) - API documentation
- [Onboarding Guide](../getting-started/onboarding.md) - Getting started

---

**Last Updated:** 2025-11-22
