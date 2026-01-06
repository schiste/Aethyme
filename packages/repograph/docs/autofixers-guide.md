# RepoGraph Autofixers Guide

Complete guide to using RepoGraph's safe automated code improvement system.

## Overview

RepoGraph Autofixers automatically detect and fix common codebase issues while maintaining safety through:

- **Risk Assessment**: Files are categorized as low/medium/high risk
- **Generated File Detection**: Automatically skips lock files, build outputs, and generated code
- **Approval Workflows**: Risky changes require manual approval
- **Dry-Run Mode**: Preview changes before applying
- **Pull Request Integration**: Create PRs with fixes for team review

## Available Fixers

### 1. Documentation Regenerator (`docs_regen`)

Generates and updates FOLDER.md files in directories containing code.

**What it fixes:**
- Missing FOLDER.md files in code directories
- Outdated directory documentation
- Missing file inventories

**Example output:**
```markdown
# components

**Location:** `src/components`

## Overview

This directory contains 5 files and 2 subdirectories.

## Files

### .tsx
- `Button.tsx` - Reusable button component
- `Input.tsx` - Form input component
```

**Risk Level:** LOW

### 2. Link Fixer (`link_fix`)

Converts absolute links to relative links in documentation.

**What it fixes:**
- `/docs/guide.md` → `../docs/guide.md`
- `http://localhost:3000/api.md` → `./api.md`
- Internal repository links

