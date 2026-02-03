# AI-Readiness Scorecard Guide

The AI-Readiness Scorecard is a comprehensive analysis tool that evaluates your codebase for compatibility with AI agents and automated tooling.

## Overview

The scorecard runs 8 specialized detectors that identify common issues that can hinder AI agent effectiveness:

1. **Data-UI Coverage** - Checks for test selectors on interactive elements
2. **Folder Documentation** - Verifies FOLDER.md documentation exists
3. **Relative Links** - Detects absolute paths that should be relative
4. **I18n Gaps** - Finds hardcoded strings that should be internationalized
5. **Generated Files** - Detects manual edits to auto-generated code
6. **Schema Drift** - Checks for type safety issues
7. **Route Coverage** - Verifies API endpoints are documented
8. **Ability Coverage** - Checks for permission/authorization definitions

## Using the CLI

### Basic Scan

```bash
# Scan current directory
aethyme ai-ready

# Scan specific repository
aethyme ai-ready --repo /path/to/repo

# Output to file
aethyme ai-ready --format md --output scorecard.md
```

### Format Options

```bash
# Markdown output (default)
aethyme ai-ready --format md

# JSON output
aethyme ai-ready --format json

# Both formats
aethyme ai-ready --format both
```

### Selective Scanning

Run only specific detectors:

```bash
aethyme ai-ready --detectors data-ui-coverage,folder-docs,relative-links
```

### Exit Codes

The CLI returns different exit codes based on findings:

- `0` - Repository is AI-ready (score >= 90, no blockers/warnings)
- `1` - Warnings present (score >= 50, or warnings but no blockers)
- `2` - Blockers present (score < 50, or any blockers)

This allows integration with CI/CD pipelines:

```bash
#!/bin/bash
aethyme ai-ready --repo .
exit_code=$?

if [ $exit_code -eq 2 ]; then
    echo "BLOCKER issues found! Fix before deploying agents."
    exit 1
elif [ $exit_code -eq 1 ]; then
    echo "Warnings found. Consider addressing before deployment."
fi
```

## Using the API

### Trigger a Scan

```bash
POST /api/v1/scorecard/scan
Content-Type: application/json
Authorization: Bearer <token>

{
  "repository_id": "abc-123",
  "detectors": ["data-ui-coverage", "folder-docs"]  // optional
}
```

Response:
```json
{
  "scan_id": "def-456",
  "status": "pending",
  "message": "Scan queued for repository example-repo"
}
```

### Get Scan Results

```bash
GET /api/v1/scorecard/results/{scan_id}
Authorization: Bearer <token>
```

Response:
```json
{
  "scan_id": "def-456",
  "repository_id": "abc-123",
  "tenant_id": "tenant-789",
  "timestamp": "2025-11-22T12:00:00Z",
  "score": 85,
  "summary": {
    "total_findings": 12,
    "blockers": 0,
    "warnings": 8,
    "info": 4
  },
  "findings": {
    "blockers": [],
    "warnings": [...],
    "info": [...]
  },
  "detectors": [...],
  "performance": {
    "total_scan_time_ms": 1234.5,
    "files_scanned": 150
  }
}
```

### Get Latest Summary

```bash
GET /api/v1/scorecard/summary/{repository_id}
Authorization: Bearer <token>
```

### Get Scan History

```bash
GET /api/v1/scorecard/history/{repository_id}?limit=10
Authorization: Bearer <token>
```

### List Available Detectors

```bash
GET /api/v1/scorecard/checks
```

## Understanding Findings

### Severity Levels

- **BLOCKER** - Must be fixed before deploying agents (e.g., manual edits to generated files)
- **WARNING** - Should be addressed for optimal agent performance (e.g., missing test selectors)
- **INFO** - Suggestions that may improve agent effectiveness (e.g., missing validators)

### Score Calculation

The overall score (0-100) is calculated as:

```
Score = 100 - (blockers * 20) - (warnings * 5) - (info * 1)
```

With caps:
- Maximum penalty from blockers: 100 points (5 blockers)
- Maximum penalty from warnings: 100 points (20 warnings)
- Maximum penalty from info: 10 points (10 info findings)

### Evidence Links

Each finding includes:
- `file_path` - Relative path to the file
- `line_number` - Specific line (when applicable)
- `evidence` - Code snippet showing the issue
- `suggestion` - Recommended fix

## Best Practices

### For High Scores

1. **Add data-ui attributes** to all interactive elements:
   ```tsx
   <button data-ui="submit-button" onClick={handleSubmit}>
     Submit
   </button>
   ```

2. **Create FOLDER.md** in important directories:
   ```markdown
   # components/

   React components for the application.

   ## Structure
   - `Button.tsx` - Reusable button component
   - `Form.tsx` - Form wrapper with validation
   ```

