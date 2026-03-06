# Rollback Runbook

Last Updated: 2026-03-06

## Overview

Use this when a recent code or migration change breaks the active core flow.

## Symptoms

- API startup fails after a deploy or local change
- core tests regress on the index -> search -> scorecard path
- migrations leave the schema out of sync with runtime code

## Detection

```bash
cd packages/aethyme
. .venv/bin/activate
make test-full
```

## Recovery

1. identify the last known good commit
2. revert the breaking change rather than layering a workaround
3. reapply migrations only if the rollback requires it
4. rerun the full core test suite
