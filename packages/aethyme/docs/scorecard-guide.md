# Scorecard Guide

Last Updated: 2026-03-06

Aethyme scorecard is a repository analysis layer on top of the indexed graph.

## What It Does

- runs detector-based checks against a repository
- stores scan results in tenant scope
- exposes scan results through the API and CLI

## Active Entry Points

- API: `POST /api/v1/scorecard/scan`
- CLI: `aethyme ai-ready --repo PATH`

## Output

Scorecard reports include:

- overall score
- blocker, warning, and info counts
- detector findings
- scan timing and file counts

## Rule

Treat scorecard as a useful secondary feature, not as a substitute for graph correctness.
