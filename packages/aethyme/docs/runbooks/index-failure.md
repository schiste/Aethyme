# Index Failure Runbook

Last Updated: 2026-03-06

## Overview

Use this when repository indexing fails or stalls.

## Symptoms

- repository status remains `failed` or `indexing`
- index freshness does not improve
- graph queries return no results for a recently indexed repository

## Detection

```bash
curl -s http://localhost:8001/api/v1/index/status/<repo-id> \
  -H "Authorization: Bearer $TOKEN"
```

## Recovery

1. verify the repository path exists and is readable
2. rerun indexing with `use_fallback=true`
3. inspect API logs for parser or database failures
4. rerun the relevant indexing tests
