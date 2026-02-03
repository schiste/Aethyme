# Aethyme Cloud - Sales-Ready Product Roadmap

**From MVP to Enterprise-Grade SaaS Platform**

**Document Version:** 1.0
**Last Updated:** October 4, 2025
**Target Launch:** Q2 2026 (6-8 months)

---

## 🎯 Vision & Positioning

### Product Vision
**"The intelligent code search and understanding platform for modern development teams"**

Turn every repository into a queryable knowledge graph. Enable developers to:
- Search across all codebases instantly
- Understand code relationships and dependencies
- Integrate AI assistants with company code context
- Enforce architectural standards and best practices

### Market Position
- **Primary:** Sourcegraph alternative with AI-first approach
- **Secondary:** GitHub/GitLab search enhancement
- **Differentiation:** Graph-based architecture + LLM integration

### Target Customers

**Phase 1 - MVP (Late Nov 2025):**
- Early adopter engineering teams (10-50 developers)
- AI-powered development teams
- Remote-first startups

**Phase 2 - Growth (Q1 2026):**
- Mid-market companies (50-200 developers)
- Companies with multiple repositories
- Teams using Cursor, Copilot, Claude

**Phase 3 - Enterprise (Q2 2026):**
- Enterprise companies (200+ developers)
- Financial services, healthcare (compliance requirements)
- Multi-cloud, hybrid deployments

---

## 📊 Three-Phase Roadmap Overview

| Phase | Timeline | Focus | Revenue Target |
|-------|----------|-------|----------------|
| **MVP** | Nov 2025 (7 weeks) | Core functionality | Beta - $0 |
| **Growth** | Dec 2025 - Feb 2026 (3 months) | Scale & features | $10K MRR |
| **Enterprise** | Mar 2026 - May 2026 (3 months) | Enterprise-ready | $50K MRR |

**Total Time to Sales-Ready:** 6-8 months

---

## 🚀 PHASE 1: MVP (Current → Nov 2025)

**Goal:** Functional product that solves core problem
**Status:** 35% complete (Phase 5/12)
**Timeline:** 7 weeks remaining
**Investment:** Bootstrapped / seed funding

### Remaining Work (Phases 6-12)

#### Phase 6: Repository Indexing (2 weeks) ⏳
- **Critical Path:** YES
- Celery workers for background processing
- Git clone and update operations
- Tree-sitter AST parsing (30+ languages)
- SCIP index generation
- Elasticsearch document creation
- Webhook handlers for auto-sync
- Progress tracking UI

**Deliverable:** Repositories automatically indexed on push

#### Phase 7: Code Search (1.5 weeks) ⏳
- **Critical Path:** YES
- Full-text search across all repositories
- Symbol search (functions, classes, variables)
- Path/filename search
- Fuzzy matching
- Search filters (language, repo, date)
- Search results ranking
- Command palette (Cmd+K) search

**Deliverable:** Fast, accurate code search

#### Phase 8: Graph Analysis (1.5 weeks) ⏳
- **Critical Path:** YES
- Function call graph generation
- Class hierarchy visualization
- Import/dependency analysis
- "Find references" across repositories
- "Go to definition" cross-repo
- Impact analysis (what breaks if I change X)

**Deliverable:** Queryable code knowledge graph

#### Phase 9: Query Interface (1 week) ⏳
- **Critical Path:** MEDIUM
- Natural language queries ("Where is user auth handled?")
- Graph query DSL for power users
- Saved queries and bookmarks
- Query sharing and collaboration
- Query performance optimization

**Deliverable:** Ask questions about codebase

#### Phase 10: Billing Integration (1 week) ⏳
- **Critical Path:** YES (for revenue)
- Stripe subscription management
- 3 pricing tiers (Free, Pro, Team)
- Usage metering (repositories, searches)
- Billing portal
- Invoice generation
- Payment retry logic

**Deliverable:** Revenue-generating product

