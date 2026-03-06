# Aethyme CLI Reference

## Local Verification
- `make test-unit`
- `make test-integration`
- `make test-full`

## Global Options
- `--tenant-id`
- `--json`
- `--verbose`

## Commands

### Indexing
- Canonical contract: [`src/indexing/service.py`](/Users/christophehenner/Downloads/Repositories/Aethyme/packages/aethyme/src/indexing/service.py)
- `aethyme index PATH --name NAME --languages python,typescript --use-fallback`
- `aethyme stats`

### Queries
- `aethyme search TERM --limit 20 --type hybrid`
- `aethyme ego SYMBOL --depth 2 --limit 100`
- `aethyme impact SYMBOL --max-depth 10 --limit 1000`

### Scorecard
- `aethyme ai-ready PATH`

### Autofix
- `aethyme autofix PATH --dry-run`
- `aethyme autofix PATH --apply`
- `aethyme autofix PATH --pr`
