# Comparative Eval: Aethyme Graph-Mediated Navigation vs Raw Browsing

**Date:** 2026-03-08
**Repository:** `packages/aethyme` (321 files, 809 functions, 194 classes)
**Agent:** Claude Opus 4.6 (interactive mode via Chau7 tabs)
**Engine:** aethyme-engine (Rust, local-first)

## Executive Summary

Graph-mediated navigation consistently saves tokens and time vs raw browsing, with dramatic accuracy improvements on targeted navigation tasks.

| Metric | Explain-Repo | Navigation CTF |
|---|---|---|
| **Score delta** | +54% strict, +18% lenient | +100 points (0→100) |
| **Token savings** | 15.2% | 30.0% |
| **Time savings** | 31.6% | 81.0% |
| **Tool call reduction** | 38.5% | 91.7% |

## Eval 1: Explain-Repo

**Task:** Explore a 321-file polyglot repo (Rust/Python/JS) and produce a structured JSON explanation covering code areas, entrypoints, configs, languages, risk areas, navigation order, and evidence.

### Prompts

- **Baseline:** "Explore the repository directly and produce a structured explanation." (182 chars + schema)
- **Aethyme:** "Use navigation context." (111 chars + pre-computed context pack + schema)

The Aethyme prompt includes anchors, scope, navigation order, risk flags, and CLI commands for iterative graph expansion — all derived from the engine's deterministic analysis.

### Results

| Metric | Baseline | Aethyme |
|---|---|---|
| Strict score (exact match) | 21.67/100 | 33.33/100 |
| Lenient score (substring) | 47.0/100 | 55.3/100 |
| Tokens consumed | 87,282 | 74,006 |
| Wall time | 1m 54s | 1m 18s |
| Tool calls | ~26 | ~16 |

### Behavioral Observations

**Baseline agent:**
- Started with broad `find` (shotgun approach)
- Read 21+ files including many not directly relevant
- Spent tokens on exploratory `ls` and `Read` of peripheral files
- Total exploration was undirected — breadth-first with no priority signal

**Aethyme agent:**
- Started by reading anchor files (README, auth-boundary, cli.rs) per navigation order
- Fewer but more targeted tool calls
- Read supplementary files to fill gaps (engine.py, pyproject.toml, passes/)
- Navigation context gave clear priority signal for what to read first

### Scoring Notes

Both agents produced **descriptive strings** (e.g., `"rust/crates/aethyme-engine/src — Rust engine: ..."`) rather than bare paths (`"rust"`). The strict scorer does exact string matching, penalizing both. Lenient scoring (substring match) gives a truer picture. The Aethyme agent's higher lenient score reflects better coverage of reference areas, code areas, and representative files.

Fields where Aethyme gained most: `reference_areas` (1.0 vs 0.0 strict), `representative_code_files` (+0.33), `representative_docs` (+0.34).

## Eval 2: Navigation CTF

**Task:** "Find the manifest that manages the main code entrypoint in the .github area, identify the entrypoint file it controls, and name the top-level area that owns both."

**Reference answer:**
- Config: `.github/actions/aethyme-scorecard/package.json`
- Code: `.github/actions/aethyme-scorecard/index.js`
- Area: `.github`
- Chain: `package.json → .github` (configures), `package.json → index.js` (entrypoint_for)

### Results

| Metric | Baseline | Aethyme |
|---|---|---|
| Score | **0/100** | **100/100** |
| Tokens consumed | 83,150 | 58,231 |
| Wall time | 1m 19s | ~15s |
| Tool calls | 12 | 1 |

### Behavioral Observations

**Baseline agent (0/100):**
- Searched `**/.github/**/*` but didn't explore the results (collapsed in terminal)
- Read all 5 workflow YAML files (cd.yml, ci.yml, evals.yml, performance.yml, aethyme-example.yml)
- Read `pyproject.toml` and identified `src.cli:main` as the entrypoint
- Concluded `cd.yml` is the "manifest" because it deploys the Docker image
- **Never explored `.github/actions/aethyme-scorecard/`**
- Ironically listed `.github/actions/aethyme-scorecard/action.yml` as a rejected candidate, noting "its index.js is its own self-contained action entrypoint" — recognizing the right file but dismissing it!

**Aethyme agent (100/100):**
- Navigation context pre-identified `package.json` as an anchor
- Made 1 tool call: `Read(.github/actions/aethyme-scorecard/package.json)` to confirm
- Produced a perfect JSON response in ~15 seconds

### Why the Baseline Failed

The graph reveals structural relationships (config → entrypoint, config → area) that aren't visible from filesystem browsing. The baseline agent:

1. **Assumed "manifest" = CI/CD workflow** — a reasonable but wrong interpretation
2. **Focused on `src/cli.py` as the main entrypoint** — correct for the Python package, but the question asks about the `.github` area
3. **Never discovered the `package.json` → `index.js` relationship** because it requires understanding config-to-entrypoint graph edges

The Aethyme engine pre-computed this relationship via its config pass, which extracts `main` fields from `package.json` and links them to the files they reference. This is invisible to breadth-first file exploration.

## Repo Signals

The engine's navigability signals for this repository:

| Signal | Score | Level |
|---|---|---|
| Boundary clarity | 69 | mixed |
| Entrypoint clarity | 100 | strong |
| Config hygiene | 27 | weak |
| Hidden coupling | 41 | weak |
| Parser visibility | 91 | strong |

The **strong entrypoint clarity** (100) and **strong parser visibility** (91) mean the engine can reliably extract and link entrypoints — exactly the capability that drove the CTF result. The **weak config hygiene** (27 — 23 duplicate config families) means there are many configs to sift through, making undirected search harder.

## Combined Summary

| | Explain-Repo | Navigation CTF | Average |
|---|---|---|---|
| **Aethyme score** | 55.3 (lenient) | 100.0 | 77.7 |
| **Baseline score** | 47.0 (lenient) | 0.0 | 23.5 |
| **Token savings** | 15.2% | 30.0% | 22.6% |
| **Time savings** | 31.6% | 81.0% | 56.3% |

## Methodology

- Both agents run Claude Opus 4.6 in interactive mode via Chau7 terminal tabs
- Same repository, same session configuration, same model
- Baseline gets: task description + output schema
- Aethyme gets: task description + pre-computed navigation context (anchors, scope, risk flags, CLI commands) + output schema
- Scoring uses weighted rubric from `src/eval/scoring.py` (explain-repo: set intersection; CTF: exact path match + relationship chain overlap)
- Token counts from Claude Code's terminal display; wall times from Claude's "Cooked/Crunched/Cogitated for Xs" reports

## Artifacts

- Explain-repo artifacts: `/tmp/aethyme-eval-artifacts.json`
- Explain-repo results: `/tmp/aethyme-eval-explain-repo-results.json`
- CTF artifacts: `/tmp/aethyme-eval-ctf-artifacts.json`
- CTF results: `/tmp/aethyme-eval-ctf-results.json`
- Scoring framework: `src/eval/scoring.py`
- Report framework: `src/eval/report.py`
