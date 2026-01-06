# New Developer Onboarding Checklist

**Purpose:** Fast-track new developers to productivity
**Target:** <1 week to first PR merged
**Owner:** Skills & Training Lead

---

## Pre-Start (Before Day 1)

### Access & Accounts
- ☐ GitHub organization access granted
- ☐ Slack channels added
- ☐ Email account created
- ☐ 1Password vault access
- ☐ Google Cloud Platform access (if DevOps)
- ☐ Linear/Jira project access
- ☐ Figma access (if Frontend)

### Hardware & Software
- ☐ Laptop provisioned (macOS/Linux recommended)
- ☐ IDE installed (VS Code, PyCharm, or preferred)
- ☐ Docker Desktop installed
- ☐ Git configured
- ☐ VPN access configured (if remote)

---

## Day 1: Welcome & Setup

### Morning: Introductions (9am - 12pm)

**9:00am - Welcome Meeting**
- ☐ Meet manager and team
- ☐ Overview of RepoGraph project
- ☐ Review roadmap (`packages/repograph/ROADMAP.md`)
- ☐ Understand Stage 1 goals

**10:00am - Environment Setup**
```bash
# Clone repository
git clone git@github.com:org/repograph.git
cd repograph

# Install dependencies
pnpm install

# Copy environment template
cp .env.example .env

# Start dev environment
make dev
```

**Check:**
- ☐ Repository cloned successfully
- ☐ Dependencies installed (no errors)
- ☐ Dev environment running
- ☐ Can access:
  - Backend API: http://localhost:8000
  - PostgreSQL: localhost:5432
  - Redis: localhost:6379

### Afternoon: Orientation (1pm - 5pm)

**1:00pm - Architecture Overview**
- ☐ Read: `Agents/skills/architecture/skill.md`
- ☐ Understand: Multi-tenant architecture
- ☐ Understand: RLS policies
- ☐ Understand: Data flow (UI → API → DB)

**2:00pm - Skills System**
- ☐ Read: `Agents/skills/README.md` (if exists)
- ☐ Browse: `Agents/skills/` directory
- ☐ Load priority skills:
  - architecture
  - scripts-management
  - rbac (if backend)
  - learnings-management

**3:00pm - Run Tests**
```bash
# Run all tests
pnpm test

# Run quality checks
pnpm quality:all

# Check test coverage
pnpm test:coverage
```

**Check:**
- ☐ All tests pass (or understand known failures)
- ☐ Quality checks pass
- ☐ Coverage report generated

**4:00pm - First Code Exploration**
```bash
# Search codebase
pnpm discover <topic>

# Examples:
pnpm discover authentication
pnpm discover multi-tenant
pnpm discover suppliers
```

**Check:**
- ☐ Can search codebase effectively
- ☐ Understand project structure
- ☐ Found example of key pattern (auth, RLS, etc.)

---

## Day 2: Deep Dive

### Morning: Core Concepts (9am - 12pm)

**Backend Developers:**
- ☐ Read: `Agents/skills/rbac/skill.md`
- ☐ Read: `Agents/skills/rate-limiting/skill.md`
- ☐ Read: `Agents/skills/database-migrations/skill.md`
- ☐ Explore: `backend/api/` directory
- ☐ Explore: `backend/db/models.py`

**Frontend Developers:**
- ☐ Read: `apps/customer/README.md`
- ☐ Understand: Menu configuration
- ☐ Understand: Route registration
- ☐ Understand: Data-UI selectors
- ☐ Explore: `apps/customer/src/`

**DevOps Developers:**
- ☐ Read: `Agents/skills/kubernetes-helm/skill.md`
- ☐ Read: `deploy/README.md`
- ☐ Understand: CI/CD pipeline
- ☐ Explore: `.github/workflows/`
- ☐ Explore: `deploy/k8s/`

### Afternoon: Hands-On Exercise (1pm - 5pm)

**Backend Exercise: Build Test Endpoint**
```python
# Task: Create authenticated test endpoint

# 1. Add model
# backend/test/models.py

class TestResource(Base):
    __tablename__ = "test_resources"
    id = Column(UUID, primary_key=True)
    org_id = Column(UUID, nullable=False)  # Multi-tenant
    name = Column(String, nullable=False)

# 2. Add RLS policy (migration)
# 3. Add serializer
# 4. Add ViewSet with permission check
# 5. Write tests
```

