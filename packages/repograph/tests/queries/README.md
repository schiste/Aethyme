# RepoGraph Query Tests

Contract tests for RepoGraph query operations (Sprint 1 Task S1-T3).

## Overview

This test suite validates the performance, correctness, and security of RepoGraph's core query operations:
- **Search**: Symbol search with exact, fuzzy, and hybrid matching
- **Ego Graph**: Relationship network traversal
- **Impact Analysis**: Reverse dependency analysis

## Test Files

### `fixtures/graph_data.sql`
Sample graph data for testing:
- 107 realistic code symbols (classes, functions, methods, models, API routes)
- 500+ dependency edges (imports, invokes, contains, inherits)
- 2 separate tenants (org1, org2) for RLS testing
- Realistic e-commerce application structure

### `test_search.py`
Search query contract tests (20+ test cases):
- Exact match search
- Fuzzy search (trigram similarity)
- Hybrid search (FTS + fuzzy)
- Filters (kind, language)
- Pagination and limits
- RLS isolation
- Performance benchmarks

### `test_ego.py`
Ego graph contract tests (15+ test cases):
- Graph traversal at depths 1, 2, 3
- Symbol not found (404 errors)
- Circular dependency handling
- Large graph performance
- RLS isolation

### `test_impact.py`
Impact analysis contract tests (18+ test cases):
- Direct and transitive dependencies
- Max depth limits
- Reverse dependency traversal
- Performance with deep graphs
- RLS isolation

## Running Tests

### Prerequisites
```bash
# Install dependencies
pip install pytest pytest-asyncio pytest-benchmark

# Set up database with fixtures
psql -U repograph -d repograph -f fixtures/graph_data.sql
```

### Run All Tests
```bash
# From packages/repograph directory
pytest tests/queries/ -v
```

### Run Specific Test Files
```bash
pytest tests/queries/test_search.py -v
pytest tests/queries/test_ego.py -v
pytest tests/queries/test_impact.py -v
```

### Run with Coverage
```bash
pytest tests/queries/ --cov=src/queries --cov=src/cache --cov-report=html
```

### Run Performance Benchmarks
```bash
pytest tests/queries/test_search.py::TestSearchPerformance -v --benchmark-only
```

## Test Organization

### Test Classes

Each test file is organized by functionality:

**test_search.py:**
- `TestSearchExact` - Exact symbol matching
- `TestSearchFuzzy` - Fuzzy/trigram search
- `TestSearchHybrid` - Combined FTS + fuzzy
- `TestSearchFilters` - Filter by kind, language
- `TestSearchPagination` - Limit and pagination
- `TestSearchRLS` - Row-level security
- `TestSearchPerformance` - Performance benchmarks

**test_ego.py:**
- `TestEgoGraphBasic` - Basic traversal at various depths
- `TestEgoGraphErrors` - Error handling and edge cases
- `TestEgoGraphCircular` - Circular dependency handling
- `TestEgoGraphLarge` - Large graph performance
- `TestEgoGraphDepthLevels` - Depth organization validation
- `TestEgoGraphRLS` - Row-level security

**test_impact.py:**
- `TestImpactAnalysisBasic` - Basic impact queries
- `TestImpactAnalysisTransitive` - Transitive dependencies
- `TestImpactAnalysisDepthLimits` - Max depth constraints
- `TestImpactAnalysisReverseImpact` - Reverse dependency analysis
- `TestImpactAnalysisPerformance` - Performance benchmarks
- `TestImpactAnalysisErrors` - Error handling
- `TestImpactAnalysisCircular` - Circular dependency handling
- `TestImpactAnalysisRLS` - Row-level security
- `TestImpactAnalysisDepthOrganization` - Depth level validation

## Test Data

### Tenants
- **org1** (UUID: `00000000-0000-0000-0000-000000000001`)
  - test_repo1: Realistic e-commerce application
  - 107 symbols: UserService, ProductService, OrderService, models, APIs, utilities
  - 500+ edges: imports, invokes, contains, inherits

- **org2** (UUID: `00000000-0000-0000-0000-000000000002`)
  - test_repo2: Blog application
  - 4 symbols: BlogService, create_post, get_posts, API routes

### Symbol Examples
```python
# Services
'services/user.py:UserService'
'services/product.py:ProductService'
'services/order.py:OrderService'
'services/payment.py:PaymentService'

# Models
'models/user.py:User'
'models/product.py:Product'
'models/order.py:Order'

# API Routes
'api/routes/users.py:create_user'
'api/routes/products.py:search_products'
'api/routes/orders.py:create_order'

# Utilities
'utils/validators.py:validate_email'
'utils/crypto.py:hash_password'
```

