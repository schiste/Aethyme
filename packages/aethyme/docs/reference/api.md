# Aethyme API Reference

Complete API reference for Aethyme endpoints.

## Base URL

```
https://api.aethyme.com/v1
```

## Authentication

All API requests require authentication via JWT token or API key:

```bash
curl -H "Authorization: Bearer YOUR_TOKEN" \
     -H "X-Organization-ID: YOUR_ORG" \
     https://api.aethyme.com/v1/search?query=UserService
```

## Endpoints

### System Endpoints

**GET /api/v1/health** - Health check
**GET /api/v1/status** - System status
**GET /api/v1/version** - Version information
**GET /api/v1/info** - API information

### Authentication

**POST /api/v1/auth/login** - User login
**POST /api/v1/auth/token** - Generate token
**POST /api/v1/auth/refresh** - Refresh token

### Query Endpoints

**GET /api/v1/search** - Search for symbols
**GET /api/v1/ego/{symbol}** - Get ego graph
**GET /api/v1/impact/{symbol}** - Impact analysis

### Scorecard Endpoints

**POST /api/v1/scorecard/scan** - Trigger scorecard scan
**GET /api/v1/scorecard/results/{scan_id}** - Get scan results
**GET /api/v1/scorecard/summary/{repo_id}** - Latest summary
**GET /api/v1/scorecard/history/{repo_id}** - Scan history
**GET /api/v1/scorecard/checks** - List available checks

### Autofix Endpoints

**POST /api/v1/autofix/run** - Run autofixes
**POST /api/v1/autofix/apply** - Apply fixes
**GET /api/v1/autofix/types** - List fix types
**GET /api/v1/autofix/history/{repo_id}** - Autofix history

### Telemetry Endpoints

**GET /api/v1/telemetry/metrics** - List metrics
**POST /api/v1/telemetry/query** - Query metrics
**GET /api/v1/telemetry/summary/{metric}** - Metric summary
**GET /api/v1/telemetry/kpi** - KPI dashboard
**POST /api/v1/telemetry/event** - Log event

### Guardrails Endpoints

**GET /api/v1/guardrails/list** - List guardrails
**GET /api/v1/guardrails/config** - Get configuration
**POST /api/v1/guardrails/config** - Update configuration
**POST /api/v1/guardrails/schema-first/validate** - Validate schema
**POST /api/v1/guardrails/drift-sentinel/check** - Check drift
**POST /api/v1/guardrails/model-routing/route** - Route model
**GET /api/v1/guardrails/violations** - Get violations
**GET /api/v1/guardrails/stats** - Statistics

## Interactive Documentation

- Swagger UI: https://api.aethyme.com/docs
- ReDoc: https://api.aethyme.com/redoc
- OpenAPI Spec: https://api.aethyme.com/openapi.json