**Check:**
- ☐ Model created with org_id (multi-tenant)
- ☐ RLS policy written
- ☐ Endpoint requires authentication
- ☐ Tests pass

**Frontend Exercise: Build Test Component**
```typescript
// Task: Create test component with data-ui selectors

// 1. Create component in apps/customer/src/components/test/
// 2. Add data-ui selectors to all interactive elements
// 3. Register route in menu.config.ts
// 4. Write component tests
```

**Check:**
- ☐ Component renders
- ☐ Has data-ui selectors (100% coverage)
- ☐ Route registered
- ☐ Tests pass

**DevOps Exercise: Deploy to Local K8s**
```bash
# Task: Deploy RepoGraph to local Kubernetes

# 1. Start minikube/kind
minikube start

# 2. Build image
docker build -t repograph-api:test .

# 3. Deploy with Helm
helm install repograph-test ./deploy/helm/repograph

# 4. Verify deployment
kubectl get pods
kubectl get services
```

**Check:**
- ☐ Pods running
- ☐ Services accessible
- ☐ Health checks passing

---

## Day 3: First Contribution

### Morning: Pick First Issue (9am - 12pm)

**Find Starter Issue:**
- ☐ Browse issues labeled `good-first-issue`
- ☐ Ask team for recommendations
- ☐ Choose issue aligned with role (backend/frontend/devops)

**Common Starter Issues:**
- Add test coverage for existing endpoint
- Fix documentation typo/improvement
- Add data-ui selectors to component
- Update README with clarifications
- Write missing skill document

**Plan Work:**
- ☐ Understand acceptance criteria
- ☐ Identify files to modify
- ☐ Search for similar patterns in codebase
- ☐ Load relevant skills

### Afternoon: Implement & Submit (1pm - 5pm)

**Implementation:**
```bash
# 1. Create branch
git checkout -b fix/issue-123-description

# 2. Make changes

# 3. Run tests
pnpm test

# 4. Run quality checks
pnpm quality:all

# 5. Commit with good message
git commit -m "fix: issue description

- What changed
- Why changed
- How tested

Closes #123"

# 6. Push and create PR
git push origin fix/issue-123-description
```

**Check:**
- ☐ Tests pass locally
- ☐ Quality checks pass
- ☐ Commit message follows conventions
- ☐ PR created with description
- ☐ PR linked to issue

---

## Day 4: Code Review & Refinement

### Morning: Address Feedback (9am - 12pm)

**Review Process:**
- ☐ Respond to reviewer comments
- ☐ Make requested changes
- ☐ Re-run tests
- ☐ Push updates
- ☐ Request re-review

**Learning:**
- ☐ Understand why changes requested
- ☐ Ask questions if unclear
- ☐ Update local knowledge

### Afternoon: Pair Programming (1pm - 5pm)

**Pair with Senior Developer:**
- ☐ Shadow on real task
- ☐ Ask questions
- ☐ Take notes
- ☐ Practice navigating codebase

**Topics to Cover:**
- How to debug issues
- How to search for patterns
- How to write effective tests
- How to navigate skills system

---

## Day 5: Team Integration

### Morning: Demo & Retrospective (9am - 12pm)

**Demo Your Work:**
- ☐ Present your first PR to team
- ☐ Explain what you learned
- ☐ Share challenges overcome

**Retrospective:**
- ☐ What went well in onboarding?
- ☐ What was confusing?
- ☐ What would improve onboarding?
- ☐ What resources were most helpful?

### Afternoon: Next Steps Planning (1pm - 5pm)

**With Manager:**
- ☐ Assign to Stage 1 task team
- ☐ Review training plan (Week 1-4)
- ☐ Set 30/60/90 day goals
- ☐ Schedule regular 1:1s

**Self-Study:**
- ☐ Read assigned task roadmap (S1-T1 to S1-T11)
- ☐ Load task-specific skills
- ☐ Prepare questions for team

---

## Week 2: Specialized Training

Follow [TRAINING_PLAN.md](./TRAINING_PLAN.md) based on assigned task:

- **S1-T1 (Auth):** Learn auth, RLS, rate limiting
- **S1-T2 (Indexing):** Learn SCIP, caching, metrics
- **S1-T3 (Queries):** Learn query optimization, caching
- **S1-T4/T5 (AI-Readiness):** Learn scorecard, autofixers
- **S1-T6/T7 (Guardrails):** Learn LLM guardrails, telemetry
- **S1-T8/T9/T10 (Ops):** Learn Kubernetes, CI/CD, docs