#### Phase 11: Testing & Polish (1 week) ⏳
- **Critical Path:** YES
- E2E test coverage (80%+)
- Performance optimization
- Bug fixes
- UI/UX polish
- Documentation completion
- Onboarding flow

**Deliverable:** Stable, polished product

#### Phase 12: Beta Launch (1 week) ⏳
- **Critical Path:** YES
- Deploy to production (GCP)
- Beta user onboarding (50 users)
- Monitoring and alerting
- Support system setup
- Feedback collection
- Iteration based on feedback

**Deliverable:** MVP in production with paying users

### MVP Success Criteria

**Technical:**
- ✅ Index 1000+ repositories
- ✅ <100ms search response time
- ✅ 99.5% uptime
- ✅ Support 30+ languages

**Business:**
- 🎯 50 beta users (developer teams)
- 🎯 10 paying teams ($29-99/mo)
- 🎯 NPS score >40
- 🎯 Weekly active usage >70%

**Revenue:**
- Target: $2-5K MRR by end of Phase 1

---

## 📈 PHASE 2: GROWTH (Dec 2025 - Feb 2026)

**Goal:** Scale product, add key features, achieve product-market fit
**Timeline:** 12 weeks (3 months)
**Investment:** $100-200K (seed extension or angel)

### Month 1: AI Integration (Weeks 13-16)

#### 13. Claude Code Integration ⭐
- **Priority:** CRITICAL
- Deep integration with Claude.ai API
- Code context injection for AI responses
- Inline code explanations
- Refactoring suggestions
- Documentation generation
- VS Code extension bridge

**Value Prop:** AI that knows your entire codebase

#### 14. Cursor Integration
- **Priority:** HIGH
- Cursor IDE plugin
- Code context provider
- Symbol resolution
- Cross-repository references
- Real-time sync

#### 15. GitHub Copilot Context
- **Priority:** MEDIUM
- Copilot integration API
- Context enrichment
- Repository-aware suggestions

#### 16. Custom AI Prompts
- **Priority:** MEDIUM
- Prompt templates
- Team-shared prompts
- Context-aware prompting
- Prompt analytics

**Month 1 Deliverable:** AI assistants with company code context

### Month 2: IDE Plugins (Weeks 17-20)

#### 17. VS Code Extension ⭐
- **Priority:** CRITICAL
- Search panel in sidebar
- Symbol lookup
- Go to definition (cross-repo)
- Find references (cross-repo)
- Code navigation
- Inline results

**Adoption Target:** 500+ installs

#### 18. JetBrains Plugin
- **Priority:** HIGH
- IntelliJ IDEA, PyCharm, WebStorm support
- Same features as VS Code
- Native IDE integration

#### 19. Vim/Neovim Plugin
- **Priority:** MEDIUM
- Telescope.nvim integration
- FZF integration
- LSP-like experience

#### 20. Web IDE (Monaco)
- **Priority:** LOW
- In-browser code viewer
- Quick exploration
- No local setup needed

**Month 2 Deliverable:** Developers use Aethyme in their workflow

### Month 3: Collaboration & Analytics (Weeks 21-24)

#### 21. Team Collaboration ⭐
- **Priority:** CRITICAL
- Code annotations and comments
- @mentions in code
- Discussion threads
- Code review integration
- Shared searches and queries

**Value Prop:** GitHub Issues meets code search

#### 22. Analytics Dashboard
- **Priority:** HIGH
- Code ownership insights
- Language/framework distribution
- Technical debt tracking
- Activity heatmaps
- Contribution graphs

#### 23. Compliance & Audit
- **Priority:** HIGH (Enterprise requirement)
- Access logs and audit trail
- Search query logging
- Sensitive data detection
- PII scanning
- License compliance checking

#### 24. Advanced Search
- **Priority:** MEDIUM
- Regular expression search
- Structural search (AST-based)
- Saved searches
- Search alerts (code changes matching query)
- Search API for automation

