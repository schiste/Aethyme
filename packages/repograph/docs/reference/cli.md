# RepoGraph CLI Reference

Complete reference for the RepoGraph command-line interface.

## Installation

```bash
pip install repograph
```

## Global Options

- `--tenant-id` - Tenant ID for multi-tenant isolation
- `--json` - Output in JSON format
- `--verbose`, `-v` - Verbose output
- `--version` - Show version
- `--help` - Show help

## Commands

### Index Commands
- `repograph index repo --repo PATH` - Index repository
- `repograph index status` - Check indexing status
- `repograph index trigger REPO_ID` - Trigger re-indexing

### Query Commands
- `repograph query search TERM` - Search for symbols
- `repograph query ego SYMBOL` - Get ego graph
- `repograph query impact SYMBOL` - Impact analysis

### AI-Readiness
- `repograph ai-ready` - Run scorecard

### Autofix
- `repograph autofix dry-run` - Preview fixes
- `repograph autofix apply` - Apply fixes
- `repograph autofix pr` - Create PR with fixes

### Configuration
- `repograph config show` - Show configuration
- `repograph config set KEY VALUE` - Set configuration
- `repograph login` - Authenticate

### Metrics
- `repograph kpi` - Show KPI report
- `repograph stats` - Show graph statistics

For detailed documentation, see the full CLI reference.
