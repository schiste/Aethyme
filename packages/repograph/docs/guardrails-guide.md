# RepoGraph Guardrails Guide

## Overview

The RepoGraph Guardrails system provides safety mechanisms to prevent unsafe operations, detect schema drift, and enforce schema-first development practices. This guide covers all guardrail features and how to use them effectively.

## Features

### 1. Schema-First Planning

Schema-first planning ensures that schemas, routes, and API contracts are defined before code generation, preventing drift and ensuring consistency.

#### Usage

```python
from src.guardrails.schema_first import SchemaGate

# Initialize gate
gate = SchemaGate("/path/to/repo", strict_mode=True)
gate.initialize()

# Check operation
result = gate.check_operation(
    operation_type='endpoint',
    targets=['/api/users'],
)

if not result.is_valid:
    print("Errors:", result.errors)
    print("Suggestions:", result.suggestions)
```

#### Schema Types Supported

- **OpenAPI/Swagger**: `openapi.yaml`, `swagger.json`
- **GraphQL**: `*.graphql` files
- **TypeScript**: Interfaces and types in `*.ts` files
- **Pydantic**: Python models inheriting from `BaseModel`
- **JSON Schema**: `*.schema.json` files
- **Protobuf**: `*.proto` files (future)

#### Best Practices

1. **Define schemas first** before implementing features
2. **Run validation** before generating code
3. **Use strict mode** in CI/CD pipelines
4. **Override carefully** - only when absolutely necessary

### 2. Drift Sentinels

Drift sentinels detect schema and code changes that could cause issues.

#### Preflight Checks

Run preflight checks before making changes:

```python
from src.guardrails.sentinels import PreflightCheck

checker = PreflightCheck("/path/to/repo")

context = {
    'files': [
        {'path': 'routes.py', 'content': file_content},
    ],
    'old_schemas': old_schemas,
    'new_schemas': new_schemas,
    'known_routes': {'/users', '/products'},
}

results = checker.run_checks(context)

if not results['passed']:
    print(f"Detections: {results['total_detections']}")
    print(f"Max risk: {results['max_risk']}")

    if results['blocked']:
        raise RuntimeError("Operation blocked by guardrails")
```

#### Detection Rules

| Rule ID | Description | Risk Level |
|---------|-------------|------------|
| `no_generated_edits` | Prevent edits to auto-generated files | HIGH |
| `no_breaking_schema` | Prevent breaking schema changes | CRITICAL |
| `no_route_drift` | Ensure routes match API spec | HIGH |
| `type_mismatch` | Detect type mismatches | MEDIUM |
| `schema_entity_removed` | Detect removed entities | HIGH |
| `schema_endpoint_removed` | Detect removed endpoints | CRITICAL |

#### Custom Rules

Add custom sentinel rules:

```python
from src.guardrails.sentinels import SentinelRule, RiskLevel

def custom_check(context):
    detections = []
    # Your custom logic here
    return detections

rule = SentinelRule(
    rule_id='custom_check',
    name='Custom Security Check',
    description='Check for security issues',
    risk_level=RiskLevel.HIGH,
    check_function=custom_check,
)

checker.add_rule(rule)
```

### 3. Feature Flags

Control guardrails dynamically using feature flags.

#### Default Flags

- `GUARDRAILS_V1`: Master guardrails switch (enabled)
- `SCHEMA_FIRST`: Schema-first planning (enabled)
- `DRIFT_SENTINELS`: Drift detection (enabled)
- `PREFLIGHT_CHECKS`: Preflight checks (enabled)

#### Usage

```python
from src.guardrails.flags import is_enabled, get_flags_instance

# Check if enabled
if is_enabled('SCHEMA_FIRST', tenant_id='tenant1'):
    # Run schema-first checks
    pass

# Manage flags
flags = get_flags_instance()

# Enable/disable globally
flags.enable('SCHEMA_FIRST')
flags.disable('SCHEMA_FIRST')

# Rollout gradually
flags.set_rollout('SCHEMA_FIRST', percentage=50)

# Whitelist/blacklist tenants
flags.whitelist_tenant('SCHEMA_FIRST', 'trusted_tenant')
flags.blacklist_tenant('SCHEMA_FIRST', 'problem_tenant')
```

#### Rollout Strategy

1. **Testing Phase**: Whitelist internal tenants
2. **Gradual Rollout**: Set rollout percentage (10% → 25% → 50% → 100%)
3. **Monitor**: Watch for issues in metrics
4. **Rollback**: Disable or blacklist if problems occur

## API Endpoints

### Check Operation

```http
POST /api/v1/guardrails/check
Authorization: Bearer <token>

{
  "operation_type": "endpoint",
  "targets": ["/api/users"],
  "override": false
}
```

Response:
```json
{
  "is_valid": false,
  "errors": ["Endpoint '/api/users' not found in any schema"],
  "warnings": [],
  "suggestions": ["Add '/api/users' to API schema before implementing"]
}
```

### Preflight Check

