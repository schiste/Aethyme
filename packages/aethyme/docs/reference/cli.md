# Aethyme CLI Reference

Complete reference for the Aethyme command-line interface.

## Installation

```bash
pip install aethyme
```

## Global Options

- `--tenant-id` - Tenant ID for multi-tenant isolation
- `--json` - Output in JSON format
- `--verbose`, `-v` - Verbose output
- `--version` - Show version
- `--help` - Show help

## Commands

### Index Commands
- `aethyme index repo --repo PATH` - Index repository
- `aethyme index status` - Check indexing status
- `aethyme index trigger REPO_ID` - Trigger re-indexing

### Query Commands
- `aethyme query search TERM` - Search for symbols
- `aethyme query ego SYMBOL` - Get ego graph
- `aethyme query impact SYMBOL` - Impact analysis

### AI-Readiness
- `aethyme ai-ready` - Run scorecard

### Autofix
- `aethyme autofix dry-run` - Preview fixes
- `aethyme autofix apply` - Apply fixes
- `aethyme autofix pr` - Create PR with fixes

### Configuration
- `aethyme config show` - Show configuration
- `aethyme config set KEY VALUE` - Set configuration
- `aethyme login` - Authenticate

### Metrics
- `aethyme kpi` - Show KPI report
- `aethyme stats` - Show graph statistics

For detailed documentation, see the full CLI reference.
