# Agent Enablement Quick Start Guide

**5-Minute Guide to RepoGraph Agent-Enablement Features**

---

## Installation

```bash
cd /path/to/repograph
pip install -e .
```

---

## Quick Commands

### 1. Check Parity Score (30 seconds)

```bash
repograph agent parity --repo /path/to/your/repo --format json
```

**Output:**
- Overall score (0-100)
- Violations by invariant
- Actionable recommendations

### 2. Export Context Pack (1 minute)

```bash
repograph agent context-pack --repo . --output context.json
```

**Output:**
- menu/routes/env/tests/models/api data
- Compressed JSON (60-70% token savings)
- Deterministic checksum

### 3. Generate Agent.md (30 seconds)

```bash
repograph agent manifest --repo . --template standard
```

**Output:**
- `agent.md` file in repo root
- Project identity, conventions, red flags
- Integration with parity score

### 4. Find and Fix Gaps (2 minutes)

```bash
# Show gaps
repograph agent gaps --repo . --priority critical

# Generate fixes (dry-run)
repograph agent autofix --repo . --dry-run

# Apply safe fixes
repograph agent autofix --repo . --safe-only
```

**Output:**
- Prioritized gap list
- Autofix patches in unified diff format
- Applied fixes to repository

---

## API Usage

### Check Parity via API

```bash
curl -X POST http://localhost:8000/api/agent/parity-scan \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "org_id": "my-org",
    "repo_path": "/path/to/repo"
  }'
```

### Download Context Pack

```bash
curl -X GET "http://localhost:8000/api/agent/context-pack/repo123?compress=true" \
  -H "Authorization: Bearer $TOKEN" \
  -o context.json.gz
```

---

## Python API

### Scan Repository

```python
from pathlib import Path
from src.agent_enablement import ParityScorer

repo_path = Path("/path/to/repo")
scorer = ParityScorer()

report = scorer.scan_repository(repo_path)
print(f"Score: {report.overall_score}/100")
print(f"Violations: {report.total_violations}")
print(f"Actionable: {report.actionable_fixes}")
```

### Generate Context Pack

```python
from src.agent_enablement import ContextPackGenerator

generator = ContextPackGenerator()
pack = generator.generate_pack(
    repo_path,
    pack_types=["routes", "env", "tests"]
)

generator.save_pack(pack, Path("context.json"))
print(f"Checksum: {pack.checksum}")
```

### Detect Gaps

```python
from src.agent_enablement import GapDetector

detector = GapDetector()
gaps = detector.detect_gaps(repo_path)

critical_gaps = [g for g in gaps if g.priority.name == "CRITICAL"]
print(f"Critical gaps: {len(critical_gaps)}")
```

### Generate Manifest

```python
from src.agent_enablement import OnboardingManifestGenerator

generator = OnboardingManifestGenerator()
manifest = generator.generate_manifest(
    repo_path,
    template="standard"
)

generator.save_manifest(manifest, repo_path)
```

---

## Integration Workflow

### Complete Agent Onboarding Flow

```bash
#!/bin/bash
REPO_PATH="/path/to/repo"

# Step 1: Check readiness
echo "Checking agent readiness..."
repograph agent parity --repo $REPO_PATH --format json > parity.json
SCORE=$(jq '.overall_score' parity.json)

if (( $(echo "$SCORE < 80" | bc -l) )); then
  echo "Score too low ($SCORE). Running autofixes..."

  # Step 2: Auto-fix safe issues
  repograph agent autofix --repo $REPO_PATH --safe-only

  # Step 3: Re-check
  repograph agent parity --repo $REPO_PATH --format json > parity.json
  SCORE=$(jq '.overall_score' parity.json)
fi

# Step 4: Generate context pack
echo "Generating context pack..."
repograph agent context-pack --repo $REPO_PATH --output context.json

# Step 5: Generate manifest
echo "Generating Agent.md..."
repograph agent manifest --repo $REPO_PATH --template standard

# Step 6: Report
echo "✓ Agent onboarding complete"
echo "  Parity Score: $SCORE/100"
echo "  Context Pack: context.json"
echo "  Manifest: $REPO_PATH/agent.md"
```

---

## Common Use Cases

### Use Case 1: Pre-Onboarding Repo Prep

**Goal:** Prepare repository before agent starts work

```bash
# Scan and fix
repograph agent parity --repo . --output parity.json
repograph agent autofix --repo . --safe-only

# Generate onboarding materials
repograph agent manifest --repo . --template comprehensive
repograph agent context-pack --repo . --output onboarding-pack.json

# Agent reads:
# 1. agent.md
# 2. onboarding-pack.json
# 3. parity.json (knows what gaps remain)
```

### Use Case 2: Continuous Monitoring

**Goal:** Monitor parity score over time

```bash
# Weekly cron job
repograph agent parity --repo . --output "parity_$(date +%Y%m%d).json"

# Check staleness
repograph agent staleness --repo .

# Alert if score drops
# (integrate with alerting system)
```

### Use Case 3: CI/CD Integration

**Goal:** Enforce parity in CI pipeline

```yaml
# .github/workflows/parity-check.yml
name: Agent Parity Check

on: [push, pull_request]

jobs:
  parity:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Check Parity Score
        run: |
          repograph agent parity --repo . --output parity.json
          SCORE=$(jq '.overall_score' parity.json)

          if (( $(echo "$SCORE < 80" | bc -l) )); then
            echo "::error::Parity score too low: $SCORE/100"
            exit 1
          fi

      - name: Upload Report
        uses: actions/upload-artifact@v3
        with:
          name: parity-report
          path: parity.json
```

---

## Troubleshooting

### Issue: Parity score unexpectedly low

**Solution:**
```bash
# Show detailed gaps
repograph agent gaps --repo . --format text

# Check specific invariant
repograph agent parity --repo . --format md | grep "data-ui"
```

### Issue: Context pack too large

**Solution:**
```bash
# Generate with compression
repograph agent context-pack --repo . --output pack.json --compress

# Generate specific types only
repograph agent context-pack --repo . --output pack.json --types routes,env
```

### Issue: Autofix fails

**Solution:**
```bash
# Try dry-run first
repograph agent autofix --repo . --dry-run

# Apply safe fixes only
repograph agent autofix --repo . --safe-only

# Check for conflicts
repograph agent gaps --repo . --autofixable-only
```

---

## Next Steps

1. **Read Full Documentation:**
   - `docs/s1-tLS1-T11-IMPLEMENTATION-SUMMARY.md` - Complete reference
   - `docs/invariants-reference.md` - All 9 invariants explained
   - `docs/context-packs-spec.md` - Pack format specification

2. **Try Example Repositories:**
   - `tests/agent_enablement/fixtures/exemplar_repo` - Perfect parity
   - `tests/agent_enablement/fixtures/partial_repo` - Typical case
   - `tests/agent_enablement/fixtures/broken_repo` - Needs work

3. **Integration:**
   - Connect to RepoGraph indexing (`repograph index`)
   - Use with query features (`repograph query search`)
   - Integrate with AI onboarding system

---

**Questions? Issues?**
- See: `docs/s1-tLS1-T11-IMPLEMENTATION-SUMMARY.md`
- Tests: `tests/agent_enablement/`
- Source: `src/agent_enablement/`

**Status:** ✅ AGENT_PARITY_V1 - ENABLED