---

## Week 3-4: Full Productivity

### Week 3: First Major Task
- ☐ Complete assigned subtask from Stage 1
- ☐ Participate in team standups
- ☐ Code review 2+ PRs from teammates
- ☐ Ask for help when stuck

### Week 4: Independence
- ☐ Lead a task end-to-end
- ☐ Review code from peers
- ☐ Present work in team demo
- ☐ Contribute to skill documentation

---

## 30/60/90 Day Goals

### 30 Days
- ☐ Environment fully set up
- ☐ 3+ PRs merged
- ☐ Proficient in 3 core skills
- ☐ Complete assigned Stage 1 subtask
- ☐ Participate in 10+ code reviews

### 60 Days
- ☐ Lead 1 major feature
- ☐ Mentor new developer (if available)
- ☐ Expert in 1 core skill
- ☐ Present 1 skill share
- ☐ Contribute to architecture discussions

### 90 Days
- ☐ Independently own task area
- ☐ Create 1 new skill document
- ☐ Review critical PRs
- ☐ Identify and fix technical debt
- ☐ Contribute to roadmap planning

---

## Resources

### Documentation
- **Roadmap:** `packages/repograph/ROADMAP.md`
- **Skills:** `Agents/skills/`
- **Architecture:** `docs/architecture/`
- **API Docs:** `docs/api/`

### Tools
- **Search Code:** `pnpm discover <query>`
- **Run Tests:** `pnpm test`
- **Quality Gate:** `pnpm quality:all`
- **Dev Environment:** `make dev`

### People
- **Manager:** Weekly 1:1s
- **Buddy:** Assigned senior developer
- **Team:** Daily standups
- **Skills Lead:** Training questions

### Communication
- **Slack:**
  - #repograph-general (announcements)
  - #repograph-dev (development)
  - #repograph-help (questions)
- **GitHub:** Issues, PRs, discussions
- **Meetings:**
  - Daily standup (15min)
  - Weekly team sync (1hr)
  - Bi-weekly demo (1hr)

---

## Common Questions

### "How do I run tests for specific file?"
```bash
# Python tests
pytest backend/tests/test_specific.py

# TypeScript tests
pnpm test --filter specific.test.ts
```

### "How do I debug auth issues?"
1. Check JWT token in request headers
2. Verify org_id in token matches resource
3. Check RLS policy allows access
4. Review audit logs

### "How do I add a new API endpoint?"
1. Load skill: `api-conventions`
2. Add model with org_id
3. Write RLS policy
4. Add serializer
5. Add ViewSet with permission
6. Write tests
7. Update OpenAPI schema

### "Where are the logs?"
```bash
# Local development
docker-compose logs -f api

# Kubernetes
kubectl logs -f deployment/repograph-api

# Structured logs (search)
cat logs/api.log | jq '. | select(.level == "ERROR")'
```

### "How do I find examples of X?"
```bash
# Use pnpm discover
pnpm discover authentication
pnpm discover multi-tenant
pnpm discover data-ui

# Or grep skills
grep -r "authentication" Agents/skills/
```

---

## Onboarding Checklist Summary

### Day 1
- ☐ Environment setup complete
- ☐ Can run tests
- ☐ Understand architecture
- ☐ Know how to search codebase

### Day 2
- ☐ Core concepts understood
- ☐ Completed hands-on exercise
- ☐ Tests passing

### Day 3
- ☐ First PR created
- ☐ Tests + quality checks passing
- ☐ Linked to issue

### Day 4
- ☐ Addressed code review feedback
- ☐ Pair programmed with senior

### Day 5
- ☐ First PR merged
- ☐ Assigned to task team
- ☐ Training plan understood

### Week 2+
- ☐ Following specialized training
- ☐ Loading task-specific skills
- ☐ Contributing to Stage 1 tasks

---

## Success Metrics

**Individual:**
- Time to first PR: <3 days
- Time to first merge: <5 days
- Skills loaded: 3+ by end of Week 1
- Code reviews: 2+/week
- Questions asked: Healthy (shows engagement)

**Team:**
- Onboarding satisfaction: 4+/5
- New developer productivity: 50% by Week 2, 100% by Week 4
- Retention: 90%+ stay after 90 days

---

**Document Owner:** Skills & Training Lead
**Review Cadence:** After each new hire
**Last Updated:** 2025-11-22
**Feedback:** Share improvements in #repograph-dev
