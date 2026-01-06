## AI-Powered Features API Documentation

### Overview

RepoGraph Cloud provides AI-powered semantic code search using a **Bring Your Own Key (BYOK)** architecture. Customers provide their own AI API keys from providers like OpenAI, Claude (Anthropic), or Azure OpenAI.

**Benefits of BYOK**:
- **Cost Control**: Customers pay AI providers directly (typically $1-5/month)
- **Privacy**: Your data never goes through our AI infrastructure
- **Flexibility**: Choose your preferred AI provider and model
- **Transparency**: Full visibility into AI usage and costs

---

## Authentication

All AI endpoints require authentication via Bearer token:

```bash
Authorization: Bearer YOUR_ACCESS_TOKEN
```

---

## AI Credentials Management

### Create AI Credentials

Create and store AI provider credentials (encrypted at rest).

**Endpoint**: `POST /api/ai/credentials`

**Request Body**:
```json
{
  "provider_type": "openai",
  "provider_name": "My OpenAI Key",
  "api_key": "sk-...",
  "organization_id": "org-...",  // Optional (OpenAI only)
  "validate": true  // Validate before saving
}
```

**Provider-Specific Fields**:

**OpenAI**:
```json
{
  "provider_type": "openai",
  "api_key": "sk-...",
  "organization_id": "org-..."  // Optional
}
```

**Claude (Anthropic)**:
```json
{
  "provider_type": "claude",
  "api_key": "sk-ant-..."
}
```

**Azure OpenAI**:
```json
{
  "provider_type": "azure_openai",
  "api_key": "...",
  "resource_name": "my-resource",
  "deployment_name": "gpt-4-deployment"
}
```

**Response** (201 Created):
```json
{
  "id": 1,
  "provider_type": "openai",
  "provider_name": "My OpenAI Key",
  "is_active": true,
  "is_validated": true,
  "last_validated_at": "2025-10-04T12:00:00Z",
  "validation_error": null,
  "last_used_at": null,
  "total_requests": 0,
  "total_tokens_used": 0,
  "created_at": "2025-10-04T12:00:00Z",
  "updated_at": null,
  "api_key_preview": "sk-abc123***"
}
```

**Example**:
```bash
curl -X POST https://api.repograph.com/api/ai/credentials \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "provider_type": "openai",
    "api_key": "sk-...",
    "provider_name": "My OpenAI Key",
    "validate": true
  }'
```

---

### List AI Credentials

Get all AI credentials for the current user.

**Endpoint**: `GET /api/ai/credentials`

**Query Parameters**:
- `provider_type` (optional): Filter by provider type

**Response** (200 OK):
```json
[
  {
    "id": 1,
    "provider_type": "openai",
    "provider_name": "My OpenAI Key",
    "is_active": true,
    "is_validated": true,
    "last_validated_at": "2025-10-04T12:00:00Z",
    "api_key_preview": "sk-abc123***",
    ...
  }
]
```

---

### Get Specific Credential

**Endpoint**: `GET /api/ai/credentials/{credential_id}`

**Response** (200 OK): Same as create response

---

### Update Credential

**Endpoint**: `PATCH /api/ai/credentials/{credential_id}`

**Request Body**:
```json
{
  "provider_name": "Updated Name",
  "api_key": "sk-new-key...",  // Optional: rotate key
  "is_active": true
}
```

**Response** (200 OK): Updated credential

---

### Delete Credential

**Endpoint**: `DELETE /api/ai/credentials/{credential_id}`

**Response** (204 No Content)

---

### Validate Credential

Re-validate stored credentials (makes test API call).

**Endpoint**: `POST /api/ai/credentials/{credential_id}/validate`

**Response** (200 OK):
```json
{
  "is_valid": true,
  "error": null,
  "validated_at": "2025-10-04T12:00:00Z"
}
```

---

### Get Supported Providers

**Endpoint**: `GET /api/ai/providers`

**Response** (200 OK):
```json
["openai", "claude", "azure_openai"]
```

---

### Get Supported Models

Get embedding and chat models for a provider.

**Endpoint**: `GET /api/ai/providers/{provider_type}/models`

**Response** (200 OK):
```json
{
  "provider_type": "openai",
  "embedding_models": [
    "text-embedding-3-small",
    "text-embedding-3-large",
    "text-embedding-ada-002"
  ],
  "chat_models": [
    "gpt-4-turbo-preview",
    "gpt-4",
    "gpt-3.5-turbo"
  ]
}
```

---

### Get AI Usage Summary

**Endpoint**: `GET /api/ai/usage`

**Query Parameters**:
- `start_date` (optional): ISO 8601 date (e.g., "2025-10-01T00:00:00Z")
- `end_date` (optional): ISO 8601 date

**Response** (200 OK):
```json
{
  "total_tokens": 123456,
  "total_requests": 789,
  "by_provider": {
    "openai": {
      "tokens": 100000,
      "requests": 500
    },
    "claude": {
      "tokens": 23456,
      "requests": 289
    }
  },
  "period_start": "2025-10-01T00:00:00Z",
  "period_end": "2025-10-04T12:00:00Z"
}
```

