# RepoGraph Cloud - Product Roadmap

**Last Updated:** November 5, 2025
**Version:** 1.0 MVP → 2.0 Enterprise

---

## Vision

Build the leading AI-powered code intelligence platform that turns repositories into queryable knowledge graphs, enabling developers to search, analyze, and understand code at scale.

---

## Current Status

**Phase:** MVP Development (90% Complete)
**Target Launch:** Mid-November 2025
**Next Milestone:** Phase 12 Frontend UI (1-2 weeks)

---

## Roadmap Overview

### Phase 1: MVP (Current - 90% Complete)
**Target:** Mid-November 2025
**Goal:** Launch core platform with essential features

**Complete:**
- Authentication & user management
- Repository management
- OAuth integration (GitHub, GitLab, Bitbucket)
- Code indexing (7 languages)
- Full-text search
- Advanced search (Boolean, regex, field-specific)
- AI provider layer (BYOK architecture)
- Vector embeddings
- Semantic search backend
- Webhooks & incremental indexing
- Background task processing

**Remaining:**
- AI Settings UI
- Semantic Search UI
- Usage Dashboard
- End-to-end testing
- User documentation

### Phase 2: Graph Visualization (2-3 weeks)
**Target:** Late November - Early December 2025
**Goal:** Add visual analysis capabilities

**Features:**
- Interactive call graph visualization (D3.js/Cytoscape)
- Dependency tree explorer
- Impact analysis visualization
- Code relationship explorer
- Export to PNG/SVG

**Technical:**
- Graph data API endpoints
- Frontend visualization components
- Performance optimization for large graphs

### Phase 3: Developer Tools (3-4 weeks)
**Target:** December 2025 - January 2026
**Goal:** Integrate with developer workflows

**IDE Plugins:**
- VS Code extension
- JetBrains plugin (IntelliJ, PyCharm, etc.)
- Vim/Neovim plugin

**CLI Tool:**
- Search from terminal
- Repository management
- CI/CD integration

**MCP Server:**
- Model Context Protocol server
- AI assistant integration
- Custom context generation

### Phase 4: Enterprise Features (4-6 weeks)
**Target:** January - February 2026
**Goal:** Enterprise-ready platform

**Authentication & Access:**
- SSO/SAML integration
- Advanced RBAC
- Team management
- Organization hierarchies
- Audit logs

**Billing & Usage:**
- Stripe integration
- Usage-based pricing
- Team billing
- Invoice generation
- Payment methods

**Security & Compliance:**
- SOC 2 Type II compliance
- GDPR compliance
- Data encryption at rest
- VPC support
- Self-hosted option

### Phase 5: Advanced AI (6-8 weeks)
**Target:** February - April 2026
**Goal:** Next-generation AI capabilities

**Enhanced Search:**
- Natural language queries
- Multi-language embeddings
- Code similarity search
- Duplicate code detection

**AI-Powered Analysis:**
- Automated code review suggestions
- Bug prediction
- Performance optimization hints
- Security vulnerability detection

**Custom Models:**
- Fine-tuned models on customer code
- Domain-specific embeddings
- Custom prompt templates

### Phase 6: Scale & Performance (Ongoing)
**Target:** Continuous
**Goal:** Handle enterprise-scale deployments

**Performance:**
- Sub-100ms search latency
- Real-time indexing
- Horizontal scaling
- Caching optimization

**Scale Targets:**
- 1M+ repositories
- 100B+ lines of code
- 10K+ concurrent users
- 99.99% uptime

---

## Feature Priorities

### Must-Have (MVP)
- [x] Authentication system
- [x] Repository management
- [x] OAuth integration
- [x] Code indexing
- [x] Search functionality
- [x] AI provider integration
- [ ] AI Settings UI
- [ ] Semantic Search UI

### Should-Have (Post-MVP)
- [ ] Graph visualization
- [ ] IDE plugins
- [ ] CLI tool
- [ ] Team management
- [ ] Billing system

### Nice-to-Have (Future)
- [ ] Custom AI models
- [ ] Advanced analytics
- [ ] Mobile apps
- [ ] Slack/Discord bots
- [ ] API marketplace

---

## Success Metrics

### MVP Launch (Phase 1)
- 10 beta users
- 50+ repositories indexed
- < 500ms search response time
- 99% uptime
- User satisfaction > 4/5

### Growth Phase (Phases 2-3)
- 100+ active users
- 1000+ repositories
- 10+ IDE plugin installs
- 5+ team subscriptions

