# Aethyme Efficiency Guide

## Overview

The Aethyme Efficiency system optimizes AI operations through intelligent model routing, context management, and cost tracking. This guide covers all efficiency features and optimization strategies.

## Features

### 1. Model Routing

Intelligently route requests to appropriate models based on task complexity, budget, and quality requirements.

#### Model Tiers

| Tier | Models | Use Cases | Cost | Speed |
|------|--------|-----------|------|-------|
| **Fast** | GPT-3.5-turbo, Claude Haiku | Simple tasks, typo fixes, formatting | $$ | ⚡⚡⚡ |
| **Balanced** | GPT-4-turbo, Claude Sonnet | Typical coding tasks, documentation | $$$ | ⚡⚡ |
| **Powerful** | GPT-4, Claude Opus | Complex algorithms, architecture | $$$$ | ⚡ |

#### Routing Strategies

**Cheapest**: Always use fastest, cheapest model
```python
router = ModelRouter(strategy=RoutingStrategy.CHEAPEST)
```

**Best Quality**: Always use most powerful model
```python
router = ModelRouter(strategy=RoutingStrategy.BEST_QUALITY)
```

**Adaptive** (Recommended): Analyze task complexity
```python
router = ModelRouter(strategy=RoutingStrategy.ADAPTIVE)

decision = router.route(
    task_description="Fix typo in documentation",
    estimated_input_tokens=500,
    estimated_output_tokens=100,
)
# → Routes to Fast tier
```

**Budget Aware**: Consider remaining budget
```python
router = ModelRouter(
    strategy=RoutingStrategy.BUDGET_AWARE,
    budget_per_day=100.00,  # $100/day limit
)
```

#### Escalation

Automatically escalate on failure:

```python
from src.efficiency.model_router import EscalationConfig

router = ModelRouter(
    escalation_config=EscalationConfig(
        max_retries=3,
        escalate_on_error=True,
        escalate_on_quality=True,
        quality_threshold=0.7,
    ),
)

# If fast model fails, escalates to balanced
decision = router.handle_failure(
    model_id="gpt-3.5-turbo",
    error="Rate limit exceeded",
    attempt=1,
)
```

### 2. Context Management

Optimize token usage through compaction, working memory, and organization.

#### Context Compaction

Reduce context size while preserving information:

```python
from src.efficiency.context import ContextCompactor, ContextItem

compactor = ContextCompactor(aggressive=False)

items = [/* context items */]
compacted, stats = compactor.compact(items)

print(f"Saved {stats['tokens_saved']} tokens ({stats['reduction_percent']:.1f}%)")
```

**Compaction Techniques**:
- Remove duplicate items
- Deduplicate imports/headers
- Truncate low-priority items
- Remove comments (aggressive mode)

**Results**: Typically 20-40% token reduction

#### Working Memory Slots

Limit context size with bounded slots:

```python
from src.efficiency.context import ContextManager

manager = ContextManager(max_total_tokens=100000)

# Create slots for different contexts
manager.create_slot("code_context", max_tokens=50000)
manager.create_slot("docs_context", max_tokens=30000)
manager.create_slot("test_context", max_tokens=20000)

# Add items (auto-compaction on overflow)
item = ContextItem(
    item_id="user_model",
    item_type=ContextType.CODE,
    content=file_content,
    priority=Priority.HIGH,
)
manager.add_to_slot("code_context", item)
```

**LRU Eviction**: Least-recently-used items evicted when full

#### Context Playlists

Organize context for tasks:

```python
playlist = manager.create_playlist(
    task_description="Add user authentication",
    context_items={
        "Models": [user_model, auth_model],
        "Routes": [auth_routes],
        "Tests": [auth_tests],
        "Config": [config_file],
    },
)

# Render for LLM
markdown = playlist.render("markdown")
json_output = playlist.render("json")
```

#### Outcome Cards

Summarize task results:

```python
card = manager.create_outcome_card(
    task_description="Implement OAuth login",
    status="success",
    summary="Added OAuth login with Google and GitHub providers",
    key_decisions=[
        "Use Passport.js for OAuth abstraction",
        "Store tokens in httpOnly cookies",
    ],
    artifacts_created=[
        "src/auth/oauth.ts",
        "tests/auth/oauth.test.ts",
    ],
    tokens_used=15000,
    cost=0.45,
    duration_seconds=45.2,
)

# Share with team
print(card.render("markdown"))
```

### 3. Token & Cost Tracking

Track usage and enforce budgets.

#### Token Tracking

```python
from src.efficiency.tracking import TokenTracker

tracker = TokenTracker()

usage = tracker.record_usage(
    operation_id="task_123",
    operation_type="code_generation",
    model_id="gpt-4-turbo",
    input_tokens=5000,
    output_tokens=2000,
    tenant_id="tenant_1",
    user_id="user_42",
)

# Get totals
total_tokens = tracker.get_total_tokens(tenant_id="tenant_1")
print(f"Total tokens: {total_tokens:,}")
```

#### Cost Tracking

```python
from src.efficiency.tracking import CostTracker

tracker = CostTracker()

record = tracker.record_cost(
    operation_id="task_123",
    operation_type="code_generation",
    model_id="gpt-4-turbo",
    cost_usd=0.14,
    input_tokens=5000,
    output_tokens=2000,
    tenant_id="tenant_1",
    user_id="user_42",
)

# Get cost by time window
from datetime import datetime, timedelta

daily_cost = tracker.get_total_cost(
    tenant_id="tenant_1",
    start_time=datetime.utcnow() - timedelta(days=1),
)
```

#### Budget Enforcement

```python
from src.efficiency.tracking import BudgetEnforcer, Budget, TimeWindow

enforcer = BudgetEnforcer()

# Set daily budget
budget = Budget(
    budget_id="daily_budget",
    tenant_id="tenant_1",
    limit_usd=50.00,
    window=TimeWindow.DAILY,
    alert_threshold=0.8,  # Alert at 80%
    hard_limit=True,  # Block when exceeded
)
enforcer.set_budget(budget)

# Check before operation
result = enforcer.check_budget(
    tenant_id="tenant_1",
    estimated_cost=2.50,
)

if not result['allowed']:
    print(f"Budget exceeded: {result['reason']}")
    # Use cheaper model or defer operation
```

### 4. Usage Statistics

Calculate comprehensive usage stats:

```python
from src.efficiency.tracking import calculate_usage_stats
from datetime import datetime, timedelta

now = datetime.utcnow()
start = now - timedelta(days=7)

stats = calculate_usage_stats(
    token_records=tracker.usage_records,
    cost_records=cost_tracker.cost_records,
    window=TimeWindow.WEEKLY,
    start_time=start,
    end_time=now,
)

print(f"Total tokens: {stats.total_tokens:,}")
print(f"Total cost: ${stats.total_cost:.2f}")
print(f"Avg tokens/op: {stats.to_dict()['avg_tokens_per_op']:.0f}")

# By model
for model, data in stats.by_model.items():
    print(f"{model}: {data['tokens']:,} tokens, ${data['cost']:.2f}")
```

## API Endpoints

### Get Efficiency Stats

```http
GET /api/v1/guardrails/efficiency/stats?window=daily
Authorization: Bearer <token>
```

Response:
```json
{
  "window": "daily",
  "start_time": "2025-11-21T00:00:00Z",
  "end_time": "2025-11-22T00:00:00Z",
  "total_tokens": 150000,
  "total_cost": 2.45,
  "operations_count": 42,
  "avg_tokens_per_op": 3571.4,
  "avg_cost_per_op": 0.058,
  "by_model": {
    "gpt-3.5-turbo": {"tokens": 50000, "cost": 0.50},
    "claude-3-sonnet": {"tokens": 100000, "cost": 1.95}
  }
}
```

### Get Budget Status

```http
GET /api/v1/guardrails/efficiency/budget
Authorization: Bearer <token>
```

Response:
```json
{
  "budgets": [
    {
      "budget_id": "daily_budget",
      "limit_usd": 50.00,
      "current_spend": 12.45,
      "remaining": 37.55,
      "utilization": 0.249,
      "window": "daily"
    }
  ],
  "alerts": []
}
```