**Month 3 Deliverable:** Team productivity and governance features

### Phase 2 Success Criteria

**Technical:**
- ✅ 10,000+ repositories indexed
- ✅ 3 IDE plugins available
- ✅ AI integration working
- ✅ 99.9% uptime

**Business:**
- 🎯 500 active users
- 🎯 100 paying teams
- 🎯 $10K MRR
- 🎯 30% month-over-month growth
- 🎯 <3% monthly churn

**Product-Market Fit Indicators:**
- 40%+ users would be "very disappointed" if product disappeared
- 5+ organic customer testimonials
- Word-of-mouth growth >30%

---

## 🏢 PHASE 3: ENTERPRISE (Mar - May 2026)

**Goal:** Enterprise-ready platform with compliance, security, scale
**Timeline:** 12 weeks (3 months)
**Investment:** $500K-1M (Series A preparation)

### Month 1: Enterprise Security (Weeks 25-28)

#### 25. SSO & SAML ⭐
- **Priority:** CRITICAL (Enterprise blocker)
- Okta integration
- Azure AD / Entra ID
- Google Workspace
- Custom SAML providers
- JIT provisioning
- SCIM user sync

**Enterprise Requirement:** Must-have for >200 users

#### 26. Advanced RBAC
- **Priority:** CRITICAL
- Fine-grained permissions
- Repository-level access control
- Team-based permissions
- Custom roles
- Permission inheritance
- Admin delegation

#### 27. Data Residency
- **Priority:** HIGH (GDPR, compliance)
- Multi-region deployment
- EU data residency
- US-only option
- Data locality guarantees
- Compliance certifications (SOC 2 Type II)

#### 28. Security Hardening
- **Priority:** CRITICAL
- Penetration testing
- Secrets scanning
- Vulnerability scanning
- Security headers
- Rate limiting v2
- DDoS protection

**Month 1 Deliverable:** Enterprise security compliance

### Month 2: Scale & Performance (Weeks 29-32)

#### 29. Horizontal Scaling ⭐
- **Priority:** CRITICAL
- Multi-region deployment
- Load balancing
- Database sharding
- Elasticsearch clustering
- Redis clustering
- CDN for static assets

**Scale Target:** 100K+ repositories, 10K+ users

#### 30. Performance Optimization
- **Priority:** HIGH
- Query optimization
- Caching strategy v2
- Search result pagination
- Incremental indexing
- Lazy loading
- Response time <50ms

#### 31. High Availability
- **Priority:** CRITICAL (Enterprise SLA)
- 99.99% uptime SLA
- Active-active deployment
- Automated failover
- Disaster recovery
- Backup automation
- Incident response

#### 32. Monitoring & Observability
- **Priority:** HIGH
- Datadog / New Relic integration
- Custom metrics dashboard
- Alerting system v2
- Log aggregation
- Distributed tracing
- Performance profiling

**Month 2 Deliverable:** Enterprise-grade infrastructure

### Month 3: Enterprise Features (Weeks 33-36)

#### 33. Self-Hosted Option ⭐
- **Priority:** HIGH (Large enterprise requirement)
- Docker Compose deployment
- Kubernetes Helm charts
- AWS deployment guide
- Azure deployment guide
- GCP deployment guide
- Air-gapped installation

**Revenue Impact:** Unlock $100K+ annual contracts

#### 34. Custom Integrations
- **Priority:** MEDIUM
- Jira integration
- Slack advanced features
- Microsoft Teams
- ServiceNow
- Custom webhooks
- REST API expansion

#### 35. Advanced Reporting
- **Priority:** MEDIUM
- Executive dashboards
- Custom reports
- Scheduled reports
- CSV/PDF export
- Data warehouse integration
- API analytics

#### 36. White-Label Option
- **Priority:** LOW
- Custom branding
- Custom domain
- Custom email templates
- Reseller program