### Enterprise Phase (Phases 4-6)
- 10+ enterprise customers
- $100K+ ARR
- 10K+ repositories
- SOC 2 certified
- 99.9% uptime

---

## Timeline Summary

```
Nov 2025:  MVP Launch (Phase 1 complete)
Dec 2025:  Graph Visualization (Phase 2)
Jan 2026:  Developer Tools (Phase 3)
Feb 2026:  Enterprise Features (Phase 4 start)
Mar 2026:  Enterprise Complete (Phase 4 done)
Apr 2026:  Advanced AI (Phase 5 start)
Q3 2026:   Scale & Optimization (Phase 6)
```

**Total Timeline to Enterprise-Ready:** ~6 months from MVP launch

---

## Technology Roadmap

### Current Stack
- Backend: FastAPI + Python 3.11
- Frontend: Next.js 14 + React 18
- Database: PostgreSQL 15
- Search: Elasticsearch 8
- Queue: Celery + Redis
- AI: OpenAI/Claude/Azure (BYOK)

### Planned Additions
- Graph Database: Neo4j (Phase 2)
- Visualization: D3.js/Cytoscape (Phase 2)
- Monitoring: Datadog/New Relic (Phase 4)
- CDN: Cloudflare (Phase 4)
- Load Balancer: Cloud Load Balancing (Phase 6)

---

## Market Strategy

### Target Customers

**Phase 1-2 (MVP):**
- Individual developers
- Small teams (2-10 people)
- Open source projects

**Phase 3-4 (Growth):**
- Engineering teams (10-50 people)
- Startups
- Mid-size companies

**Phase 5-6 (Enterprise):**
- Large enterprises (500+ developers)
- Fortune 500 companies
- Government agencies

### Pricing Strategy

**Free Tier:**
- 1 user
- 3 repositories
- 100 searches/day
- Community support

**Pro ($29/user/month):**
- Unlimited repositories
- Unlimited searches
- IDE plugins
- Email support

**Team ($99/user/month):**
- Everything in Pro
- Team management
- Advanced RBAC
- Priority support

**Enterprise (Custom):**
- Everything in Team
- SSO/SAML
- Self-hosted option
- SLA & dedicated support
- Custom integrations

---

## Risk Mitigation

### Technical Risks
- **Performance at scale:** Continuous optimization, horizontal scaling
- **AI cost management:** BYOK architecture eliminates platform costs
- **Data security:** Encryption, SOC 2, regular audits

### Market Risks
- **Competition:** Focus on AI-first approach, superior UX
- **Adoption:** Free tier, excellent documentation, community building
- **Retention:** Continuous feature delivery, responsive support

### Operational Risks
- **Team scaling:** Hire ahead of growth, clear documentation
- **Infrastructure costs:** Efficient architecture, usage-based billing
- **Support load:** Self-service docs, automated onboarding

---

## Dependencies

### Critical Path
1. Complete Phase 12 (AI UI) → Enables MVP launch
2. MVP launch → Enables beta user feedback
3. Beta feedback → Informs Phase 2 priorities
4. Phase 2-3 → Enables Team tier
5. Phase 4 → Enables Enterprise tier

### External Dependencies
- OAuth provider availability (GitHub, GitLab)
- AI provider APIs (OpenAI, Anthropic)
- Cloud infrastructure (GCP)
- Payment processing (Stripe)

---

## Decision Points

### End of Phase 1 (Nov 2025)
**Decision:** Launch MVP or add more features?
- **Option A:** Launch with current features
- **Option B:** Add graph visualization first
- **Recommendation:** Launch MVP, iterate based on feedback

### End of Phase 3 (Jan 2026)
**Decision:** Focus on enterprise or growth features?
- **Option A:** Build enterprise features (higher revenue)
- **Option B:** Focus on developer tools (more users)
- **Recommendation:** Depends on customer feedback

### End of Phase 4 (Mar 2026)
**Decision:** Continue feature development or focus on scale?
- **Option A:** Build advanced AI features
- **Option B:** Optimize for scale and performance
- **Recommendation:** Balance both with dedicated teams

---

## Review & Updates

This roadmap is reviewed and updated:
- **Weekly:** During development sprints
- **Monthly:** For milestone tracking
- **Quarterly:** For strategic direction

**Last Review:** November 5, 2025
**Next Review:** November 15, 2025 (After Phase 12 completion)

---

**For detailed status, see [STATUS.md](STATUS.md)**
**For historical plans, see [docs/planning/](docs/planning/)**
