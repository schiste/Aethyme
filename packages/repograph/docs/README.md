# RepoGraph Documentation

Complete documentation for RepoGraph, the graph-based code intelligence system.

**Version:** 1.0 (Stage 1 - CLI-First)
**Last Updated:** 2025-11-22

---

## Quick Navigation

### For New Users
- [Quickstart Guide](getting-started/quickstart.md) - Get started in 5 minutes
- [AI Integration Guide](guides/ai-integration.md) - Use with AI assistants

### For Developers
- [Onboarding Guide](getting-started/onboarding.md) - Set up development environment
- [CLI Reference](reference/cli.md) - Command-line interface
- [API Reference](reference/api.md) - REST API documentation
- [Contributing Guide](../CONTRIBUTING.md) - How to contribute

### For Operations
- [Runbooks](runbooks/) - Operational procedures
- [Security Overview](security/security-overview.md) - Security architecture
- [Troubleshooting](guides/troubleshooting.md) - Common issues and solutions

---

## Documentation Index

### Getting Started

| Document | Description |
|----------|-------------|
| [Quickstart](getting-started/quickstart.md) | 5-minute quick start guide |
| [Onboarding](getting-started/onboarding.md) | Complete developer onboarding (Day 1 - Week 1) |

### Reference Documentation

| Document | Description | Lines |
|----------|-------------|-------|
| [API Reference](reference/api.md) | Complete REST API documentation | 1,572 |
| [CLI Reference](reference/cli.md) | Command-line interface reference | 1,022 |

### Guides

| Document | Description |
|----------|-------------|
| [AI Integration](guides/ai-integration.md) | Integrate RepoGraph with AI assistants (1,372 lines) |
| [AI Benefits](guides/ai-benefits.md) | Benefits of AI-readiness features |
| [Testing Guide](guides/testing.md) | Testing strategy and practices |
| [Troubleshooting](guides/troubleshooting.md) | Common issues and solutions |

### Architecture

| Document | Description |
|----------|-------------|
| [Stage 1 Architecture](architecture/stage1-architecture.md) | CLI-first service architecture |
| [Security](architecture/security.md) | Security architecture and controls |
| [Deployment](architecture/deployment.md) | Deployment strategies and infrastructure |
| [Integration Points](architecture/integration-points.md) | External system integrations |
| [Performance Budgets](architecture/performance-budgets.md) | Performance SLOs and targets |
| [Migration Plan](architecture/migration-plan.md) | Migration and upgrade strategies |
| [Technical Assessment](architecture/technical-assessment.md) | Technical decisions and trade-offs |

### Runbooks (Operations)

| Runbook | Severity | Description |
|---------|----------|-------------|
| [Index Failure](runbooks/index-failure.md) | HIGH | Diagnose and resolve indexing failures |
| [Staleness Remediation](runbooks/staleness-remediation.md) | MEDIUM | Detect and fix stale repository indexes |
| [Rollback Procedures](runbooks/rollback.md) | CRITICAL | Emergency rollback procedures |
| [Backup & Restore](runbooks/backup-restore.md) | CRITICAL | Database backup and disaster recovery |
| [Performance Degradation](runbooks/performance-degradation.md) | HIGH | Diagnose and fix performance issues |
| [Security Incident](runbooks/security-incident.md) | CRITICAL | Security incident response procedures |

### Security & Privacy

| Document | Description |
|----------|-------------|
| [Security Overview](security/security-overview.md) | Security principles and controls |
| [Privacy Policy](security/privacy-policy.md) | Data handling and privacy |
| [Threat Model](security/threat-model.md) | Threat analysis and mitigations |

### Migration & Upgrades

| Document | Description |
|----------|-------------|
| [Upgrading Guide](migrations/upgrading.md) | Version upgrade procedures |
| [Data Migration](migrations/data-migration.md) | Data export/import and migration |

### Implementation Summaries

| Document | Task | Status |
|----------|------|--------|
| [S1-T2 Summary](s1-tLS1-T2-IMPLEMENTATION-SUMMARY.md) | Indexing Reliability | Completed |
| [Reorganization Summary](REORGANIZATION_SUMMARY.md) | Code reorganization | Completed |
| [Freshness Dashboard](FRESHNESS-DASHBOARD-SETUP.md) | Monitoring setup | Completed |

### Root Documentation

| Document | Description |
|----------|-------------|
| [../README.md](../README.md) | Project overview and features |
| [../ROADMAP.md](../ROADMAP.md) | Product roadmap (Stage 1 & 2) |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Contribution guidelines |

---

## Documentation by Audience

### New Developer (Day 1)

1. [Onboarding Guide](getting-started/onboarding.md) - Complete setup
2. [Quickstart](getting-started/quickstart.md) - First indexing
3. [CLI Reference](reference/cli.md) - Command reference
4. [Testing Guide](guides/testing.md) - Run tests
5. [Contributing Guide](../CONTRIBUTING.md) - First contribution

### API Consumer

