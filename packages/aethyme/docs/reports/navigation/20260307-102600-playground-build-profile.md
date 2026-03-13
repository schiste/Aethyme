Last Updated: 2026-03-07

# Aethyme Playground Build Profile

## Summary

A fresh repograph build on `/Users/christophehenner/Downloads/Repositories/Aethyme Playground` was profiled with live stage output from the Rust engine.

The build did not complete within a usable cold-start window. The important result is that the bottleneck is now isolated:

- `discover_repo` completed in `4003 ms`
- `structure` completed in `7715 ms`
- the next stage (`code`) never completed before the run was stopped
- the `aethyme-engine-cli build-profile` process stayed at roughly `99% CPU` for more than `3 minutes`

That means the dominant cold-start cost is in the Rust `code` pass, not in:

- broken symlink handling
- generated-directory exclusion
- Python wrapper overhead
- graph overview ranking

## What Was Changed Before Profiling

These engine improvements were implemented first:

1. broken symlink skipping in discovery
2. broader generated-directory exclusion:
   - `.venv*`
   - `coverage`
   - `.turbo`
   - `.cache`
3. line counting only for likely text files under a size threshold
4. token-driven semantic lookup instead of scanning all global function/class names for every function body
5. a new Rust `build-profile` command with live stage timing emission

All validation stayed green:

- `cargo test`: passed
- `ruff check src tests`: passed

## Live Profile Output

Observed output from the fresh build:

```text
Cleared cache for /Users/christophehenner/Downloads/Repositories/Aethyme Playground
stage=discover_repo duration_ms=4003
stage=structure duration_ms=7715
```

No later stage output was emitted before termination.

## Process Observation

While the build was running:

- command:
  - `./rust/target/debug/aethyme-engine-cli build-profile --repo /Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- CPU:
  - approximately `99%`
- elapsed time:
  - more than `3 minutes`

This strongly indicates the engine was actively working inside the `code` pass rather than deadlocked.

## Interpretation

### What the numbers mean

`discover_repo` at ~4 seconds is high but still tolerable for a large mixed repo.

`structure` at ~7.7 seconds is also expensive, but it is not the dominant problem.

The real problem is that `code` does not finish in a practical cold-start budget on this repo shape.

### Likely root causes inside `code`

The current `code` pass still does expensive work across a large source set:

- reads and parses every supported source file
- builds global maps for functions/classes
- resolves imports
- resolves cross-file calls
- resolves cross-file references

Even after the token-driven optimization, the pass is still too expensive on a repo with thousands of TS/TSX/Python files.

## Current Engineering Insight

The next optimization work should not target:

- repo discovery
- overview ranking
- Python wrapper caching

It should target the `code` pass itself.

## Recommended Next Steps

1. split the `code` pass into live sub-stages:
   - parse files
   - normalize symbols
   - resolve imports
   - resolve calls
   - resolve references
2. rerun the profile to isolate which `code` sub-stage dominates
3. optimize the dominant sub-stage before doing any further repo-level validation on `Aethyme Playground`

## Bottom Line

The current Rust repograph generator is functionally correct, but its cold-start performance on a large mixed repo is limited primarily by the `code` pass.

That is now measured, not guessed.