### Create Budget

```http
POST /api/v1/guardrails/efficiency/budget
Authorization: Bearer <token>

{
  "limit_usd": 100.00,
  "window": "daily",
  "alert_threshold": 0.8,
  "hard_limit": true
}
```

## Optimization Strategies

### Task Classification

Classify tasks for appropriate routing:

| Task Type | Tier | Example |
|-----------|------|---------|
| Typo/formatting fixes | Fast | "Fix typo in variable name" |
| Simple refactoring | Fast | "Rename function for clarity" |
| Standard CRUD | Balanced | "Add user update endpoint" |
| Business logic | Balanced | "Implement discount calculation" |
| Complex algorithms | Powerful | "Optimize query performance" |
| Architecture design | Powerful | "Design microservices architecture" |

### Token Optimization

#### 1. Pre-compaction
Compact context before sending to model:

```python
# Before
tokens_before = sum(item.token_count for item in items)

# Compact
compacted, stats = compactor.compact(items)

# After
tokens_after = stats['compacted_tokens']
savings = stats['tokens_saved']
```

#### 2. Selective Context
Only include relevant context:

```python
# ❌ Bad: Include everything
context = all_files_in_repo

# ✅ Good: Include only related files
context = [
    affected_file,
    related_model,
    relevant_test,
    api_contract,
]
```

#### 3. Streaming Responses
Stream output to start processing sooner:

```python
# Benefits:
# - Lower latency to first token
# - Can abort early if going wrong
# - Better user experience
```

### Cost Optimization

#### 1. Tier Selection Matrix

```python
def select_tier(task_description: str) -> ModelTier:
    """Select appropriate tier based on task."""
    desc = task_description.lower()

    # Fast tier indicators
    if any(word in desc for word in ['typo', 'format', 'rename', 'simple']):
        return ModelTier.FAST

    # Powerful tier indicators
    if any(word in desc for word in ['complex', 'algorithm', 'optimize', 'architecture']):
        return ModelTier.POWERFUL

    # Default to balanced
    return ModelTier.BALANCED
```

#### 2. Budget Allocation

```python
# Allocate budget by priority
budgets = {
    "production_fixes": Budget(limit_usd=200, window=TimeWindow.DAILY),
    "feature_development": Budget(limit_usd=100, window=TimeWindow.DAILY),
    "documentation": Budget(limit_usd=50, window=TimeWindow.DAILY),
    "experiments": Budget(limit_usd=20, window=TimeWindow.DAILY),
}
```

#### 3. Batch Operations

```python
# ❌ Bad: One API call per task
for task in tasks:
    result = process_task(task)

# ✅ Good: Batch related tasks
batched_tasks = batch_by_context(tasks, max_batch_size=10)
for batch in batched_tasks:
    results = process_batch(batch)  # Single API call
```

## Metrics & Monitoring

### Key Metrics

- `aethyme_tokens_total`: Total tokens by operation/model/tenant
- `aethyme_cost_total`: Total cost by tenant/model/operation
- `aethyme_model_escalations`: Escalation events
- `aethyme_context_compaction_ratio`: Compaction effectiveness
- `aethyme_budget_utilization`: Budget usage percentage

### Dashboards

Import from `monitoring/dashboards/guardrails-efficiency-dashboard.json`

**Key Panels**:
- Token usage by model
- Cost by tenant
- Compaction ratio distribution
- Budget utilization gauges
- Model requests by tier

### Alerts

**Recommended Alerts**:

1. **High Cost**: Daily cost > $X
2. **Budget Alert**: Utilization > 80%
3. **Budget Exceeded**: Hard limit reached
4. **Frequent Escalations**: Escalations/hour > threshold
5. **Poor Compaction**: Compaction ratio < 10%

## Best Practices

### 1. Start Conservative

```python
# Begin with balanced tier and adapt
router = ModelRouter(strategy=RoutingStrategy.BALANCED)

# Monitor and adjust based on:
# - Task success rates
# - Cost per task
# - Quality metrics
```

### 2. Set Budgets Early

