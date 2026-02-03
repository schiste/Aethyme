# Aethyme Cloud - Option A: Research-First Implementation
## Project Kickoff Document

**Decision:** Option A - Research-First Implementation
**Decided:** October 6, 2025
**Timeline:** 6-12 months to production release
**Investment:** $400-500K
**Goal:** Build world-class, research-backed code intelligence platform

---

## 🎯 Vision Statement

We are building **the most advanced code intelligence platform in the world**, based on cutting-edge research in:
- Graph theory and code representation
- Neurobiology and human memory systems
- LLM context management and cognitive architectures

**Not an incremental improvement. A paradigm shift.**

---

## 📚 What We're Building (from aethyme-research.md)

### The Five Research Innovations

#### 1. Event-Segmented Code Memory (ESCM)
**Concept:** Brain-inspired code event segmentation using Bayesian surprise detection

**What it does:**
- Parses codebases into episodic "events" (modules, refactors, major changes)
- Detects "surprise" signals when code complexity changes dramatically
- Segments repository into coherent events using graph-theoretic boundary refinement
- Stores events in long-term memory like human episodic memory

**Why it matters:**
- Reduces interference between unrelated code
- Dramatically narrows search space for AI agents
- Enables temporal reasoning about code evolution

**Timeline:** Months 3-4

---

#### 2. Hierarchical Code Memory Architecture (HCMA)
**Concept:** 3-tier memory system inspired by human working memory

**Architecture:**
```
┌─────────────────────────────────────┐
│   SENSORY BUFFER                    │
│   Recent code lines, local vars     │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│   SHORT-TERM MEMORY                 │
│   Compressed module summaries       │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│   LONG-TERM GRAPH MEMORY            │
│   Full repository knowledge graph   │
└─────────────────────────────────────┘
```

**Central Executive:** Controls attention and memory flow between levels

**Why it matters:**
- Respects cognitive limits (no unbounded context)
- Reduces token overhead dramatically
- Plug-and-play architecture with minimal parameters

**Timeline:** Months 5-6

---

#### 3. Prediction-Error-Driven Graph Retrieval (PEDGR)
**Concept:** Retrieve context only when AI model is uncertain

**How it works:**
- Monitor AI agent's self-estimated uncertainty
- When uncertainty spikes → trigger graph retrieval
- Type of error determines graph path (missing deps → USES edges, unclear types → CONTAINS edges)
- Retrieval window adapts dynamically

**Why it matters:**
- Avoids unnecessary context loading
- Focuses compute on hardest problems
- AI learns which graph paths reduce error fastest

**Timeline:** Months 7-8

---

#### 4. Cognitive Workspace for Code Agents (CWCA)
**Concept:** Treat repository graph as external cognitive workspace

**Features:**
- Persistent workspace objects (subgraphs, patterns, notes)
- Hierarchical buffers: focus / working / long-term
- Agent can manipulate workspace like a whiteboard
- Changes persist across sessions
- Active memory management reduces cognitive load

**Why it matters:**
- Transforms reactive search into proactive organization
- Handles larger projects without context limits
- Mirrors human use of external tools (notebooks, whiteboards)

**Timeline:** Months 9-10

---

#### 5. Multimodal Community-Aware Graph Memory (MCGM)
**Concept:** Graph communities with docs, tests, issues, commits

**Architecture:**
- Community detection (Louvain/Leiden algorithms)
- Hierarchical summaries (architectural → API level)
- Multimodal: code + docs + tests + issues + commits
- Map-reduce reasoning across communities

**Why it matters:**
- Both global overviews AND detailed information
- Reduces missing context
- Ensures code/docs/tests alignment
- Efficient exploration of unfamiliar codebases

**Timeline:** Months 11-12

---

## 🗓️ 12-Month Implementation Roadmap

### Phase 0: Foundation (Months 1-2)

**Month 1: Infrastructure & Core Platform**

**Week 1-2: Fix Current System**
- [ ] Fix API server (critical blocker)
- [ ] Apply all missing database migrations
- [ ] Verify infrastructure health (PostgreSQL, Redis, Elasticsearch)
- [ ] Document baseline performance metrics
- [ ] Set up comprehensive monitoring (OpenTelemetry, Prometheus, Grafana)

**Week 3-4: Research-Ready Schema Design**
- [ ] Design `events` table for ESCM
- [ ] Design `memory_buffers` tables for HCMA (sensory/short/long-term)
- [ ] Design `workspace_objects` tables for CWCA
- [ ] Design `communities` and `community_summaries` for MCGM
- [ ] Add `prediction_errors` tracking for PEDGR
- [ ] Design `node_embeddings` with pgvector
- [ ] Create comprehensive schema migration plan