## Performance Targets

### Search Queries
- **Without Cache**: p95 < 500ms
- **With Cache**: p95 < 50ms
- **Throughput**: >100 queries/second (no cache)

### Ego Graph Queries
- **Depth 1**: p95 < 500ms
- **Depth 2**: p95 < 1s
- **Depth 3**: p95 < 2s

### Impact Analysis
- **Depth 5**: p95 < 1s
- **Depth 10**: p95 < 2s

## RLS Testing

All test classes include RLS (Row-Level Security) validation:

```python
def test_tenant_isolation():
    """Test org1 can only see org1 data."""
    tenant1_store = GraphStore(tenant_id="00000000-0000-0000-0000-000000000001")

    # Should find org1 symbols
    results = tenant1_store.search("UserService")
    assert len(results) > 0

    # Should NOT find org2 symbols
    results = tenant1_store.search("BlogService")
    assert len(results) == 0
```

## Troubleshooting

### Tests Fail with "Relation does not exist"
Load test fixtures:
```bash
psql -U repograph -d repograph -f fixtures/graph_data.sql
```

### Tests Timeout
Increase pytest timeout:
```bash
pytest tests/queries/ --timeout=300
```

### RLS Tests Fail
Verify RLS policies are enabled:
```sql
SELECT tablename, policyname, permissive, roles, cmd, qual
FROM pg_policies
WHERE schemaname = 'repograph';
```

### Performance Tests Fail
1. Check database indexes:
   ```sql
   \di repograph.*
   ```

2. Run ANALYZE to update statistics:
   ```sql
   ANALYZE repograph.nodes;
   ANALYZE repograph.edges;
   ```

3. Check connection pool settings in test environment

## CI/CD Integration

### GitHub Actions
```yaml
- name: Run Query Tests
  run: |
    pytest tests/queries/ -v --junitxml=junit.xml

- name: Upload Coverage
  uses: codecov/codecov-action@v2
  with:
    files: ./coverage.xml
    flags: query-tests
```

### Pre-commit Hooks
```yaml
- repo: local
  hooks:
    - id: query-tests
      name: Query Tests
      entry: pytest tests/queries/ -v
      language: system
      pass_filenames: false
```

## Contributing

### Adding New Tests

1. **Choose the appropriate test file:**
   - Search tests → `test_search.py`
   - Ego graph tests → `test_ego.py`
   - Impact analysis tests → `test_impact.py`

2. **Follow the naming convention:**
   - Test classes: `TestFeatureName`
   - Test methods: `test_specific_behavior`

3. **Use fixtures:**
   ```python
   def test_my_feature(test_tenant_id):
       store = GraphStore(tenant_id=test_tenant_id)
       # Your test code
   ```

4. **Add assertions:**
   - Test positive cases
   - Test negative cases (errors, not found)
   - Test edge cases (empty, large, circular)
   - Test RLS isolation

5. **Document the test:**
   ```python
   def test_my_feature(test_tenant_id):
       """Test that my feature works correctly.

       Verifies that the feature:
       - Does X when Y
       - Returns Z when A
       - Handles error condition B
       """
   ```

### Adding Test Data

To add new test symbols or edges:

1. Edit `fixtures/graph_data.sql`
2. Follow the existing patterns:
   ```sql
   -- Add nodes
   INSERT INTO repograph.nodes (id, tenant_id, repository_id, symbol, ...)
   VALUES ('n999', '00000000-0000-0000-0000-000000000001', ...);

   -- Add edges
   INSERT INTO repograph.edges (id, tenant_id, repository_id, from_node_id, to_node_id, ...)
   VALUES ('e999', '00000000-0000-0000-0000-000000000001', 'n001', 'n999', ...);
   ```
3. Reload fixtures:
   ```bash
   psql -U repograph -d repograph -f fixtures/graph_data.sql
   ```

## References

- [ROADMAP.md](../../ROADMAP.md) - Sprint 1 Task S1-T3 details
- [API Reference](../../docs/reference/api.md) - Query API documentation
- [Implementation Summary](../../benchmarks/results/IMPLEMENTATION_SUMMARY.md) - Performance results
- [GraphStore](../../src/graph/store.py) - Query implementation