**Month 3 Deliverable:** Enterprise sales-ready features

### Phase 3 Success Criteria

**Technical:**
- ✅ 100K+ repositories supported
- ✅ 99.99% uptime
- ✅ SOC 2 Type II certified
- ✅ GDPR compliant
- ✅ Self-hosted option available

**Business:**
- 🎯 2,000+ users across 200+ companies
- 🎯 10+ enterprise customers (>$50K/year)
- 🎯 $50K MRR
- 🎯 Annual contracts >$500K
- 🎯 Net Revenue Retention >120%

**Enterprise Readiness:**
- Security questionnaire template
- Enterprise sales playbook
- Reference customers (3+)
- Case studies (2+)
- ROI calculator

---

## 💰 Pricing Strategy Evolution

### MVP Pricing (Phase 1)

**Free Tier:**
- 5 repositories
- 1 user
- Basic search
- Community support

**Pro - $29/month:**
- 25 repositories
- 5 users
- Advanced search
- AI integration
- Email support

**Team - $99/month:**
- 100 repositories
- 20 users
- All Pro features
- Priority support
- Analytics

**Target:** Prove willingness to pay

### Growth Pricing (Phase 2)

**Free Tier:** (Same)

**Pro - $49/month:**
- 50 repositories (+2x)
- 10 users (+2x)
- IDE plugins
- AI integration
- Collaboration features

**Team - $199/month:**
- 250 repositories (+2.5x)
- 50 users (+2.5x)
- All Pro features
- Team collaboration
- Analytics
- Priority support

**Business - $499/month:** NEW
- 1,000 repositories
- 150 users
- All Team features
- SSO/SAML
- Advanced RBAC
- SLA 99.9%
- Dedicated support

**Target:** $10K MRR

### Enterprise Pricing (Phase 3)

**Free/Pro/Team/Business:** (Adjusted upward 20-30%)

**Enterprise - Custom:**
- Unlimited repositories
- Unlimited users
- Self-hosted option
- 99.99% SLA
- Custom integrations
- White-label option
- Dedicated CSM
- Professional services

**Typical Enterprise Deal:**
- $50K-500K/year
- Multi-year contracts
- Quarterly business reviews
- Custom development included

**Target:** $50K+ MRR, $500K ARR in pipeline

---

## 🎯 Go-To-Market Strategy

### Phase 1 (MVP): Product-Led Growth

**Channels:**
- Product Hunt launch
- Hacker News Show HN
- Developer Twitter/X
- GitHub README
- Tech blogs (Medium, Dev.to)

**Content:**
- "How we built Aethyme"
- "AI assistants that know your code"
- Open-source indexing library

**Acquisition Cost:** $0 (organic only)

### Phase 2 (Growth): Developer Marketing

**Channels:**
- Developer conferences (sponsorships)
- Tech podcasts (interviews)
- YouTube tutorials
- Comparison pages (vs Sourcegraph)
- Integration partnerships (Claude, Cursor)

**Content:**
- Case studies
- Video demos
- Documentation
- Developer guides
- Webinars

**Acquisition Cost:** <$200 CAC target

### Phase 3 (Enterprise): Sales-Led

**Channels:**
- Outbound sales (SDRs)
- Enterprise inbound (high-intent)
- G2/Capterra reviews
- Industry analysts (Gartner)
- Security certifications

**Content:**
- Enterprise security whitepaper
- ROI calculator
- Compliance documentation
- Executive presentations
- Reference architecture

**Sales Team:**
- 2 AEs (Account Executives)
- 1 SDR (Sales Development Rep)
- 1 Solutions Engineer
- 1 CSM (Customer Success Manager)

**Acquisition Cost:** <$10K CAC for enterprise deals

---

## 🏗️ Infrastructure Evolution

### MVP (Phase 1): Single Region