**Deliverable:** ✅ Working infrastructure + research-ready database schema

---

**Month 2: Core Graph Engine**

**Week 5-6: Advanced Graph Queries**
- [ ] Implement Ego Graph v2 (typed edges, direction filters, pagination)
- [ ] Implement Impact Analysis v2 (bidirectional, path enumeration)
- [ ] Add meet-in-the-middle BFS for efficient traversal
- [ ] Performance optimization: p95 < 75ms (ego), < 200ms (impact)
- [ ] Comprehensive test suite with golden datasets

**Week 7-8: Indexing Pipeline Enhancement**
- [ ] Enhance SCIP indexer for typed edges (INVOKE, IMPORT, INHERIT, CONTAIN)
- [ ] Tree-sitter fallback with confidence scores
- [ ] Provenance tracking for all edges
- [ ] Call graph extraction baseline
- [ ] Data-flow edges (basic)

**Deliverable:** ✅ Production-grade graph query engine

---

### Phase 1: Event-Segmented Code Memory - ESCM (Months 3-4)

**Month 3: ESCM Backend**

**Week 9-10: Event Detection Algorithm**
- [ ] Implement Bayesian surprise detection
  - Calculate baseline code complexity metrics
  - Detect sudden complexity changes (new modules, refactors, dependency shifts)
  - Generate surprise signals with confidence scores
- [ ] Implement graph-theoretic boundary refinement
  - Use graph structure to align event boundaries with dependency breaks
  - Prevent arbitrary mid-function segmentation
  - Validate boundaries with commit history

**Week 11-12: Event Memory Storage**
- [ ] Design event representation schema
  - Event metadata (trigger type, confidence, timestamp)
  - Associated code snippets, tests, documentation
  - Graph substructure snapshot
- [ ] Implement event storage and retrieval
  - Efficient indexing for temporal and similarity queries
  - Event compression for long-term storage
- [ ] Build event query API
  - Find events by timeframe, surprise magnitude, affected modules
  - Retrieve complete event context

**Deliverable:** ✅ Working ESCM system with event segmentation and retrieval

---

**Month 4: ESCM Integration & Validation**

**Week 13-14: Two-Stage Retrieval**
- [ ] Implement similarity-based event search
  - Vector embeddings for events
  - ANN search for relevant events
- [ ] Implement temporally contiguous retrieval
  - Surface events before/after target event
  - Capture evolution narrative
- [ ] Combine retrieval strategies with learned weights

**Week 15-16: ESCM Testing & Validation**
- [ ] Create evaluation dataset
  - Golden events from real repositories
  - Expected boundaries and relationships
- [ ] Measure performance
  - Event detection accuracy (precision/recall)
  - Retrieval relevance (MRR, NDCG)
  - Search space reduction vs full-repo search
- [ ] Iterate based on results

**Deliverable:** ✅ Validated ESCM reduces context by 40%+ vs naive approaches

---

### Phase 2: Hierarchical Code Memory - HCMA (Months 5-6)

**Month 5: HCMA Architecture**

**Week 17-18: Memory Buffers Implementation**
- [ ] Sensory buffer
  - Rolling window of recent code lines
  - Local variable tracking
  - Fast access, limited capacity
- [ ] Short-term memory
  - Module/function summaries using token compression
  - Recently accessed items with recency/frequency scoring
  - LRU eviction policy
- [ ] Long-term graph memory
  - Full repository knowledge graph (already built)
  - Persistent storage
  - Indexed for fast retrieval

**Week 19-20: Central Executive Controller**
- [ ] Attention mechanism
  - Determine which memory level to query
  - Flow control between levels
  - Load balancing to prevent over-querying long-term memory
- [ ] Promotion/demotion logic
  - Move useful items from sensory → short-term
  - Archive important patterns to long-term
  - Evict stale items

**Deliverable:** ✅ Working 3-tier memory system

---

**Month 6: HCMA Optimization & Training**

**Week 21-22: Multi-Level RL Training**
- [ ] Define reward signals
  - Final answer correctness (sparse)
  - Memory efficiency (dense)
  - Retrieval accuracy (dense)
- [ ] Implement RL training loop
  - Policy: when to promote/demote memory items
  - Train on diverse code tasks
  - Validate on held-out repositories

