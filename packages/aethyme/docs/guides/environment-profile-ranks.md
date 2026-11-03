# Environment profile ranks

Last Updated: 2026-11-03

Profiles may share a rank when they represent equivalent minimum environment
levels. Selecting another profile at the inferred rank does not require a
reason; selecting a higher rank still does. Inference keeps the default
profile when rules fire only at the default rank, and selects the first rule
that raises the minimum to a higher rank when multiple rules tie there.

The `duplicate_rank` validation diagnostic is no longer emitted. Consumers of
validation diagnostics should not rely on that diagnostic to enforce unique
ranks.