---

## Semantic Search

### Natural Language Code Search

Search your codebase using natural language queries.

**Endpoint**: `POST /api/semantic/search`

**Request Body**:
```json
{
  "query": "function that validates email addresses",
  "repository_id": "repo_123",  // Optional: filter by repository
  "language": "python",  // Optional: filter by language
  "symbol_kind": "function",  // Optional: filter by symbol kind
  "limit": 10,
  "provider_type": "openai",  // Optional: default is OpenAI
  "model": "text-embedding-3-small",  // Optional
  "credential_id": 1  // Optional: specific credential
}
```

**Symbol Kinds**:
- `function`
- `class`
- `method`
- `variable`
- `interface`
- `type`

**Response** (200 OK):
```json
{
  "query": "function that validates email addresses",
  "results": [
    {
      "symbol_id": "scip:python::module.file:function.validate_email",
      "symbol_name": "validate_email",
      "symbol_kind": "function",
      "language": "python",
      "file_path": "utils/validators.py",
      "signature": "def validate_email(email: str) -> bool",
      "documentation": "Validates email addresses using regex pattern.",
      "code_snippet": "def validate_email(email: str) -> bool:\n    pattern = r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}$'\n    return bool(re.match(pattern, email))",
      "similarity": 0.89
    },
    {
      "symbol_id": "scip:python::module.file:function.check_email",
      "symbol_name": "check_email",
      "symbol_kind": "function",
      "language": "python",
      "file_path": "auth/validators.py",
      "signature": "def check_email(email: str) -> bool",
      "documentation": "Check if email format is valid.",
      "code_snippet": "def check_email(email: str) -> bool:\n    ...",
      "similarity": 0.76
    }
  ],
  "total": 2,
  "model_used": "text-embedding-3-small",
  "provider_used": "openai"
}
```

**Example**:
```bash
curl -X POST https://api.repograph.com/api/semantic/search \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "function that validates email addresses",
    "repository_id": "repo_123",
    "limit": 10
  }'
```

**Example Queries**:
- "function that validates email addresses"
- "authentication middleware with JWT"
- "parse JSON configuration file"
- "algorithm to sort an array"
- "HTTP request handler for user login"
- "database connection pool manager"
- "cache implementation with expiration"

---

### Generate Embeddings for Repository

Generate vector embeddings for all code in a repository (background task).

**Endpoint**: `POST /api/semantic/embeddings/generate`

**Request Body**:
```json
{
  "repository_id": "repo_123",
  "provider_type": "openai",
  "model": "text-embedding-3-small",
  "credential_id": 1  // Optional
}
```

**Response** (202 Accepted):
```json
{
  "repository_id": "repo_123",
  "embeddings_created": 0,
  "model_used": "text-embedding-3-small",
  "provider_used": "openai",
  "status": "accepted"
}
```

**Notes**:
- Returns immediately (202 Accepted)
- Embeddings generated in background
- Check embedding stats to see progress
- Typically takes 1-5 minutes for medium codebase

---

### Delete Repository Embeddings

Delete all embeddings for a repository.

**Endpoint**: `DELETE /api/semantic/embeddings/repository/{repository_id}`

**Response** (204 No Content)

---

### Delete File Embeddings

Delete embeddings for a specific file.

**Endpoint**: `DELETE /api/semantic/embeddings/file`

**Query Parameters**:
- `repository_id`: Repository ID
- `file_path`: File path

**Response** (204 No Content)

---

### Get Embedding Statistics

**Endpoint**: `GET /api/semantic/embeddings/stats`

**Query Parameters**:
- `repository_id` (optional): Filter by repository

**Response** (200 OK):
```json
{
  "total_embeddings": 1543,
  "by_language": {
    "python": 890,
    "javascript": 450,
    "typescript": 203
  },
  "by_symbol_kind": {
    "function": 980,
    "class": 340,
    "method": 223
  },
  "by_provider": {
    "openai": 1543
  }
}
```

---

### Get Example Queries

Get example natural language queries for semantic search.

**Endpoint**: `GET /api/semantic/examples`

**Response** (200 OK):
```json
[
  "function that validates email addresses",
  "authentication middleware with JWT",
  "parse JSON configuration file",
  ...
]
```

---

## Cost Estimation

### OpenAI Pricing

**Embeddings** (text-embedding-3-small):
- $0.020 per 1M tokens
- Medium codebase (50K symbols): ~$0.10-0.50 one-time
- Incremental updates: ~$0.01 per update

**Semantic Search**:
- $0.020 per 1M tokens
- ~$0.0001 per query
- 1,000 searches/month: ~$0.10

### Claude Pricing

**Chat** (Claude 3 Haiku):
- Input: $0.25 per 1M tokens
- Output: $1.25 per 1M tokens

**Note**: Claude doesn't provide embeddings. Use OpenAI for embeddings, Claude for chat.

### Azure OpenAI Pricing

Same as OpenAI, but billed through Azure account.

### Typical Customer Costs