**Week 23-24: Integration & Benchmarking**
- [ ] Integrate HCMA with existing query engine
- [ ] Measure token overhead reduction
- [ ] Compare to flat memory baseline
- [ ] Optimize parameter sizes (sensory capacity, short-term size)

**Deliverable:** ✅ HCMA handles 10M+ token codebases efficiently

---

### Phase 3: Prediction-Error-Driven Retrieval - PEDGR (Months 7-8)

**Month 7: Error Detection & Triggering**

**Week 25-26: Uncertainty Estimation**
- [ ] Implement self-consistency checking
  - Multiple LLM passes, measure agreement
  - Entropy-based uncertainty scores
- [ ] Implement error signal generation
  - Detect incomplete/incorrect responses
  - Classify error type (missing deps, unclear types, logic gaps)
- [ ] Design trigger thresholds
  - When uncertainty exceeds threshold → retrieve
  - Adaptive thresholds based on task complexity

**Week 27-28: Error-Modulated Graph Traversal**
- [ ] Map error types to edge types
  - Missing dependencies → USES, INHERITS edges
  - Unclear types → CONTAINS, PARAMETER, RETURN edges
  - Logic gaps → INVOKE edges (call chains)
- [ ] Implement dynamic retrieval window
  - Narrow window as uncertainty decreases
  - Expand when error persists
- [ ] Priority queue for graph paths
  - Rank by expected error reduction
  - Learn from past successes

**Deliverable:** ✅ PEDGR system triggers intelligent retrieval

---

**Month 8: PEDGR Learning & Validation**

**Week 29-30: Active Learning**
- [ ] Track which graph paths reduce error
- [ ] Update path priorities based on outcomes
- [ ] Personalize for different task types
- [ ] Build path effectiveness database

**Week 31-32: Evaluation**
- [ ] Measure unnecessary retrievals avoided
- [ ] Measure error reduction speed
- [ ] Compare to static top-k retrieval
- [ ] Validate on diverse code reasoning tasks

**Deliverable:** ✅ PEDGR reduces unnecessary retrievals by 30%+

---

### Phase 4: Cognitive Workspace - CWCA (Months 9-10)

**Month 9: Workspace Objects & Persistence**

**Week 33-34: Workspace Architecture**
- [ ] Define workspace object types
  - Pinned graph substructures
  - Code pattern abstractions
  - Agent annotations and notes
  - Design decisions and constraints
- [ ] Implement hierarchical buffers
  - Focus buffer (immediate task)
  - Working buffer (near-future tasks)
  - Repository buffer (background knowledge)
- [ ] Build persistence layer
  - Save/load workspace state
  - Version control for workspaces
  - Share workspaces across agents/sessions

**Week 35-36: Active Memory Management**
- [ ] Implement promotion/demotion operations
  - Agent explicitly moves objects between buffers
  - Automatic suggestions based on usage patterns
- [ ] Add workspace manipulation tools
  - Annotate graph segments
  - Restructure class hierarchies
  - Tag important symbols
- [ ] Multi-level reward signals for workspace efficiency

**Deliverable:** ✅ Persistent cognitive workspace system

---

**Month 10: CWCA Integration & Optimization**

**Week 37-38: Agent Integration**
- [ ] Integrate workspace into LLM agent workflow
- [ ] Provide workspace management API
  - Pin/unpin items
  - Move between buffers
  - Query workspace contents
- [ ] Implement workspace-aware retrieval
  - Check workspace before graph queries
  - Update workspace after successful retrievals

**Week 39-40: Evaluation & Tuning**
- [ ] Measure cognitive load reduction
- [ ] Measure repetitive retrieval elimination
- [ ] Test on large multi-step tasks
- [ ] Optimize buffer sizes and promotion policies

**Deliverable:** ✅ CWCA improves agent task success by 10pp+

---

### Phase 5: Multimodal Community-Aware Graph - MCGM (Months 11-12)

**Month 11: Community Detection & Multimodal Ingestion**

**Week 41-42: Community Detection**
- [ ] Implement Louvain/Leiden algorithm
  - Build dependency graph
  - Detect dense communities (modules, packages)
  - Create hierarchical community structure
- [ ] Generate community summaries
  - LLM-based summarization at multiple levels
  - Cache summaries by commit SHA
  - Invalidate on significant changes

**Week 43-44: Multimodal Artifact Ingestion**
- [ ] Ingest documentation
  - Parse Markdown/MDX/ADR files
  - Extract code references
  - Link to symbols via file paths and anchors
