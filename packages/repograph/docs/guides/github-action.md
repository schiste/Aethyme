# GitHub Action Guide

Guide for using the RepoGraph Scorecard GitHub Action.

## Setup

Add the action to your workflow:

```yaml
name: RepoGraph Scorecard
on: [pull_request]

jobs:
  scorecard:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/repograph-scorecard
        with:
          api-key: \${{ secrets.REPOGRAPH_API_KEY }}
          org-id: \${{ secrets.REPOGRAPH_ORG_ID }}
```

## Inputs

- `api-key` (required) - RepoGraph API key
- `org-id` (required) - Organization ID
- `repo-path` - Repository path (default: .)
- `apply-fixes` - Apply autofixes (default: false)
- `fail-on-blockers` - Fail on blockers (default: true)
- `min-score` - Minimum score (default: 70)
- `create-pr` - Create PR with fixes (default: false)

## Outputs

- `score` - Overall score (0-100)
- `blockers` - Number of blockers
- `warnings` - Number of warnings
- `info` - Number of info items
- `report-path` - Path to report file

## Examples

See .github/workflows/repograph-example.yml for complete examples.