```python
# Set budgets before scaling
enforcer.set_budget(Budget(
    budget_id="initial",
    limit_usd=10.00,  # Start low
    window=TimeWindow.DAILY,
    hard_limit=True,
))

# Gradually increase based on value
```

### 3. Monitor Compaction

```python
# Track compaction effectiveness
compacted, stats = compactor.compact(items)

if stats['reduction_percent'] < 10:
    # Compaction not effective
    # Context may already be minimal
    pass
elif stats['reduction_percent'] > 60:
    # Very effective
    # Consider aggressive mode
    compactor = ContextCompactor(aggressive=True)
```

### 4. Use Outcome Cards

```python
# Document all significant operations
card = manager.create_outcome_card(
    task_description=task,
    status="success",
    summary=summary,
    tokens_used=tokens,
    cost=cost,
)

# Share for visibility
send_to_slack(card.render("markdown"))
save_to_database(card.to_dict())
```

### 5. Regular Review

Weekly review of:
- Cost per operation type
- Model tier distribution
- Escalation frequency
- Budget utilization
- Compaction effectiveness

## Performance Targets

| Metric | Target | Critical |
|--------|--------|----------|
| Compaction ratio | > 20% | < 10% |
| Routing latency | < 10ms | > 100ms |
| Budget utilization | 70-90% | > 95% |
| Escalation rate | < 5% | > 20% |
| Cost per task | Varies | 2x baseline |

## Troubleshooting

### High Costs

**Symptoms**:
- Costs exceeding budget
- Frequent budget alerts

**Solutions**:
1. Review tier distribution - too many powerful tier?
2. Check for context bloat - run compaction
3. Batch similar operations
4. Use cheaper models for simple tasks
5. Set per-operation budgets

### Poor Compaction

**Symptoms**:
- Compaction ratio < 10%
- No token savings

**Solutions**:
1. Context already minimal - expected
2. Enable aggressive mode if needed
3. Remove low-priority items manually
4. Use working memory slots

### Frequent Escalations

**Symptoms**:
- Many fast → balanced escalations
- Escalation rate > 10%

**Solutions**:
1. Review task classification
2. Start with balanced tier
3. Improve task descriptions
4. Check for systemic model issues

## Configuration

### Environment Variables

```bash
# Model routing
MODEL_ROUTER_STRATEGY=adaptive
DEFAULT_BUDGET_PER_DAY=100.00

# Context management
MAX_CONTEXT_TOKENS=100000
COMPACTION_ENABLED=true
AGGRESSIVE_COMPACTION=false

# Tracking
TOKEN_TRACKING_ENABLED=true
COST_TRACKING_ENABLED=true
```

### Per-Tenant Configuration

```python
# Custom budgets per tenant
tenant_budgets = {
    "enterprise_1": 1000.00,
    "startup_1": 100.00,
    "trial_1": 10.00,
}

for tenant_id, limit in tenant_budgets.items():
    enforcer.set_budget(Budget(
        budget_id=f"{tenant_id}_daily",
        tenant_id=tenant_id,
        limit_usd=limit,
        window=TimeWindow.DAILY,
    ))
```

## Advanced Features

### Custom Model Tiers

```python
# Add custom local model
custom_model = ModelConfig(
    model_id="local-llama-70b",
    provider=ModelProvider.LOCAL,
    tier=ModelTier.BALANCED,
    cost_per_1k_input=0.0,  # Free!
    cost_per_1k_output=0.0,
    max_tokens=4096,
    context_window=8192,
)

router.add_model(custom_model)
```

### Quality-Based Escalation

```python
# Escalate if quality below threshold
config = EscalationConfig(
    escalate_on_quality=True,
    quality_threshold=0.7,  # 70% quality required
)

# After each operation, assess quality
quality_score = assess_output(result)
if quality_score < 0.7:
    # Automatically retry with better model
    router.handle_failure(model_id, "Low quality", attempt)
```

## Support

- **Documentation**: `/docs/efficiency-guide.md`
- **API Reference**: `/docs/api-reference.md`
- **Metrics**: Grafana dashboard
- **Cost Analysis**: Weekly cost reports
- **Issues**: GitHub issues with `efficiency` label