**Architecture:**
- GCP us-central1
- Cloud Run (API)
- Cloud SQL (PostgreSQL)
- Memorystore (Redis)
- Elasticsearch (managed)
- Cloud Storage (repos, artifacts)

**Cost:** ~$500/month for 100 users

### Growth (Phase 2): Multi-Region

**Architecture:**
- 3 regions (US, EU, Asia)
- Cloud Run + Load Balancer
- Cloud SQL with read replicas
- Redis Cluster
- Elasticsearch 3-node cluster
- CDN (Cloud CDN or Cloudflare)

**Cost:** ~$2,000/month for 1,000 users

### Enterprise (Phase 3): Global Scale

**Architecture:**
- 6+ regions worldwide
- Kubernetes (GKE)
- Multi-region PostgreSQL
- Redis Cluster (sharded)
- Elasticsearch 9+ nodes
- Global CDN
- DDoS protection
- WAF (Web Application Firewall)

**Cost:** ~$10,000/month for 10,000 users

**Self-Hosted Option:**
- Kubernetes Helm charts
- Minimum 3 nodes
- Customer-managed infrastructure
- Support for AWS, Azure, GCP, on-prem

---

## 👥 Team Evolution

### Current (Phase 1)
- 1 Full-stack engineer (you)

### Phase 2 (Growth) - Target Team
- 2 Backend engineers
- 1 Frontend engineer
- 1 DevOps/SRE
- 1 Product manager
- 1 Designer (contract)

**Total:** 5-6 people

### Phase 3 (Enterprise) - Target Team
- 4 Backend engineers
- 2 Frontend engineers
- 2 DevOps/SRE
- 1 Product manager
- 1 Designer
- 2 Sales (AE + SDR)
- 1 Customer success
- 1 Marketing

**Total:** 13-15 people

---

## 📊 Financial Projections

### Revenue Forecast

| Phase | Timeline | Users | MRR | ARR | Notes |
|-------|----------|-------|-----|-----|-------|
| MVP (Beta) | Nov 2025 | 50 | $2K | $24K | 10 paying teams |
| Growth Start | Dec 2025 | 150 | $5K | $60K | Product-led growth |
| Growth Mid | Jan 2026 | 300 | $10K | $120K | IDE plugins live |
| Growth End | Feb 2026 | 500 | $15K | $180K | AI integration |
| Enterprise Start | Mar 2026 | 800 | $25K | $300K | First enterprise deal |
| Enterprise Mid | Apr 2026 | 1,500 | $40K | $480K | Enterprise features |
| Enterprise End | May 2026 | 2,000 | $50K+ | $600K+ | Sales-ready |

### Investment Requirements

**Phase 1 (MVP):**
- Runway: 3 months solo dev
- Burn: $10K/month (personal + infra)
- Total: $30K

**Phase 2 (Growth):**
- Team: 5-6 people
- Burn: $75K/month
- Total: $225K (3 months)

**Phase 3 (Enterprise):**
- Team: 13-15 people
- Burn: $150K/month
- Total: $450K (3 months)

**Total Investment Needed:** $700K-1M

**Suggested Funding:**
- Bootstrapped: Phase 1
- Angel/Pre-seed ($250K): Phase 2
- Seed ($1-2M): Phase 3 + expansion

---

## 🎯 Key Milestones & Gates

### Gate 1: MVP Launch (Nov 2025)
**Criteria:**
- ✅ All Phase 1-12 features complete
- ✅ 50 beta users signed up
- ✅ 10 paying teams
- ✅ <100ms search performance
- ✅ 99.5% uptime for 2 weeks

**Decision:** Proceed to Phase 2 or pivot?

### Gate 2: Product-Market Fit (Feb 2026)
**Criteria:**
- ✅ $10K MRR achieved
- ✅ NPS >40
- ✅ 40%+ "very disappointed" metric
- ✅ <3% churn rate
- ✅ Organic growth evidence

**Decision:** Raise seed round for enterprise push?

