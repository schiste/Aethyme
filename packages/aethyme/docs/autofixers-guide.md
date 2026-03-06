# Autofixers Guide

Aethyme keeps autofixers as local repository tooling.

## Current Scope

The active autofixer package provides:
- safety evaluation
- patch generation
- docs regeneration
- markdown link cleanup
- selector insertion
- JSX and Vue i18n scaffolding
- formatting helpers

It does not currently expose a public HTTP API or approval workflow.

## Local Usage

```python
from pathlib import Path

from src.autofixers.patch import PatchGenerator
from src.autofixers.safety import SafetyEngine
from src.autofixers.fixers import DocsRegenerator

repo_path = Path('/path/to/repo')
patches = PatchGenerator(repo_path, SafetyEngine())

for fix in DocsRegenerator(repo_path).create_folder_docs():
    patches.add_patch(
        fix['file_path'],
        fix['original_content'],
        fix['new_content'],
        fix['fix_type'],
    )

preview = patches.dry_run()
print(preview['summary'])
```

## Rules

- treat autofixes as local tooling, not product surface
- run in dry-run mode first
- keep generated-file detection enabled
- only reintroduce a public API after the core loop is stable
