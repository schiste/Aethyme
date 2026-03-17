---
name: aethyme
description: Use Aethyme's code graph for repository navigation — finding files,
  tracing imports, locating callers, and understanding structure.
---

# Aethyme Navigation

## Setup

```
AETHYME_ROOT="{{AETHYME_ROOT}}"
ENGINE="$AETHYME_ROOT/rust/target/release/aethyme-engine-cli"
```

## Commands

### Structure
```bash
# List top-level code areas
$ENGINE query-areas --repo . --depth 1

# List all areas
$ENGINE query-areas --repo .
```

### Imports
```bash
# What does this file import/depend on?
$ENGINE deps --repo . --file <relative-path>

# What files import this file?
$ENGINE importers --repo . --file <relative-path>
```

### Callers (graph + grep combined)
```bash
# Find all call sites of a function/method/class across the codebase
# Returns file:line:code for each match
$ENGINE callers --repo . --symbol <name>
```

### Overview
```bash
# Structural overview with areas, entrypoints, risks
$ENGINE query-overview --repo .
```

## When to Use

- **Starting a task:** `query-areas --depth 1` to see the codebase structure
- **Finding dependencies:** `deps --file <path>` to see what a file depends on
- **Impact analysis:** `importers --file <path>` to see what depends on a file
- **Finding callers:** `callers --symbol <name>` to find all uses of a function
- **General orientation:** `query-overview` for entrypoints and risk areas

## When NOT to Use

- Don't use Aethyme when a simple `grep` or `find` suffices
- Don't call multiple commands when one answers your question
- If `callers` returns what you need, don't also run `importers`
