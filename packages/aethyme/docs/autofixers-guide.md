# Autofixers Guide

Last Updated: 2026-03-06

Autofixers are local operator tools. They are not a first-class API product surface.

## Supported Modes

- dry run
- apply to disk
- create PR flow

## Supported Fix Types

- docs
- links
- selectors
- i18n
- format

## Entry Point

```bash
cd packages/aethyme
. .venv/bin/activate
python -m src.cli autofix /absolute/path/to/repo --dry-run
```

## Rule

Keep autofixers narrow and testable. Do not market them as a broad autonomous platform.
