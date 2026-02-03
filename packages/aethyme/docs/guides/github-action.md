# GitHub Action Guide

Guide for using the Aethyme Scorecard GitHub Action.

## Setup

Add the action to your workflow:

```yaml
name: Aethyme Scorecard
on: [pull_request]

jobs:
  scorecard:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/aethyme-scorecard
        with:
          api-key: \${{ secrets.AETHYME_API_KEY }}
          org-id: \${{ secrets.AETHYME_ORG_ID }}
```

## Inputs

- `api-key` (required) - Aethyme API key
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

See .github/workflows/aethyme-example.yml for complete examples.