1. [Quickstart](getting-started/quickstart.md) - Basic usage
2. [API Reference](reference/api.md) - Complete API documentation
3. [Authentication](reference/api.md#authentication) - Auth setup
4. [Rate Limits](reference/api.md#rate-limits) - Usage limits
5. [Troubleshooting](guides/troubleshooting.md) - Common issues

### Operations Engineer

1. [Deployment Architecture](architecture/deployment.md) - Infrastructure
2. [Runbooks](runbooks/) - Operational procedures
3. [Performance Budgets](architecture/performance-budgets.md) - SLOs
4. [Security Overview](security/security-overview.md) - Security controls
5. [Backup & Restore](runbooks/backup-restore.md) - DR procedures

### AI Assistant User

1. [AI Integration Guide](guides/ai-integration.md) - Setup and usage
2. [AI Benefits](guides/ai-benefits.md) - Understand the value
3. [API Reference](reference/api.md) - API usage
4. [Quickstart](getting-started/quickstart.md) - Get started

### Security Reviewer

1. [Security Overview](security/security-overview.md) - Security architecture
2. [Threat Model](security/threat-model.md) - Threat analysis
3. [Privacy Policy](security/privacy-policy.md) - Data handling
4. [Security Architecture](architecture/security.md) - Technical implementation
5. [Security Incident Runbook](runbooks/security-incident.md) - Incident response

### Decision Maker

1. [AI Benefits](guides/ai-benefits.md) - Business value
2. [Roadmap](../ROADMAP.md) - Product direction
3. [Performance Budgets](architecture/performance-budgets.md) - SLOs
4. [Security Overview](security/security-overview.md) - Security posture

---

## Documentation Statistics

As of 2025-11-22:

- **Total Markdown Files:** 30+
- **Total Lines:** 19,000+
- **Reference Docs:** 2,500+ lines
- **Runbooks:** 6 operational procedures
- **Architecture Docs:** 7 documents
- **Test Coverage:** Link validation + code examples

---

## Documentation Standards

### File Organization

```
docs/
├── getting-started/     # Onboarding and quickstart
├── reference/           # API and CLI reference
├── guides/              # How-to guides
├── architecture/        # System architecture
├── runbooks/            # Operational procedures
├── security/            # Security and privacy
├── migrations/          # Upgrade guides
└── README.md            # This file
```

### Conventions

- **Markdown Format:** CommonMark compliant
- **Links:** Relative links for internal docs
- **Code Examples:** Tested and executable
- **Last Updated:** All docs include last updated date
- **Runbooks:** Standard format (Overview, Symptoms, Diagnostic, Resolution)

---

## Contributing to Documentation

See [Contributing Guide](../CONTRIBUTING.md) for general contribution guidelines.

### Documentation-Specific Guidelines

1. **Test Your Changes:**
   ```bash
   # Check links
   bash scripts/docs/check-links.sh

   # Lint documentation
   bash scripts/docs/lint-docs.sh

   # Run doc tests
   pytest tests/docs/
   ```

2. **Follow Templates:**
   - Runbooks: Use standard sections (Overview, Symptoms, Diagnostic, Resolution)
   - Guides: Include examples and use cases
   - Reference: Complete API/CLI documentation

3. **Update This Index:**
   - Add new documents to appropriate section
   - Update statistics
   - Run `bash scripts/docs/generate-docs.sh`

---

## Documentation Tooling

### Available Scripts

| Script | Purpose |
|--------|---------|
| `scripts/docs/check-links.sh` | Validate all internal links |
| `scripts/docs/lint-docs.sh` | Lint markdown formatting |
| `scripts/docs/generate-docs.sh` | Generate API/CLI docs from code |

### Running Documentation Tests

```bash
# All doc tests
pytest tests/docs/

# Link validation
pytest tests/docs/test_links.py

# Code example validation
pytest tests/docs/test_examples.py
```

---

## Support

### Getting Help

- **Troubleshooting:** [Troubleshooting Guide](guides/troubleshooting.md)
- **GitHub Issues:** [Report bugs](https://github.com/aeptus/repograph/issues)
- **GitHub Discussions:** [Ask questions](https://github.com/aeptus/repograph/discussions)
- **Slack** (internal): #repograph-support

### Reporting Documentation Issues

Found an error in the docs?

1. Check if already reported in [Issues](https://github.com/aeptus/repograph/issues)
2. Create new issue with label `documentation`
3. Include:
   - Document name and section
   - Description of issue
   - Suggested fix (if applicable)

---

## Documentation Roadmap

### Stage 1 (Current)

- [x] Core API/CLI documentation
- [x] Operational runbooks
- [x] Security documentation
- [x] Onboarding guide
- [x] Troubleshooting guide
- [x] Migration guides

### Stage 2 (Planned)

- [ ] Frontend documentation
- [ ] UI component library docs
- [ ] E2E testing guide
- [ ] Video tutorials
- [ ] Interactive examples
- [ ] API SDKs documentation

---

## Version History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2025-11-22 | Initial complete documentation for Stage 1 |
| 0.9 | 2025-11-15 | Added runbooks and operational docs |
| 0.8 | 2025-11-01 | Initial API/CLI reference |

---

**Documentation maintained by:** RepoGraph Team
**Review Cycle:** Monthly
**Next Review:** 2025-12-22