### Gate 3: Enterprise-Ready (May 2026)
**Criteria:**
- ✅ $50K MRR
- ✅ SOC 2 Type II certified
- ✅ 3+ enterprise customers
- ✅ Self-hosted option live
- ✅ Sales team hired

**Decision:** Series A raise for scaling?

---

## 🚧 Risk Mitigation

### Technical Risks

**Risk:** Scaling indexing to 100K+ repos
- **Mitigation:** Incremental indexing, distributed workers, queue prioritization

**Risk:** Search performance degradation
- **Mitigation:** Elasticsearch optimization, caching, query optimization

**Risk:** Security breach
- **Mitigation:** Pen testing, bug bounty, security audits, compliance

### Business Risks

**Risk:** Sourcegraph competition
- **Mitigation:** AI-first differentiation, better UX, lower price

**Risk:** Low willingness to pay
- **Mitigation:** Freemium model, clear ROI, dev productivity metrics

**Risk:** Slow enterprise sales
- **Mitigation:** Product-led growth first, self-serve motion

### Market Risks

**Risk:** AI coding tools replace need for search
- **Mitigation:** Integrate with AI tools, become context provider

**Risk:** GitHub/GitLab add similar features
- **Mitigation:** Speed to market, better product, multi-platform

---

## 📋 Success Metrics by Phase

### MVP (Phase 1)
- **Activation:** User connects first repository
- **Engagement:** 3+ searches per week
- **Retention:** 70%+ weekly active
- **Revenue:** $2K MRR

### Growth (Phase 2)
- **Activation:** IDE plugin installed
- **Engagement:** 10+ searches per week
- **Retention:** 80%+ weekly active
- **Viral:** 1.3+ invitation rate
- **Revenue:** $10K MRR

### Enterprise (Phase 3)
- **Activation:** SSO configured
- **Engagement:** 50+ queries per user/month
- **Retention:** 95%+ monthly active
- **Expansion:** 120%+ net revenue retention
- **Revenue:** $50K MRR, $500K ARR pipeline

---

## 🎯 Competitive Positioning

### vs. Sourcegraph
**Their Advantage:** Established, enterprise-proven, 10+ years
**Our Advantage:** AI-first, modern UX, 1/3 the price, graph-based

### vs. GitHub Code Search
**Their Advantage:** Built-in, free, massive scale
**Our Advantage:** Multi-platform, AI integration, graph analysis, better search

### vs. OpenGrok
**Their Advantage:** Open-source, free
**Our Advantage:** SaaS, no setup, AI features, modern UX

### Our Unique Value
1. **Graph-based architecture** - relationships, not just text
2. **AI-native** - Claude, Copilot, Cursor integration
3. **Multi-platform** - GitHub + GitLab + Bitbucket
4. **Modern UX** - built for 2025+, not 2010

---

## 📖 Summary: The Path to $1M ARR

**Month 0 (Oct 2025):** Phase 5/12 complete (35% MVP)

**Month 2 (Nov 2025):** MVP launch - $2K MRR

**Month 3 (Dec 2025):** AI integration - $5K MRR

**Month 4 (Jan 2026):** IDE plugins - $10K MRR → **Product-Market Fit**

**Month 5 (Feb 2026):** Collaboration - $15K MRR

**Month 6 (Mar 2026):** Enterprise security - $25K MRR

**Month 7 (Apr 2026):** Scale & performance - $40K MRR

**Month 8 (May 2026):** Sales-ready - $50K MRR → **Enterprise-Ready**

**Month 9-12 (Jun-Sep 2026):** Scale to $80K+ MRR ($1M ARR run rate)

---

**Timeline to Sales-Ready Product:** 8 months
**Timeline to $1M ARR:** 12 months
**Total Investment Required:** $700K-1M

**Next Immediate Step:** Complete Phase 6 (Repository Indexing) - 2 weeks

---

*This roadmap is a living document and will be updated as we learn from customers and market feedback.*
