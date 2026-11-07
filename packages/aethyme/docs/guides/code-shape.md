# Code-shape diagnostics

Last Updated: 2026-11-07

`aethyme quality code-shape --repo .` reports source-shape review signals for
tracked JavaScript, TypeScript, and Python files. It uses the current working
contents of tracked files. Findings are advisory by default: they are prompts
for review, not proof that a module needs to be split.

Use `--level error` to return exit status 1 when a finding is present. Status 2
means the command failed or, in error mode, dependency analysis is incomplete;
status 0 means no error-mode findings or an advisory run. Use `--level off` to
disable the check. `--format json` emits the complete versioned report,
including `complete`, `incomplete`, effective policy, and findings.

Create `.aethyme/code-shape.toml` to override only the settings you need; an
explicit `--config PATH` selects another file. The values below are the defaults:

```toml
# Exact names of imports known to refer to packages outside this repository.
external_imports = []
# Import aliases are configured in an optional [aliases] table below.
# Additional behavior-module paths or directory prefixes to omit from findings.
exclude = []

[policy]
level = "warning" # warning, error, or off
min_source_lines = 100
max_source_lines = 300
min_callables = 3
max_callables = 7
min_exports = 1
max_exports = 3
allow_zero_exports = true
min_internal_ratio = 0.3
internal_ratio_min_callables = 4
max_runtime_fan_out = 7
max_runtime_fan_in = 20
max_callable_lines = 50
max_cyclomatic_complexity = 10
allow_runtime_cycles = false
```

All range endpoints are inclusive. Internal ratio applies only at the specified
minimum number of callables. The `--level` flag overrides the configured level.
Invalid settings and unknown keys fail instead of silently falling back.

Generated and vendored files, tests, fixtures, migrations, and stories are
excluded. Facades, entrypoints, adapters, configuration, contracts and
repository tooling are included in dependency analysis but omitted from the
behavior-module findings. Additional `exclude` entries affect findings, not
dependency analysis. The command uses file names and directory conventions to
identify these roles; inspect the scope before using it as a gate.

The dependency graph counts unique resolved in-repository runtime modules;
type-only imports are omitted. Relative imports are resolved against the tracked
source set. Bare package names and aliases may refer to in-repository modules,
so unresolved names are reported as incomplete instead of assumed external.
List an exact name in `external_imports` only after verifying it is external;
for example `external_imports = ["react", "django.db"]`. Dynamic imports and
unresolved relative imports cannot be waived this way. An incomplete report
omits graph-derived findings and error mode exits 2, so it cannot silently pass.
Map local aliases explicitly, for example:

```toml
[aliases]
"@/" = "src/"
```

Alias prefixes end in `/`; their targets are repository-relative directories.
A mapped but unresolved alias remains incomplete even if listed as external.
The resolver does not read TypeScript path configuration or Python package
discovery configuration automatically; repositories using more complex
resolution may need additional resolver support before enforcement is usable.
