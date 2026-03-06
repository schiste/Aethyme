# Aethyme Autofixers

Local repository fixers used by the Aethyme core.

## Scope

The current autofixer package is a local tooling layer. It is not mounted as a public API.

Active pieces:
- safety checks
- patch generation
- docs regeneration
- markdown link fixes
- selector insertion
- JSX/Vue i18n scaffolding
- formatting helpers
- optional GitHub PR integration

Removed from the active package surface:
- approval workflow
- unmounted autofix HTTP API

## Example

```python
from pathlib import Path

from src.autofixers.patch import PatchGenerator
from src.autofixers.safety import SafetyEngine
from src.autofixers.fixers import DocsRegenerator

repo_path = Path("/path/to/repo")
safety = SafetyEngine()
patches = PatchGenerator(repo_path, safety)

fixer = DocsRegenerator(repo_path)
for fix in fixer.create_folder_docs():
    patches.add_patch(
        fix["file_path"],
        fix["original_content"],
        fix["new_content"],
        fix["fix_type"],
    )

preview = patches.dry_run()
print(preview["summary"])
```
