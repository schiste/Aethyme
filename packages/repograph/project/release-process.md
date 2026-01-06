# RepoGraph Release Process

This document outlines the release process for RepoGraph, covering sprint releases, hotfixes, and production deployments.

## Table of Contents

- [Release Types](#release-types)
- [Sprint Release Process](#sprint-release-process)
- [Hotfix Process](#hotfix-process)
- [Rollback Plan](#rollback-plan)
- [Version Numbering](#version-numbering)
- [Release Checklist](#release-checklist)

## Release Types

### Sprint Releases

- **Frequency:** End of each sprint (typically every 2 weeks)
- **Scope:** All completed sprint tasks
- **Version:** Minor version bump (e.g., 1.0 → 1.1)
- **Environment Path:** `develop` → `staging` → `production`

### Hotfixes

- **Trigger:** Critical bugs in production
- **Scope:** Minimal fix for specific issue
- **Version:** Patch version bump (e.g., 1.0.0 → 1.0.1)
- **Environment Path:** `hotfix branch` → `staging` → `production`

### Major Releases

- **Frequency:** Stage completions (e.g., Stage 1 complete)
- **Scope:** Major features or breaking changes
- **Version:** Major version bump (e.g., 1.x → 2.0)
- **Environment Path:** `develop` → `staging` → `production` (with extended testing)

## Sprint Release Process

### Phase 1: Pre-Release (T-3 days)

#### Code Freeze

```bash
# Announce code freeze in Slack
# No new features merged after this point
# Only bug fixes allowed

# Create release branch
git checkout develop
git pull origin develop
git checkout -b release/v1.x-sprint-N
git push -u origin release/v1.x-sprint-N
```

#### Pre-Release Checklist

- [ ] All sprint tasks complete (from Sprint Board)
- [ ] All tests passing on `develop`
- [ ] No critical or high-priority bugs open
- [ ] Code review backlog cleared
- [ ] Documentation updated
- [ ] Migration scripts tested
- [ ] Performance benchmarks run

### Phase 2: Testing (T-2 days)

#### Deploy to Staging

```bash
# Tag release candidate
git tag -a v1.x-sprint-N-rc1 -m "Release candidate 1 for Sprint N"
git push origin v1.x-sprint-N-rc1

# Deploy to staging (CI/CD handles this)
# Monitor deployment
kubectl get pods -n repograph-staging
kubectl logs -f deployment/repograph-api -n repograph-staging
```

#### Staging Validation Checklist

- [ ] All services healthy
- [ ] Database migrations applied successfully
- [ ] Smoke tests passed
- [ ] Integration tests passed on staging
- [ ] Performance benchmarks met
- [ ] Security scan passed (no high/critical vulnerabilities)
- [ ] Monitoring/alerting functional
- [ ] Manual exploratory testing complete

#### Performance Validation

```bash
# Run performance benchmarks against staging
python benchmarks/indexing_benchmark.py --env staging
python benchmarks/query_benchmark.py --env staging

# Check metrics
# - Query p95 < 2s
# - Index duration < 2min (medium repos)
# - Cache hit rate > 60%
# - Error rate < 1%
```

### Phase 3: Release (T-day)

#### Final Checks

```bash
# Verify all tests still passing
make ci

# Check changelog is complete
cat CHANGELOG.md | grep "v1.x-sprint-N"

# Verify release notes drafted
cat docs/releases/v1.x-sprint-N.md
```

#### Production Release Checklist

- [ ] All staging tests passed
- [ ] No critical bugs found in staging
- [ ] Release notes written and reviewed
- [ ] Changelog updated
- [ ] Migration plan documented (if applicable)
- [ ] Rollback plan documented
- [ ] On-call rotation assigned
- [ ] Stakeholders notified

#### Create Release

```bash
# Merge release branch to main
git checkout main
git pull origin main
git merge --no-ff release/v1.x-sprint-N
git push origin main

# Tag final release
git tag -a v1.x-sprint-N -m "Sprint N release"
git push origin v1.x-sprint-N

# Merge back to develop
git checkout develop
git merge --no-ff release/v1.x-sprint-N
git push origin develop

# Delete release branch
git branch -d release/v1.x-sprint-N
git push origin --delete release/v1.x-sprint-N
```

#### Deploy to Production

```bash
# Production deployment is triggered by tag push
# Monitor deployment progress
kubectl get pods -n repograph-prod -w

# Watch logs
kubectl logs -f deployment/repograph-api -n repograph-prod

# Check health endpoints
curl https://api.repograph.com/health
curl https://api.repograph.com/ready
```

#### Post-Deployment Validation

- [ ] All pods healthy
- [ ] Health checks passing
- [ ] Smoke tests passed in production
- [ ] Metrics flowing to monitoring
- [ ] No errors in logs (first 15 minutes)
- [ ] Sample queries working correctly
- [ ] User-facing features functional

#### Monitor (First 24 Hours)

```bash
# Watch key metrics
# - Error rate
# - Latency (p95, p99)
# - Throughput
# - Database connections
# - Cache hit rate

# Check dashboards
open https://grafana.repograph.com/d/sprint1

# Review alerts
# Ensure no critical alerts firing
```

### Phase 4: Post-Release

#### Documentation

- [ ] Release notes published
- [ ] Changelog updated on website
- [ ] API documentation updated
- [ ] User guide updated (if needed)
- [ ] Internal wiki updated

#### Communication

- [ ] Announce release in Slack (#repograph-releases)
- [ ] Email customers/stakeholders
- [ ] Update status page
- [ ] Post on Twitter/blog (if public release)

#### Retrospective

- [ ] Schedule sprint retrospective
- [ ] Collect feedback on release process
- [ ] Document lessons learned
- [ ] Update release process if needed

## Hotfix Process

### When to Hotfix

Hotfixes are for **critical** issues only:
- Production is down or severely degraded
- Data integrity issues
- Security vulnerabilities
- Critical bugs affecting all users

For non-critical bugs, wait for next sprint release.

### Hotfix Steps

```bash
# 1. Create hotfix branch from main
git checkout main
git pull origin main
git checkout -b hotfix/issue-description
git push -u origin hotfix/issue-description

# 2. Fix the issue
# Make minimal, focused changes
# Add tests for the bug
# Update CHANGELOG.md

# 3. Test locally
make test
make ci

# 4. Deploy to staging for validation
git tag -a v1.x.y-rc1 -m "Hotfix release candidate"
git push origin v1.x.y-rc1

# 5. Validate on staging
# Run smoke tests
# Verify fix works
# Check no regressions

# 6. Merge to main
git checkout main
git merge --no-ff hotfix/issue-description
git tag -a v1.x.y -m "Hotfix: [description]"
git push origin main
git push origin v1.x.y

# 7. Merge back to develop
git checkout develop
git merge --no-ff hotfix/issue-description
git push origin develop

# 8. Delete hotfix branch
git branch -d hotfix/issue-description
git push origin --delete hotfix/issue-description
```

### Hotfix Checklist

- [ ] Issue confirmed in production
- [ ] Root cause identified
- [ ] Fix tested locally
- [ ] Tests added for regression
- [ ] Deployed to staging
- [ ] Validated on staging
- [ ] Release notes written
- [ ] Stakeholders notified
- [ ] Deployed to production
- [ ] Fix validated in production
- [ ] Post-mortem scheduled (for critical issues)

## Rollback Plan

### When to Rollback

Rollback if:
- Critical bugs discovered post-release
- Performance severely degraded
- Data corruption detected
- Services failing health checks
- Error rate >5% for >5 minutes

### Automatic Rollback

CI/CD automatically rolls back if:
- Health checks fail after deployment
- Error rate spike detected
- Smoke tests fail

### Manual Rollback

```bash
# Option 1: Revert to previous version
kubectl rollout undo deployment/repograph-api -n repograph-prod
kubectl rollout undo deployment/repograph-worker -n repograph-prod

# Option 2: Deploy specific version
kubectl set image deployment/repograph-api \
  repograph-api=repograph:v1.x-previous \
  -n repograph-prod

# Verify rollback
kubectl rollout status deployment/repograph-api -n repograph-prod
kubectl get pods -n repograph-prod

# Check health
curl https://api.repograph.com/health
```

### Database Rollback

If migrations were applied:

```bash
# SSH to production pod
kubectl exec -it <pod-name> -n repograph-prod -- /bin/bash

# Rollback migrations
alembic downgrade -1

# Or rollback to specific version
alembic downgrade <revision>

# Verify
alembic current
```

### Post-Rollback Steps

1. Verify system is stable
2. Notify stakeholders of rollback
3. Create incident report
4. Root cause analysis
5. Create hotfix branch
6. Test fix thoroughly
7. Re-deploy when ready

## Version Numbering

We use **Semantic Versioning** (semver): `MAJOR.MINOR.PATCH`

### Format

```
v1.2.3-sprint-5
 │ │ │    │
 │ │ │    └─ Sprint identifier (optional)
 │ │ └────── PATCH: Bug fixes, no breaking changes
 │ └──────── MINOR: New features, backwards compatible
 └────────── MAJOR: Breaking changes, API incompatibility
```

### Examples

- `v1.0.0-sprint-1`: Stage 1, Sprint 1 release
- `v1.1.0-sprint-2`: Sprint 2 release (new features)
- `v1.1.1`: Hotfix for v1.1.0
- `v2.0.0`: Stage 2 release (breaking changes)

### When to Bump

- **MAJOR:** Breaking API changes, major architecture changes
- **MINOR:** New features, sprint releases, backwards compatible
- **PATCH:** Bug fixes, hotfixes, security patches

## Release Checklist

### Sprint Release Checklist

```markdown
## Pre-Release
- [ ] All sprint tasks complete
- [ ] All tests passing
- [ ] Performance benchmarks met
- [ ] Documentation updated
- [ ] Security scan passed
- [ ] Code freeze announced

## Staging
- [ ] Deployed to staging
- [ ] Smoke tests passed
- [ ] Integration tests passed
- [ ] Performance validated
- [ ] Manual testing complete
- [ ] Monitoring functional

## Production
- [ ] Release notes written
- [ ] Changelog updated
- [ ] Stakeholders notified
- [ ] On-call assigned
- [ ] Tagged in git
- [ ] Deployed to production
- [ ] Health checks passing
- [ ] Smoke tests passed
- [ ] Metrics flowing
- [ ] No critical errors

## Post-Release
- [ ] Announcement sent
- [ ] Documentation published
- [ ] 24-hour monitoring complete
- [ ] Retrospective scheduled
```

## Emergency Contacts

- **On-Call Engineer:** [Rotation schedule]
- **Tech Lead:** [Name/Contact]
- **DevOps Lead:** [Name/Contact]
- **Product Owner:** [Name/Contact]

## Runbooks

- [Deployment Runbook](./runbooks/deployment.md)
- [Rollback Runbook](./runbooks/rollback.md)
- [Incident Response](./runbooks/incident-response.md)
- [Database Migration](./runbooks/database-migration.md)

## Metrics & Monitoring

### Key Metrics to Monitor

- **Availability:** >99% uptime
- **Latency:** p95 <2s for queries
- **Error Rate:** <1%
- **Throughput:** Requests/sec
- **Database:** Connection pool, query time
- **Cache:** Hit rate, evictions

### Dashboards

- [Production Dashboard](https://grafana.repograph.com/d/prod)
- [Sprint 1 Metrics](https://grafana.repograph.com/d/sprint1)

### Alerts

Critical alerts fire to:
- PagerDuty (for on-call)
- Slack #repograph-alerts
- Email (tech leads)

## Change Log

| Date | Change | Author |
|------|--------|--------|
| 2025-11-22 | Initial release process | DevOps Lead |
