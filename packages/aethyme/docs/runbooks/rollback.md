# Emergency Rollback Runbook

## Overview

Emergency rollback procedure for reverting a bad deployment or configuration change in Aethyme.

## Symptoms

- Error rate spikes after deployment
- User-facing failures or degraded latency
- Data corruption or severe indexing regressions
- Security regression introduced by the latest release

## Diagnostic

- Confirm the deployment or configuration change correlated with the incident
- Check health probes, error rates, and rollback targets
- Identify the last known-good release or revision before acting

## When to Rollback

Initiate rollback immediately if:
- Error rate >5% for 10 minutes
- P0 incident with customer impact
- Data corruption detected
- Security vulnerability introduced

## Quick Rollback (5 minutes)

### Using Blue-Green Deployment
```bash
# If green is bad, switch back to blue
kubectl patch service aethyme -n production \
  -p '{"spec":{"selector":{"version":"blue"}}}'

# Verify
curl https://api.aethyme.com/health/detailed
```

### Using Helm Rollback
```bash
# List releases
helm history aethyme -n production

# Rollback to previous
helm rollback aethyme -n production

# Rollback to specific revision
helm rollback aethyme 5 -n production
```

### Using kubectl
```bash
# Rollback deployment
kubectl rollout undo deployment/aethyme -n production

# Watch status
kubectl rollout status deployment/aethyme -n production
```

## Verification

- [ ] Health probes passing
- [ ] Error rate <1%
- [ ] Latency p95 <2s
- [ ] No pod restarts

## Post-Rollback

1. Create incident post-mortem
2. Identify root cause
3. Add regression tests
4. Plan forward fix

**Last Updated:** 2024-11-22
