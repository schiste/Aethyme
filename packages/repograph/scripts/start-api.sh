#!/usr/bin/env bash
set -euo pipefail

# Load environment variables
if [ -f .env ]; then
  export $(grep -v '^#' .env | xargs)
fi

# Default values
HOST="${API_HOST:-0.0.0.0}"
PORT="${API_PORT:-8001}"
WORKERS="${API_WORKERS:-4}"
LOG_LEVEL="${LOG_LEVEL:-info}"

echo "[RepoGraph] Starting API server"
echo "  Host: $HOST:$PORT"
echo "  Workers: $WORKERS"
echo "  Log Level: $LOG_LEVEL"

# Change to package directory
cd "$(dirname "$0")/.."

# Run migrations if needed
echo "[RepoGraph] Checking database..."
python -c "
from src.graph.connection_pool import db_pool
health = db_pool.health_check()
print(f'Database status: {health}')
"

# Start API server
if [ "${ENVIRONMENT:-development}" = "production" ]; then
  # Production: Use gunicorn with uvicorn workers
  echo "[RepoGraph] Starting in production mode with gunicorn"
  exec gunicorn src.api.main:app \
    --workers "$WORKERS" \
    --worker-class uvicorn.workers.UvicornWorker \
    --bind "$HOST:$PORT" \
    --log-level "$LOG_LEVEL" \
    --access-logfile - \
    --error-logfile - \
    --timeout 120 \
    --keep-alive 5 \
    --max-requests 1000 \
    --max-requests-jitter 50
else
  # Development: Use uvicorn with auto-reload
  echo "[RepoGraph] Starting in development mode with auto-reload"
  exec uvicorn src.api.main:app \
    --host "$HOST" \
    --port "$PORT" \
    --reload \
    --log-level "$LOG_LEVEL"
fi