```http
POST /api/v1/guardrails/preflight
Authorization: Bearer <token>

{
  "files": [
    {
      "path": "routes.py",
      "content": "..."
    }
  ],
  "context": {}
}
```

Response:
```json
{
  "passed": false,
  "blocked": true,
  "max_risk": "critical",
  "total_detections": 2,
  "detections": [
    {
      "rule_id": "schema_endpoint_removed",
      "risk_level": "critical",
      "description": "API endpoint '/products' removed",
      "file_path": "openapi.yaml"
    }
  ]
}
```

### Get Configuration

```http
GET /api/v1/guardrails/config
Authorization: Bearer <token>
```

Response:
```json
{
  "schema_first_enabled": true,
  "drift_sentinels_enabled": true,
  "preflight_checks_enabled": true,
  "strict_mode": true
}
```

### Update Configuration

```http
PUT /api/v1/guardrails/config
Authorization: Bearer <token>

{
  "schema_first_enabled": true,
  "drift_sentinels_enabled": true
}
```

## Metrics & Monitoring

### Key Metrics

- `repograph_violations_prevented`: Violations prevented by rule and risk level
- `repograph_preflight_checks`: Preflight check results (passed/failed/blocked)
- `repograph_schema_validations`: Schema validation results
- `repograph_guardrail_check_duration_seconds`: Check performance

### Alerts

Recommended alerts:

1. **Slow Guardrail Checks**: Alert if p95 > 50ms
2. **High Violation Rate**: Alert if violations/hour > threshold
3. **Blocked Operations**: Alert on critical-risk blocks

### Grafana Dashboard

Import the dashboard from `monitoring/dashboards/guardrails-efficiency-dashboard.json`

Key panels:
- Violations by rule
- Preflight check results
- Guardrail check duration
- Schema validation success rate

## Troubleshooting

### Guardrail Checks Timing Out

**Symptom**: Checks taking > 1 second

**Solutions**:
1. Reduce schema extraction scope
2. Cache extracted schemas
3. Disable unused rules
4. Use targeted fetch instead of full re-index

### False Positives

**Symptom**: Valid operations being blocked

**Solutions**:
1. Update schemas to match implementation
2. Use override flag for emergency situations
3. Adjust risk levels for specific rules
4. Whitelist specific files/patterns

### Schema Extraction Failing

**Symptom**: No schemas detected

**Solutions**:
1. Check file patterns match your project
2. Verify schema files are not in .gitignore
3. Add custom extraction patterns
4. Check file permissions

## Best Practices

### Development Workflow

1. **Define schema** → Update OpenAPI/GraphQL/Pydantic schemas
2. **Validate schema** → Run schema gate checks
3. **Generate skeleton** → Create interfaces/stubs from schema
4. **Implement logic** → Write business logic
5. **Preflight check** → Run drift detection
6. **Commit** → Push changes

### CI/CD Integration

```yaml
# .github/workflows/guardrails.yml
- name: Run Guardrail Checks
  run: |
    python -m src.cli guardrails check \
      --operation-type endpoint \
      --targets /api/users \
      --strict
```

### Emergency Override

When you must override guardrails:

```python
# Document why override is needed
result = gate.check_operation(
    operation_type='endpoint',
    targets=['/emergency/endpoint'],
    override=True,  # EMERGENCY: Production hotfix for issue #1234
)
```

## Configuration

### Environment Variables

```bash
# Enable/disable guardrails globally
GUARDRAILS_ENABLED=true

# Strict mode
GUARDRAILS_STRICT_MODE=true

# Schema paths
SCHEMA_PATHS=/schemas:/api-specs

# Cache TTL for schema extraction
SCHEMA_CACHE_TTL=3600
```

### Tenant-Level Configuration

Use feature flags for per-tenant configuration:

```python
# Enable for specific tenant
flags.whitelist_tenant('SCHEMA_FIRST', 'tenant_123')

# Disable for problematic tenant
flags.blacklist_tenant('DRIFT_SENTINELS', 'tenant_456')
```

## Performance

### Target Metrics

- Schema extraction: < 500ms for typical repo
- Validation check: < 50ms per operation
- Preflight check: < 200ms for 10 files

### Optimization Tips

1. Cache extracted schemas
2. Use targeted fetch for drift detection
3. Run expensive checks async
4. Limit scope with file patterns
5. Parallelize independent checks

## Security

### Threat Model

Guardrails protect against:

- Accidental breaking changes
- Schema drift
- Generated file corruption
- Unauthorized route additions
- Type safety violations

### Limitations

Guardrails do NOT protect against:

- Malicious intentional changes
- Logic bugs in implementations
- Runtime data validation issues
- Authentication/authorization flaws

Use guardrails as one layer in defense-in-depth strategy.

## Support

- **Documentation**: `/docs/guardrails-guide.md`
- **API Reference**: `/docs/api-reference.md`
- **Metrics**: Grafana dashboard
- **Issues**: GitHub issues with `guardrails` label
