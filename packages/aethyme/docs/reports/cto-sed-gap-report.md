# CTO Gap Report: Codex `sed` File Reading Bypasses Token Optimization

**Date:** 2026-03-11
**Context:** Navigation-CTF eval run, 4 conditions, Codex `exec` mode on Playground repos
**Filed by:** Aethyme eval team
**For:** Chau7 CTO team

## Observation

During a controlled eval, the CTO-off and CTO-on conditions produced nearly identical token counts:

| Condition | CTO Setting | Tokens |
|-----------|-------------|--------|
| Control CTO-off | `forceOff` | 55,878 |
| Control CTO-on | `default` (active) | 56,747 |

Expected: CTO-on should show 25-35% fewer tokens (consistent with the 6.2M character reduction / 95% hit rate reported in the CTO usage report). Instead, the delta is <2% — within noise.

## Root Cause

Codex `exec` mode reads files using `sed`, not `cat`:

```
exec /bin/zsh -lc "sed -n '1,220p' packages/auth/package.json"
exec /bin/zsh -lc "sed -n '1,220p' packages/config/src/index.ts"
exec /bin/zsh -lc "sed -n '1,220p' packages/ui/src/index.ts"
```

`sed` is **not in the CTO wrapper list** (`~/.chau7/cto_bin/` — 36 commands, no `sed`). All file content flows to the model uncompressed regardless of CTO setting.

In contrast, if the agent used `cat` (which IS wrapped), CTO would route through `chau7-optim read` and compress the output.

## Why sed Is Hard to Wrap

The CTO wrapper system supports three categories of commands:

| Category | Examples | Optimization strategy |
|----------|----------|----------------------|
| Metadata | `ls`, `find`, `tree`, `git` | Abbreviate listings |
| Content filtration | `cat`, `grep`, `rg`, `diff` | Deduplicate, trim whitespace |
| Tool output | `cargo`, `npm`, `pytest`, `tsc` | Reformat structured output |

`sed` doesn't fit cleanly:

1. **No structured output** — sed produces arbitrary text transformations, not JSON or tabular data
2. **Expression-dependent** — `sed -n '1,220p'` (range print) vs `sed 's/x/y/'` (substitution) produce completely different output shapes
3. **Often piped** — `command | sed '...'` is a streaming filter; optimizing mid-pipe risks data loss
4. **The output IS the content** — unlike `ls` where metadata can be abbreviated, sed's output is the actual file content the agent needs

## However: The Codex Use Case Is Narrow

Codex uses `sed` almost exclusively in one pattern:

```bash
sed -n '1,220p' <file>
```

This is functionally identical to `head -220 <file>` or `cat <file>` (with a line limit). It's a **file read**, not a text transformation. The output is raw file content — exactly what CTO's `read` optimizer already handles for `cat`.

## Proposed Options

### Option A: Add `sed` wrapper with pattern detection

Add `sed` to `ctoRewriteMap` but only optimize when the invocation matches the file-read pattern (`sed -n '<range>p' <file>`). Fall through to the real binary for all other sed patterns.

```
# Pseudo-logic in chau7-optim:
if sed_args match /^-n '\d+,\d+p' <file>$/:
    # This is a file read — route through `read` optimizer
    optimize_as_read(file, start_line, end_line)
else:
    # Unknown sed pattern — fall through (exit code 2)
    exit 2
```

**Pros:** Covers 90%+ of Codex's sed usage. Zero risk for non-read patterns (falls through).
**Cons:** Brittle pattern matching. New sed patterns from future Codex versions would need updating.

### Option B: Wrapper that rewrites `sed -n` to `cat` internally

The wrapper intercepts `sed -n '<range>p' <file>` and internally rewrites it to the existing `cat` optimizer path. No new optimizer subcommand needed.

**Pros:** Reuses existing `read` optimizer. Simple implementation.
**Cons:** Semantically odd (sed wrapper calling cat optimizer). Line range handling needs care.

### Option C: Accept the gap, document it

If Codex changes its file-reading tool in a future version, this gap may resolve itself. CTO already covers 95% of commands by hit rate — `sed` is a niche gap specific to Codex `exec` mode.

**Pros:** No code changes. No maintenance burden.
**Cons:** CTO-off vs CTO-on eval conditions remain indistinguishable when Codex uses `sed`.

## Impact on Eval Design

Until this gap is addressed, the two control conditions (CTO-off and CTO-on) are functionally identical when the agent uses `sed` for file reading. Options:

1. **Merge into one control condition** — run a single control and use the extra slot for a different experimental condition
2. **Force `cat` usage** — modify the eval prompt to instruct the agent to use `cat` instead of `sed` (fragile, model-dependent)
3. **Wait for wrapper fix** — if Option A or B is implemented, the two controls become meaningful again

## References

- CTO wrapper generator: `Chau7/apps/chau7-macos/Sources/Chau7/TokenOptimization/CTOManager.swift` (lines 190-305)
- CTO rewrite map: `Chau7/apps/chau7-macos/Sources/Chau7Core/TokenOptimization.swift` (lines 434-479)
- CTO usage report: `Chau7/docs/reports/2026-03-06-cto-usage-report.md`
- Eval run showing the gap: this document