- **Embedding generation**: $0.10-0.50 one-time per repository
- **Semantic search**: $0.10 per 1,000 queries
- **Total monthly cost**: $1-5 for typical usage

---

## Error Handling

### Error Responses

**Invalid API Key** (400 Bad Request):
```json
{
  "detail": "Invalid OpenAI API key"
}
```

**Rate Limit** (429 Too Many Requests):
```json
{
  "detail": "OpenAI rate limit exceeded",
  "retry_after": 60
}
```

**Quota Exceeded** (402 Payment Required):
```json
{
  "detail": "OpenAI quota exceeded - customer needs to add credits"
}
```

**No Credentials Found** (400 Bad Request):
```json
{
  "detail": "No active openai credentials found for user 123. Please add your API key in settings."
}
```

---

## Webhooks Integration

Embeddings are automatically updated when code changes via webhooks:

1. **Code Push**: GitHub/GitLab webhook triggered
2. **Incremental Indexing**: Changed files re-indexed in Elasticsearch
3. **Embedding Update** (automatic):
   - Delete embeddings for changed files
   - Generate new embeddings for updated symbols
   - Delete embeddings for removed files

**No manual intervention needed** - embeddings stay in sync with code.

---

## Security

### API Key Storage

- API keys encrypted at rest using Fernet (AES-128)
- Unique encryption key per environment (`ENCRYPTION_KEY` env var)
- Keys never logged or exposed in API responses
- Only masked previews shown: `"sk-abc123***"`

### API Key Validation

- Test API call before storing new credentials
- Re-validation on demand
- Validation errors stored for debugging
- Invalid keys automatically deactivated

### Authorization

- Users can only access their own credentials
- JWT token required for all endpoints
- No cross-user credential access

---

## Rate Limits

### RepoGraph API Limits

- Standard API rate limits apply (per user)
- No additional limits for AI endpoints

### AI Provider Limits

**OpenAI**:
- Free tier: 3 RPM, 200 RPD
- Tier 1: 500 RPM, 10,000 RPD
- Tier 2: 5,000 RPM, 100,000 RPD

**Claude**:
- Default: 50 RPM
- Higher tiers available

**Handling**:
- Automatic retry with exponential backoff
- Rate limit errors include `retry_after` header
- Background tasks respect provider limits

---

## Best Practices

### 1. Choose the Right Model

**For Embeddings**:
- `text-embedding-3-small`: Best value ($0.02/1M tokens, 1536 dims)
- `text-embedding-3-large`: Better accuracy ($0.13/1M tokens, 3072 dims)

**For Chat**:
- Claude Haiku: Fastest, cheapest
- GPT-4 Turbo: Best quality
- GPT-3.5 Turbo: Good balance

### 2. Optimize Costs

- Generate embeddings once per repository
- Use incremental updates (webhooks)
- Use `text-embedding-3-small` for most use cases
- Cache search results on client side

### 3. Query Tips

**Good queries** (specific, descriptive):
- ✅ "function that validates email addresses using regex"
- ✅ "JWT authentication middleware for Express"
- ✅ "async function to fetch user data from API"

**Poor queries** (too vague):
- ❌ "email"
- ❌ "function"
- ❌ "code"

### 4. Filter Results

Use filters to improve relevance:
- `language`: Limit to specific programming language
- `symbol_kind`: Limit to functions, classes, etc.
- `repository_id`: Search within specific repository

### 5. Monitor Usage

- Check `GET /api/ai/usage` regularly
- Set up quotas to prevent runaway costs
- Re-validate credentials monthly

---

## Migration Guide

### From Existing Search to Semantic Search

**Step 1**: Add AI credentials
```bash
POST /api/ai/credentials
{
  "provider_type": "openai",
  "api_key": "sk-..."
}
```

**Step 2**: Generate embeddings
```bash
POST /api/semantic/embeddings/generate
{
  "repository_id": "repo_123"
}
```

**Step 3**: Wait for completion (1-5 minutes)
```bash
GET /api/semantic/embeddings/stats?repository_id=repo_123
```

**Step 4**: Start searching
```bash
POST /api/semantic/search
{
  "query": "your natural language query",
  "repository_id": "repo_123"
}
```

---

## Support

### Getting AI API Keys

**OpenAI**: https://platform.openai.com/api-keys
**Claude**: https://console.anthropic.com/
**Azure OpenAI**: https://azure.microsoft.com/en-us/products/ai-services/openai-service

### Troubleshooting

**"Invalid API key"**:
- Verify key is correct
- Check if key has required permissions
- Ensure key is not expired

**"Rate limit exceeded"**:
- Wait for `retry_after` seconds
- Upgrade AI provider tier
- Reduce request frequency

**"Quota exceeded"**:
- Add credits to AI provider account
- Check billing status

**"No embeddings found"**:
- Generate embeddings first: `POST /api/semantic/embeddings/generate`
- Wait for background task to complete
- Check embedding stats

---

## Changelog

### v1.0.0 (2025-10-04)

- Initial release
- BYOK architecture
- OpenAI, Claude, Azure OpenAI support
- Natural language semantic search
- Automatic webhook integration
- Usage tracking and quotas