3. **Use relative links** in documentation:
   ```markdown
   <!-- Good -->
   See [API docs](./docs/api.md)

   <!-- Bad -->
   See [API docs](/Users/john/project/docs/api.md)
   ```

4. **Internationalize strings**:
   ```tsx
   // Good
   <h1>{t('welcome.title')}</h1>

   // Bad
   <h1>Welcome to Our Application</h1>
   ```

5. **Never edit generated files** - modify the generator/template instead

6. **Use specific types** instead of `any`:
   ```typescript
   // Good
   interface User {
     id: string;
     data: UserData;
   }

   // Bad
   interface User {
     id: string;
     data: any;
   }
   ```

7. **Document API routes**:
   ```python
   @router.post("/users")
   async def create_user(data: UserCreate):
       """
       Create a new user.

       Args:
           data: User creation data

       Returns:
           Created user with ID
       """
       return {"id": "123"}
   ```

### Continuous Monitoring

Run scorecard scans:
- **Before major refactoring** - Establish baseline
- **In CI/CD pipeline** - Catch regressions
- **Weekly/Monthly** - Track improvements over time
- **Before agent deployment** - Verify readiness

### GitHub Actions Integration

```yaml
name: AI-Readiness Check

on: [push, pull_request]

jobs:
  scorecard:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Run AI-Readiness Scorecard
        run: |
          aethyme ai-ready --format both --output scorecard
        continue-on-error: true

      - name: Upload Report
        uses: actions/upload-artifact@v3
        with:
          name: scorecard-report
          path: |
            scorecard-report.json
            scorecard-report.md

      - name: Check for Blockers
        run: |
          aethyme ai-ready
          if [ $? -eq 2 ]; then
            echo "::error::Blocker issues found!"
            exit 1
          fi
```

## Metrics and Monitoring

The scorecard exports Prometheus metrics:

- `aethyme_scorecard_scans_total` - Total scans per tenant/repo
- `aethyme_scorecard_scan_duration_seconds` - Scan duration histogram
- `aethyme_scorecard_current_score` - Current score gauge
- `aethyme_scorecard_blocker_count` - Number of blockers
- `aethyme_scorecard_findings_total` - Findings by severity/detector

Query examples:

```promql
# Average score by repository
avg(aethyme_scorecard_current_score) by (repository_id)

# Scan duration p95
histogram_quantile(0.95, aethyme_scorecard_scan_duration_seconds_bucket)

# Total blockers across all repos
sum(aethyme_scorecard_blocker_count)
```

## Troubleshooting

### Scan Fails

If a scan fails:
1. Check repository path is accessible
2. Verify file permissions
3. Check logs for detector-specific errors
4. Try running individual detectors to isolate issue

### High False Positive Rate

If detectors flag too many false positives:
1. Review detector configuration
2. Add patterns to skip lists
3. File an issue with examples

### Performance Issues

For large repositories:
1. Run selective detectors first
2. Exclude build/generated directories
3. Increase scan timeout
4. Use incremental scanning (future feature)

## Advanced Usage

### Custom Detector Configuration

Future versions will support custom configuration:

```json
{
  "detectors": {
    "data-ui-coverage": {
      "required_elements": ["button", "input", "select"],
      "ignore_patterns": ["**/*.test.tsx"]
    },
    "folder-docs": {
      "required_dirs": ["src", "components"],
      "optional_dirs": ["utils"]
    }
  }
}
```

### Integration with Autofixers

The scorecard integrates with autofixers (S1-T5):

```bash
# Scan and preview fixes
aethyme ai-ready --format md
aethyme autofix --dry-run

# Apply safe fixes
aethyme autofix --apply

# Create PR with fixes
aethyme autofix --pr
```

## FAQ

**Q: How long does a scan take?**
A: Most medium-sized repos scan in under 10 seconds. Large monorepos may take 30-60 seconds.

**Q: Can I run scans offline?**
A: Yes, the CLI works entirely offline. The API requires connectivity.

**Q: Are scans deterministic?**
A: Yes, the same codebase will always produce the same findings.

**Q: Can I customize severity levels?**
A: Not yet, but this is planned for a future release.

**Q: How does RLS work with API scans?**
A: Scans are isolated by tenant_id. Users can only trigger/view scans for their own repositories.

**Q: Can I scan private repositories?**
A: Yes, both local paths and API-based scans support private repos.

## Related Documentation

- [API Reference](./reference/api.md) - Full API documentation
- [CLI Reference](./reference/cli.md) - Complete CLI command reference
- [Autofixers Guide](./autofixer-guide.md) - Using autofixers with scorecard
- [Architecture](./architecture.md) - Scorecard implementation details
