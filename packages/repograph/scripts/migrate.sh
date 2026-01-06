#!/usr/bin/env bash
set -euo pipefail

# Load environment variables
if [ -f .env ]; then
  export $(grep -v '^#' .env | xargs)
fi

# Parse DATABASE_URL or use individual components
if [ -n "${DATABASE_URL:-}" ]; then
  # Extract components from DATABASE_URL
  # Format: postgresql://user:password@host:port/database
  DB_USER=$(echo "$DATABASE_URL" | sed -n 's/.*\/\/\([^:]*\):.*/\1/p')
  DB_PASSWORD=$(echo "$DATABASE_URL" | sed -n 's/.*:\/\/[^:]*:\([^@]*\)@.*/\1/p')
  DB_HOST=$(echo "$DATABASE_URL" | sed -n 's/.*@\([^:\/]*\).*/\1/p')
  DB_PORT=$(echo "$DATABASE_URL" | sed -n 's/.*:\([0-9]*\)\/.*/\1/p')
  DB_NAME=$(echo "$DATABASE_URL" | sed -n 's/.*\/\([^?]*\).*/\1/p')
fi

# Use individual env vars as fallback
DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5432}"
DB_NAME="${DB_NAME:-repograph}"
DB_USER="${DB_USER:-repograph}"

echo "[RepoGraph] Running database migrations"
echo "  Host: $DB_HOST:$DB_PORT"
echo "  Database: $DB_NAME"
echo "  User: $DB_USER"

# Change to package directory
cd "$(dirname "$0")/.."

# Check if database exists
echo "[RepoGraph] Checking database connection..."
PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d postgres -c "SELECT 1" > /dev/null 2>&1

if [ $? -ne 0 ]; then
  echo "[Error] Cannot connect to PostgreSQL. Please ensure the database server is running."
  exit 1
fi

# Create database if it doesn't exist
echo "[RepoGraph] Creating database if not exists..."
PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d postgres -tc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" | grep -q 1 || \
  PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d postgres -c "CREATE DATABASE $DB_NAME"

# Run migrations
echo "[RepoGraph] Applying migrations..."
for migration in migrations/*.sql; do
  if [ -f "$migration" ]; then
    echo "  Applying: $(basename "$migration")"
    PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -f "$migration"
  fi
done

echo "[RepoGraph] Migrations complete!"

# Verify schema
echo "[RepoGraph] Verifying schema..."
PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -c "
SELECT
  table_name,
  COUNT(*) as column_count
FROM information_schema.columns
WHERE table_schema = 'repograph'
GROUP BY table_name
ORDER BY table_name;
"