- [ ] Ingest tests
  - Support pytest/Jest/Vitest/JUnit
  - Map tests to code via imports and naming
  - Track coverage and flakiness
- [ ] Ingest issues (opt-in)
  - GitHub/GitLab/Jira integration
  - Link issues to commits and symbols
  - Privacy-aware (PII redaction)
- [ ] Ingest commits (opt-in)
  - Store metadata and changed files
  - Link commits to affected symbols

**Deliverable:** ✅ Multimodal graph with communities

---

**Month 12: Map-Reduce Reasoning & Production Launch**

**Week 45-46: Map-Reduce Query Processing**
- [ ] Implement community-aware query routing
  - Use community summaries to identify relevant communities
  - Query multiple communities in parallel
  - Aggregate answers with map-reduce
- [ ] Add consistency checking
  - Detect code/docs/tests mismatches
  - Surface warnings to users
  - Suggest fixes

**Week 47-48: Final Integration & Launch**
- [ ] End-to-end testing
  - All 5 research features working together
  - Performance testing at scale
  - Security audit
- [ ] Production deployment
  - Kubernetes cluster setup
  - Monitoring and alerting
  - Documentation and runbooks
- [ ] User onboarding materials
- [ ] 🚀 **PRODUCTION LAUNCH**

**Deliverable:** ✅ **World-class research-backed code intelligence platform in production**

---

## 👥 Team Structure

### Core Team (Full-Time)

**Engineering Lead** (1)
- Overall architecture ownership
- Code review and quality
- Team coordination
- Estimated: $150K/year ($75K for 6 months)

