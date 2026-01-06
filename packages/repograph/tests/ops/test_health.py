"""Tests for health check endpoints."""
import pytest
from fastapi.testclient import TestClient


@pytest.fixture
def client():
    from src.api.main import app
    return TestClient(app)


def test_liveness_probe(client):
    """Test liveness probe returns 200."""
    response = client.get("/health/live")
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "alive"
    assert "uptime_seconds" in data["checks"]


def test_readiness_probe_healthy(client, mock_db, mock_redis):
    """Test readiness probe when all dependencies healthy."""
    response = client.get("/health/ready")
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "ready"
    assert data["checks"]["database"]["status"] == "healthy"
    assert data["checks"]["redis"]["status"] == "healthy"


def test_readiness_probe_unhealthy_database(client, mock_db_down):
    """Test readiness probe fails when database is down."""
    response = client.get("/health/ready")
    assert response.status_code == 503


def test_startup_probe(client, mock_db, mock_redis):
    """Test startup probe."""
    response = client.get("/health/startup")
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "started"


def test_detailed_health_check(client, mock_db, mock_redis):
    """Test detailed health check includes all metrics."""
    response = client.get("/health/detailed")
    assert response.status_code == 200
    data = response.json()
    
    assert "database" in data["checks"]
    assert "redis" in data["checks"]
    assert "disk" in data["checks"]
    assert "memory" in data["checks"]
    assert "application" in data["checks"]
