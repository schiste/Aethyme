# RepoGraph Quick Start Guide

Get up and running with RepoGraph development in 15 minutes.

## Prerequisites

- Docker & Docker Compose installed
- Python 3.11+
- Make (usually pre-installed on macOS/Linux)
- Git

## Quick Start

### 1. Start Development Environment

```bash
# Start all services (PostgreSQL, Redis, API, Grafana, Prometheus, etc.)
make dev
```

Wait ~2 minutes for all services to start. You'll see output confirming services are up.

### 2. Verify Services

```bash
# Check all services are running
make ps

# Check health
make health
```

### 3. Access Services

Open in your browser:

- **API Documentation:** http://localhost:8000/docs
- **Grafana Dashboards:** http://localhost:3000 (admin/admin)
- **Prometheus Metrics:** http://localhost:9090
- **Jaeger Tracing:** http://localhost:16686
- **PgAdmin:** http://localhost:5050 (admin@repograph.dev/admin)
- **Redis Commander:** http://localhost:8081

### 4. Run Migrations & Seed Data

```bash
# Run database migrations
make migrate

# Seed test data
make seed
```

### 5. Run Tests

```bash
# Run all tests with coverage
make test

# Run only fast tests
pytest -m "not slow" -v

# Run specific test file
pytest tests/test_auth.py -v
```

### 6. Development Workflow

```bash
# Watch logs
make dev-logs

# Open database shell
make db-shell

# Open API container shell
make api-shell

# Run linting
make lint

# Format code
make format

# Run benchmarks
make benchmark
```

## Common Commands

```bash
make help          # Show all available commands
make dev           # Start development environment
make dev-down      # Stop all services
make test          # Run tests with coverage
make lint          # Run linters
make format        # Format code
make ci            # Run full CI pipeline locally
make clean         # Clean up generated files
```

## Project Structure

```
packages/repograph/
├── .github/workflows/     # CI/CD pipelines
├── benchmarks/           # Performance benchmarks
├── monitoring/           # Grafana, Prometheus, OTEL configs
├── project/              # Project management docs
│   ├── sprint-1-board.md
│   ├── STAGE_1_ROADMAP_TRACKER.md
│   └── templates/
├── scripts/              # Utility scripts
├── src/                  # Application source code
├── tests/                # Test suite
├── docker-compose.dev.yml
├── Makefile
└── requirements-dev.txt
```

## Key Documentation

- **Sprint Board:** `project/sprint-1-board.md`
- **Roadmap Tracker:** `project/STAGE_1_ROADMAP_TRACKER.md`
- **Release Process:** `project/RELEASE_PROCESS.md`
- **Test Guide:** `tests/README.md`
- **Infrastructure:** `project/infrastructure-summary.md`

## Troubleshooting

### Services won't start

```bash
# Stop everything
make dev-down

# Clean up
docker system prune -f

# Try again
make dev
```

### Database connection errors

```bash
# Check PostgreSQL is running
docker ps | grep postgres

# Restart services
make restart
```

### Tests failing

```bash
# Clean environment
make clean

# Reinstall dependencies
make install-dev

# Run tests
make test
```

### Port conflicts

If ports 5432, 6379, 8000, etc. are already in use:

1. Stop conflicting services
2. Or modify `docker-compose.dev.yml` to use different ports

## Next Steps

1. Review the [Sprint 1 Board](project/sprint-1-board.md)
2. Check the [Stage 1 Roadmap](project/STAGE_1_ROADMAP_TRACKER.md)
3. Read the [Test Guide](tests/README.md)
4. Explore the [Grafana Dashboard](http://localhost:3000)

## Getting Help

- Check `make help` for available commands
- Review documentation in `project/` directory
- Check `tests/README.md` for testing guidelines
- Review logs: `make dev-logs`

## First Task Checklist

- [ ] Environment running (`make dev`)
- [ ] Services accessible (check URLs above)
- [ ] Tests passing (`make test`)
- [ ] Grafana dashboard visible
- [ ] Database seeded (`make seed`)
- [ ] Reviewed Sprint 1 Board
- [ ] Ready to code!

---

**Setup Time:** 10-15 minutes
**First Test Run:** <1 minute
**First API Request:** Immediate

Happy coding!