**Senior Backend Engineers** (2)
- Graph engine implementation (Eng #1)
- Memory systems and indexing (Eng #2)
- API development
- Estimated: $140K/year each ($140K for 6 months)

**ML/AI Specialist** (1)
- Embedding generation and semantic search
- RL training for HCMA and PEDGR
- Model integration (OpenAI, Claude, local models)
- Estimated: $160K/year ($80K for 6 months)

**Research Engineer** (1)
- Algorithm implementation (ESCM, PEDGR)
- Research paper reference implementation
- Novel components (Bayesian surprise, graph boundary refinement)
- Estimated: $150K/year ($75K for 6 months)

**DevOps Engineer** (0.5 FTE)
- Infrastructure management
- Deployment automation
- Monitoring and observability
- Estimated: $130K/year ($32.5K for 6 months part-time)

**Total Team Cost:** ~$402.5K for 6 months

### Additional Costs

- Infrastructure (GCP/AWS): $20K
- Tools and services: $10K
- Contingency (10%): $43K

**Total Investment:** ~$475K

---

## 🎯 Success Metrics

### Phase 0 (Month 2)
- ✅ API p95 latency < 100ms
- ✅ Infrastructure 99.9% uptime
- ✅ All database migrations applied
- ✅ Graph queries working with test data

### Phase 1 - ESCM (Month 4)
- ✅ Event detection precision/recall > 0.85
- ✅ Context retrieval 40% smaller than full-repo
- ✅ Event segmentation aligns with commit boundaries (80%+ agreement)

### Phase 2 - HCMA (Month 6)
- ✅ Token overhead < 10% of flat memory approach
- ✅ Successfully handles 10M+ token codebases
- ✅ Central executive makes correct memory-level decisions (90%+ accuracy)

### Phase 3 - PEDGR (Month 8)
- ✅ Reduces unnecessary retrievals by 30%+
- ✅ Error reduction 2x faster than static retrieval
- ✅ Uncertainty-triggered retrieval precision > 0.80

### Phase 4 - CWCA (Month 10)
- ✅ Agent task success +10 percentage points
- ✅ Repetitive retrievals reduced by 50%+
- ✅ Workspace state persists correctly across sessions

### Phase 5 - MCGM (Month 12)
- ✅ Answer completeness +20% with multimodal context
- ✅ Cross-artifact inconsistency detection ≥ 70%
- ✅ Community-aware routing 3x faster than exhaustive search

### Production Launch (Month 12)
- ✅ Platform handles 1000+ repositories
- ✅ All research features integrated and working
- ✅ p99 latency < 500ms for complex queries
- ✅ 10 beta customers successfully onboarded
- ✅ Security audit passed
- ✅ Documentation complete

---

## 📚 Key Research Papers to Implement

1. **Knowledge Graph Based Repository-Level Code Generation**
   - Source: ar5iv.labs.arxiv.org/html/2505.14394
   - Implementation: Month 2 (graph engine)

2. **GraphRAG: Complex Data Discovery**
   - Source: microsoft.com/research
   - Implementation: Month 11-12 (MCGM)

3. **Event Segmentation Theory**
   - Source: pmc.ncbi.nlm.nih.gov/articles/PMC12313307
   - Implementation: Month 3-4 (ESCM)

4. **Cognitive Workspace for LLMs**
   - Source: arxiv.org/html/2508.13171v1
   - Implementation: Month 9-10 (CWCA)

5. **Hierarchical Memory Transformer**
   - Source: ar5iv.labs.arxiv.org/html/2405.06067v3
   - Implementation: Month 5-6 (HCMA)

6. **EM-LLM: Episodic Memory**
   - Source: em-llm.github.io
   - Implementation: Month 3-4, 7-8 (ESCM, PEDGR)

---

## 🚨 Risk Management

### Technical Risks

| Risk | Mitigation |
|------|------------|
| Research features too complex | Start with simpler versions, iterate; have fallback to classical approaches |
| Performance doesn't meet targets | Extensive profiling from month 1; optimization sprints if needed |
| ML models underperform | Multiple model options (OpenAI, Claude, local); ensemble approaches |
| Integration challenges | Weekly integration tests; modular architecture allows swapping components |

### Schedule Risks

| Risk | Mitigation |
|------|------------|
| ESCM/HCMA take longer | Phases can extend 2-4 weeks; launch with subset if needed |
| Hiring delays | Contract with consultants; partnerships with research labs |
| Scope creep | Strict roadmap lock; no new features until Phase 5 complete |

### Market Risks

| Risk | Mitigation |
|------|------------|
| Competitors ship first | Our differentiation is research innovation, not speed |
| Research features not valued | Each phase has measurable improvements; pivot if needed |
| Too complex for customers | Excellent documentation; "simple mode" that hides complexity |

---

## 📞 Next Steps (This Week)

### By October 13, 2025

**Leadership:**
- [ ] Approve $475K budget
- [ ] Commit to 6-12 month timeline
- [ ] Begin recruiting process (ML specialist, research engineer)

**Engineering:**
- [ ] Fix API server (Days 1-3)
- [ ] Create detailed Phase 0 specification (Days 4-7)
- [ ] Set up project management (JIRA/Linear)
- [ ] Schedule daily standups

**Research:**
- [ ] Download and review all 6 research papers
- [ ] Create implementation notes for each
- [ ] Identify open-source implementations to reference

---

## 🎉 Why This Will Work

**1. Clear Research Foundation**
- Not making it up - implementing peer-reviewed research
- Proven concepts from neuroscience, graph theory, LLM research
- Standing on shoulders of giants

**2. Phased Approach**
- Each month delivers working component
- Can course-correct based on results
- Early phases create foundation for later ones

**3. Measurable Progress**
- Quantitative success metrics for each phase
- Regular evaluation and validation
- Data-driven decisions

**4. Right Team**
- Mix of engineers, ML specialists, researchers
- Expertise in required domains
- Proven track record

**5. Defensible Moat**
- Novel algorithms (ESCM, HCMA, PEDGR, CWCA, MCGM)
- Hard to replicate (requires deep research understanding)
- Potential for patents/publications
- First-mover advantage in research-backed code intelligence

---

## 💡 Expected Outcomes

### At 6 Months
- ✅ ESCM, HCMA, PEDGR implemented and validated
- ✅ Platform handling real repositories
- ✅ Demonstrable 40%+ efficiency gains over baselines
- ✅ Beta testing with select customers

### At 12 Months
- ✅ All 5 research innovations in production
- ✅ World-class code intelligence platform
- ✅ 100+ repositories indexed
- ✅ 50+ paying customers
- ✅ Research papers published (conference submissions)
- ✅ Patent applications filed
- ✅ Series A fundraising or acquisition interest

### Long-term (18-24 Months)
- ✅ Category-defining product
- ✅ Industry standard for AI-powered code intelligence
- ✅ Research lab partnerships
- ✅ Open-source some components (build ecosystem)
- ✅ Expand to adjacent markets (code generation, automated refactoring)

---

**This is ambitious. This is bold. This is the right path.**

**Let's build something truly groundbreaking.**

---

**Document Created:** October 6, 2025
**Status:** Ready to Execute
**Next Review:** Weekly progress check-ins
**Target Launch:** June 2026 (12 months)

**Let's do this. 🚀**
