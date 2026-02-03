# Aethyme Autofixers

Safe automated code improvements with approval workflows.

## Quick Start

```python
from pathlib import Path
from src.autofixers.safety import SafetyEngine
from src.autofixers.patch import PatchGenerator
from src.autofixers.fixers import DocsRegenerator, LinkFixer

# Initialize
repo_path = Path("/path/to/repo")
safety_engine = SafetyEngine()
patch_gen = PatchGenerator(repo_path, safety_engine)

# Run a fixer
docs_fixer = DocsRegenerator(repo_path)
fixes = docs_fixer.create_folder_docs()

# Add patches
for fix in fixes:
    patch_gen.add_patch(
        fix["file_path"],
        fix["original_content"],
        fix["new_content"],
        fix["fix_type"]
    )

# Dry run
result = patch_gen.dry_run()
print(result["diff"])

# Apply (if safe)
result = patch_gen.apply(skip_approval=False)
```

## Available Fixers

### DocsRegenerator
Generates FOLDER.md files for directories.

```python
from src.autofixers.fixers import DocsRegenerator

fixer = DocsRegenerator(repo_path)
fixes = fixer.create_folder_docs()
```

### LinkFixer
Converts absolute links to relative.

```python
from src.autofixers.fixers import LinkFixer

fixer = LinkFixer(repo_path)
fixes = fixer.process_directory()
```

### SelectorInserter
Adds data-ui test selectors.

```python
from src.autofixers.fixers import SelectorInserter

fixer = SelectorInserter(repo_path)
fixes = fixer.process_directory()

# Get coverage report
coverage = fixer.get_coverage_report()
```

### I18nScaffolder
Adds i18n scaffolding.

```python
from src.autofixers.fixers import I18nScaffolder

fixer = I18nScaffolder(repo_path)
fixes = fixer.process_directory()

# Generate translation file
fixer.generate_translation_file(
    output_path=Path("translations/en.json"),
    language="en"
)
```

### FormatFixer
Applies code formatting.

```python
from src.autofixers.fixers import FormatFixer

fixer = FormatFixer(repo_path)
info = fixer.get_formatter_info()  # Check available formatters
fixes = fixer.process_directory()
```

## Safety Features

### Generated File Detection

```python
from src.autofixers.safety import GeneratedFileDetector

detector = GeneratedFileDetector()

# Check if file is generated
if detector.is_generated(file_path):
    print("This is a generated file - skipping")

# Filter safe files
safe_files = detector.get_safe_files(all_files)
```

### Risk Assessment

```python
from src.autofixers.safety import SafetyEngine, RiskLevel

engine = SafetyEngine()

# Assess risk
risk = engine.assess_risk(file_path, "docs_regen")

if risk == RiskLevel.HIGH:
    print("Requires approval")
elif risk == RiskLevel.MEDIUM:
    print("Review recommended")
else:
    print("Safe to auto-apply")

# Validate changes
validation = engine.validate_changes(
    original_content,
    new_content,
    file_path
)

if not validation["safe"]:
    print("Unsafe changes:", validation["warnings"])
```

## Patch Operations

### Dry Run

```python
from src.autofixers.patch import PatchGenerator

gen = PatchGenerator(repo_path)

# Add patches...

# Preview without applying
result = gen.dry_run()
print("Summary:", result["summary"])
print("Diff:", result["diff"])
```

### Apply Changes

```python
# Apply with approval checks
result = gen.apply(skip_approval=False)

if result["status"] == "requires_approval":
    print("Need approval for:", result["requires_approval"])
elif result["status"] == "success":
    print("Applied:", result["applied"])
```

### Generate Patch File

```python
# Save unified diff to file
patch_file = gen.save_patch_file(Path("changes.patch"))

# Apply later with: git apply changes.patch
```

## Approval Workflow

```python
from src.autofixers.approval import ApprovalWorkflow

workflow = ApprovalWorkflow(tenant_id="tenant-123")

# Request approval
approval_id = workflow.request_approval(
    fix_id="fix-456",
    fix_type="selector_insert",
    risk_level="medium",
    file_count=5,
    summary={"total_files": 5},
    requested_by="user@example.com"
)

# Check status
status = workflow.get_status(approval_id)

# Approve
workflow.approve(
    approval_id,
    reviewed_by="reviewer@example.com",
    comment="LGTM"
)

# Get pending
pending = workflow.get_pending_approvals()
```

## GitHub Integration

```python
from src.autofixers.github import GitHubIntegration

gh = GitHubIntegration(repo_path)

# Create autofix PR
pr_info = gh.create_autofix_pr(
    patch_generator,
    base_branch="main",
    labels=["autofix", "automated"]
)

if pr_info:
    print(f"PR created: {pr_info['url']}")
```

## Module Structure

```
src/autofixers/
├── __init__.py              # Main exports
├── safety.py                # Safety engine
├── patch.py                 # Patch generator
├── approval.py              # Approval workflow
├── github.py                # GitHub integration
└── fixers/
    ├── __init__.py          # Fixer exports
    ├── base.py              # Base fixer class
    ├── docs_regenerator.py
    ├── link_fixer.py
    ├── selector_inserter.py
    ├── i18n_scaffolder.py
    └── format_fixer.py
```

## Testing

```bash
# Run all tests
pytest tests/autofixers/

# Run specific test file
pytest tests/autofixers/test_safety.py

# Run with coverage
pytest tests/autofixers/ --cov=src/autofixers
```

## Documentation

- [Complete Guide](../../docs/autofixers-guide.md) - Full usage documentation
- [Implementation Summary](../../docs/s1-tLS1-T5-IMPLEMENTATION-SUMMARY.md) - Technical details
- [Deliverables Report](../../S1-T5-DELIVERABLES-REPORT.md) - Final report

## Feature Flag

Enable autofixers with:

```python
AUTOFIX_SAFE_V1 = True
```

## Performance

- Fix generation: <5s (typically 2-3s)
- Fix application: <10s (typically 5-8s)
- Scalable to 2000+ files

## Safety Guarantees

1. Generated files automatically skipped (40+ patterns)
2. Risk-based approval requirements
3. Change validation before apply
4. Dry-run preview always available
5. Rollback support via git
6. Complete audit trail

## License

Part of Aethyme - Enterprise SaaS Platform
