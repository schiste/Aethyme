# Emergency Rollback Runbook

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
kubectl patch service repograph -n production \
  -p '{"spec":{"selector":{"version":"blue"}}}'

# Verify
curl https://api.repograph.io/health/detailed
```

### Using Helm Rollback
```bash
# List releases
helm history repograph -n production

# Rollback to previous
helm rollback repograph -n production

# Rollback to specific revision
helm rollback repograph 5 -n production
```

### Using kubectl
```bash
# Rollback deployment
kubectl rollout undo deployment/repograph -n production

# Watch status
kubectl rollout status deployment/repograph -n production
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
