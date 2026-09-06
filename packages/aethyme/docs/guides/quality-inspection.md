# Repository quality inspection

Last Updated: 2026-09-06

Aethyme separates operational readiness from optional repository-quality
analysis:

- `aethyme readiness` is the authoritative operational report. It evaluates
  deployment, coordination, agent context, validation, parallel execution,
  graph policy, and upgrade compatibility.
- `aethyme quality inspect` provides bounded, advisory suggestions about
  maintained repository content.
- `aethyme ai-ready` is a supported legacy scorecard alias. Its score does not
  determine whether a repository is ready for agents or broker coordination.

## Inspect maintained repository content

```bash
aethyme quality inspect --repo .
aethyme quality inspect --repo . --format json
```

Quality inspection builds a disposable view from Git's tracked-file index. It
does not scan untracked or ignored files and excludes generated, vendored,
dependency, build, coverage, and virtual-environment content. The active
working-tree bytes of eligible tracked files are inspected, so maintainers can
review quality suggestions before committing a change. The disposable view is
removed after inspection.

Each detector declares whether it applies to the resulting repository view.
The report distinguishes `executed` from `skipped` detectors and explains the
decision. A skipped detector is not reported as a successful quality check.

## Bounded output

Text and JSON reports render at most 100 findings by default. Counts always
describe the complete result, including rendered and omitted findings.

```bash
aethyme quality inspect --repo . --limit 25
aethyme quality inspect --repo . --full
```

Use `--full` only when the complete finding list is necessary. It can be large
on repositories with widespread repeated patterns. Detector selection remains
available for focused inspection:

```bash
aethyme quality inspect --repo . \
  --detectors relative-links,folder-docs
```

Quality severities express suggestion priority (`high`, `medium`, or `low`).
They are not operational blockers, do not change readiness, and never alter
gate selection or broker submission behavior.

## Context-aware relative-path findings

The `relative-links` detector reports absolute filesystem paths in source and
configuration because those values can make a repository environment-specific.
In Markdown, the detector is narrower: it reports absolute destinations in
inline links, reference links, autolinks, and HTML `href` or `src` attributes.
Paths shown as prose, inline examples, indented examples, or fenced code are
not links and do not produce findings.

This distinction applies to `quality inspect`. The legacy `ai-ready` alias
retains its historical broad scan and output contract for compatibility.

## Legacy compatibility

`aethyme ai-ready` retains its historical arguments, report formats, scoring
formula, and exit-code ladder during patch releases. It emits a deprecation
notice explaining the new command split. Scripts that depend on the legacy
scorecard can migrate deliberately; removal or an incompatible output change
requires an explicitly authorized second-digit release.