**Preserves:**
- External links (https://example.com)
- Already-relative links (./file.md)
- Anchor links (#section)

**Risk Level:** LOW

### 3. Selector Inserter (`selector_insert`)

Adds `data-ui` test selectors to interactive elements in React/Vue components.

**What it fixes:**
```tsx
// Before
<button onClick={handleClick}>Submit</button>

// After
<button data-ui="login-form-button" onClick={handleClick}>Submit</button>
```

**Elements targeted:**
- Buttons
- Links
- Form inputs
- Selects and textareas

**Naming convention:**
- `{component}-{element}-{identifier}`
- Example: `user-profile-button-save`

**Risk Level:** MEDIUM (modifies code)

### 4. i18n Scaffolder (`i18n_scaffold`)

Adds internationalization scaffolding for hardcoded strings.

**What it fixes:**
```tsx
// Before
<h1>Welcome Back</h1>

// After
import { useTranslation } from 'react-i18next';

function Component() {
  const { t } = useTranslation();
  return <h1>{t('component.welcome_back')}</h1>;
}
```

**Detects:**
- JSX text content
- String literals in props (title, label, placeholder)
- Hardcoded user-facing messages

**Skips:**
- Files already using i18n
- Code-like strings (variable names, etc.)
- Very short strings

**Risk Level:** MEDIUM (modifies code)

### 5. Format Fixer (`format_fix`)

Applies automated formatting using available formatters.

**Supported formatters:**
- **Python**: black, autopep8, ruff, isort
- **JavaScript/TypeScript**: prettier, eslint
- **Go**: gofmt
- **Rust**: rustfmt

**What it fixes:**
- Inconsistent indentation
- Line length violations
- Import ordering
- Trailing whitespace

**Risk Level:** LOW (formatter-dependent)

## Safety Features

### Generated File Detection

The safety engine automatically skips:

**File patterns:**
- `*.generated.*`
- `*.gen.*`
- `*.pb.*` (protobuf)
- `*_pb2.py`

**Lock files:**
- package-lock.json
- yarn.lock
- poetry.lock
- Cargo.lock
- go.sum

**Build directories:**
- node_modules/
- dist/
- build/
- __pycache__/
- .next/
- target/

**Header detection:**
```typescript
// @generated
// This file is auto-generated. Do not edit.
```

### Risk Assessment

Files are assessed for risk based on:

**High Risk:**
- Configuration files (package.json, pyproject.toml)
- Environment files (.env)
- Security files (credentials, keys)
- Build configs

**Medium Risk:**
- Migrations
- Routes/API definitions
- Test files
- Database schemas

**Low Risk:**
- Documentation
- README files
- Markdown content

### Approval Requirements

| Risk Level | Auto-Apply | Requires Approval |
|------------|-----------|-------------------|
| LOW        | Yes       | No                |
| MEDIUM     | No        | Yes               |
| HIGH       | No        | Yes               |

## Usage

### CLI Commands

#### Dry Run (Preview Changes)

```bash
repograph autofix /path/to/repo --dry-run
```

Shows what would be changed without modifying files.

**Output:**
```
RepoGraph Autofixer
============================================================
Repository: /path/to/repo
Fix type: all
Mode: DRY RUN

Scanning for documentation issues...
  Found 3 documentation fixes
Scanning for link issues...
  Found 2 link fixes

============================================================
Summary
============================================================
Total files: 5
Low risk: 5
Medium risk: 0
High risk: 0

  docs_regen: 3 files
  link_fix: 2 files

============================================================
Changes Preview (Dry Run)
============================================================
[unified diff output]
```

#### Apply Changes

```bash
repograph autofix /path/to/repo --apply
```

Applies low-risk changes immediately. Medium/high-risk changes require approval.

#### Specific Fix Types

```bash
# Only fix documentation
repograph autofix /path/to/repo --fix-type docs --apply

# Only add test selectors
repograph autofix /path/to/repo --fix-type selectors --dry-run

# Only i18n scaffolding
repograph autofix /path/to/repo --fix-type i18n --apply
```

Available types: `all`, `docs`, `links`, `selectors`, `i18n`, `format`

#### Create Pull Request

```bash
repograph autofix /path/to/repo --pr
```

Creates a new branch, applies fixes, and opens a pull request.

**PR Format:**
```markdown
## Autofix Summary

This PR applies automated fixes to 12 files.

### Changes by Type

- **docs_regen**: 5 files
- **link_fix**: 3 files
- **selector_insert**: 4 files

### Risk Assessment

- Low risk: 8 files
- Medium risk: 4 files
- High risk: 0 files

### Files Modified

- `src/components/FOLDER.md` (docs_regen, low)
- `docs/README.md` (link_fix, low)
- `src/components/Button.tsx` (selector_insert, medium)
```

#### Skip Approval (Use Carefully)

```bash
repograph autofix /path/to/repo --apply --skip-approval
```

Applies all changes including medium/high-risk. **Not recommended for production.**

### API Endpoints

#### POST /api/autofix/dry-run

Preview fixes without applying.

**Request:**
```json
{
  "repository_path": "/path/to/repo",
  "fix_types": ["docs", "links"]
}
```

**Response:**
```json
{
  "fix_id": "fix-123",
  "summary": {
    "total_files": 5,
    "total_low_risk": 5,
    "total_medium_risk": 0,
    "by_fix_type": {
      "docs_regen": 3,
      "link_fix": 2
    }
  },
  "patches": [...],
  "diff": "unified diff output"
}
```

#### POST /api/autofix/apply

Apply fixes to repository.

**Request:**
```json
{
  "repository_path": "/path/to/repo",
  "fix_types": ["all"],
  "skip_approval": false
}
```

**Response (requires approval):**
```json
{
  "fix_id": "fix-456",
  "status": "requires_approval",
  "applied": [],
  "failed": [],
  "summary": {...},
  "approval_id": "approval-789"
}
```

**Response (success):**
```json
{
  "fix_id": "fix-456",
  "status": "success",
  "applied": ["file1.md", "file2.tsx"],
  "failed": [],
  "summary": {...}
}
```

#### POST /api/autofix/pr

Create pull request with autofixes.

**Request:**
```json
{
  "repository_path": "/path/to/repo",
  "fix_types": ["all"],
  "base_branch": "main",
  "labels": ["autofix", "automated"]
}
```

**Response:**
```json
{
  "fix_id": "fix-123",
  "pr_url": "https://github.com/owner/repo/pull/42",
  "pr_number": "42",
  "branch": "autofix/20250122-143022",
  "status": "created"
}
```

#### POST /api/autofix/approve/{approval_id}

Approve a risky fix.

**Request:**
```json
{
  "comment": "Reviewed and approved"
}
```

**Response:**
```json
{
  "approval_id": "approval-789",
  "status": "approved",
  "approved": true
}
```

#### GET /api/autofix/pending-approvals

List pending approval requests.

**Response:**
```json
{
  "approvals": [
    {
      "id": "approval-123",
      "fix_id": "fix-456",
      "fix_type": "selector_insert",
      "risk_level": "medium",
      "file_count": 4,
      "requested_by": "user@example.com",
      "requested_at": "2025-01-22T14:30:00Z"
    }
  ],
  "count": 1
}
```

## Best Practices

### 1. Always Dry-Run First

```bash
# Preview changes
repograph autofix /path/to/repo --dry-run

# Review output
# If acceptable, apply
repograph autofix /path/to/repo --apply
```

### 2. Use Specific Fix Types

```bash
# Start with safest fixes
repograph autofix /path/to/repo --fix-type docs --apply

# Then try links
repograph autofix /path/to/repo --fix-type links --apply

# Finally, code modifications (requires approval)
repograph autofix /path/to/repo --fix-type selectors --apply
```

### 3. Use PR Mode for Team Review

```bash
# Create PR instead of direct apply
repograph autofix /path/to/repo --pr
```

Team members can review the PR before merging.

### 4. Test After Applying

```bash
# After applying fixes
repograph autofix /path/to/repo --apply

# Run tests
npm test
# or
pytest

# If tests fail, rollback
git checkout HEAD -- <files>
```

### 5. Commit Incrementally

```bash
# Apply one fix type at a time
repograph autofix /path/to/repo --fix-type docs --apply
git add . && git commit -m "docs: regenerate FOLDER.md files"

repograph autofix /path/to/repo --fix-type links --apply
git add . && git commit -m "docs: convert absolute links to relative"
```

## Safety Rules

### What Autofixers Will Never Modify

1. **Generated files** - Detected by patterns and headers
2. **Lock files** - package-lock.json, poetry.lock, etc.
3. **Build outputs** - node_modules/, dist/, target/
4. **Files without changes** - Only creates patches for actual differences
5. **External links** - Preserves https:// external URLs
6. **Already-correct code** - Skips files with test selectors or i18n

### Validation Checks

Before applying, patches are validated:

- **Size limits**: File size must not double unexpectedly
- **Content preservation**: Functions/classes are not removed
- **Pattern matching**: Important code patterns are preserved

### Rollback Capability

Changes can be rolled back:

```bash
# Via git (recommended)
git checkout HEAD -- <file>

# Via autofixer (if in same session)
# Rollback support in patch generator
```

## Metrics and Monitoring

Autofixers track:

- **Fix success rate**: Percentage of fixes applied successfully
- **Files processed**: Total files scanned
- **Skipped files**: Generated files skipped for safety
- **Approval rate**: Percentage of approvals granted
- **Risk distribution**: Low/medium/high risk breakdown

Access metrics via:

```bash
# CLI stats
repograph autofix /path/to/repo --dry-run --verbose

# API endpoint
GET /api/autofix/history
```

## Troubleshooting

### "Working tree has uncommitted changes"

**Error when creating PR:**
```
Error: Working tree has uncommitted changes
```

**Solution:**
```bash
# Commit or stash changes first
git add .
git commit -m "WIP"

# Then create PR
repograph autofix /path/to/repo --pr
```

### "Cannot modify generated file"

**File detected as generated:**
```
ValueError: Cannot modify generated file: schema.gen.ts
```

**This is intentional.** Generated files are protected. Regenerate them using their source tools.

### "Requires approval"

**Medium/high-risk changes blocked:**
```
Status: requires_approval
```

**Solution:**
```bash
# Option 1: Use approval workflow (recommended)
# Approve via API or web UI

# Option 2: Skip approval (not recommended)
repograph autofix /path/to/repo --apply --skip-approval
```

### Formatting Tool Not Found

**Fixer skips files:**
```
Available tools: {}
Languages supported: []
```

**Solution:**
```bash
# Install formatters
pip install black ruff isort
npm install -g prettier eslint

# Verify
black --version
prettier --version
```

## Examples

### Example 1: New Repository Documentation

```bash
# Generate FOLDER.md for all directories
repograph autofix /path/to/new-repo --fix-type docs --apply

# Creates:
# src/FOLDER.md
# src/components/FOLDER.md
# src/utils/FOLDER.md
```

### Example 2: Fix Documentation Links

```bash
# Preview link fixes
repograph autofix /path/to/repo --fix-type links --dry-run

# Apply if looks good
repograph autofix /path/to/repo --fix-type links --apply
```

### Example 3: Add Test Selectors

```bash
# Dry run to see what selectors would be added
repograph autofix /path/to/repo --fix-type selectors --dry-run

# Review output, then apply
repograph autofix /path/to/repo --fix-type selectors --apply

# Requires approval - approve via web UI or API
```

### Example 4: Comprehensive Cleanup PR

```bash
# Create PR with all safe fixes
repograph autofix /path/to/repo --pr

# PR includes:
# - Documentation regeneration
# - Link fixes
# - Test selectors (pending approval)
# - i18n scaffolding (pending approval)
```

## Configuration

### Environment Variables

```bash
# Tenant ID for multi-tenant
export REPOGRAPH_TENANT_ID="tenant-123"

# Skip safety checks (dangerous!)
export REPOGRAPH_UNSAFE_MODE=true
```

### Feature Flags

```python
# In config.py or environment
AUTOFIX_SAFE_V1=true  # Enable autofixer system
```

## Limits and Constraints

- **Performance**: Fix generation <5s, application <10s
- **File size**: No limit, but large files may time out
- **Concurrency**: Not thread-safe (run sequentially)
- **Git requirement**: PR mode requires git and gh CLI

## See Also

- [S1-T5 Implementation Summary](./s1-tLS1-T5-IMPLEMENTATION-SUMMARY.md)
- [API Documentation](./api-reference.md)
- [Safety Patterns](./safety-patterns.md)
