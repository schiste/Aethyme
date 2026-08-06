# `explore-summary` byte-parity goldens

Last Updated: 2026-08-06

`<case>.input.json` is an `explore --format answer-json` document;
`<case>.expected.json` is the exact compact projection
`aethyme explore-summary --from` must print for it, byte for byte
including the trailing newline.

**Where the expected bytes come from.** They were produced by CPython's
`json.dumps(..., indent=2)` — the oracle the retired
`tests/local/test_explore_summary_cli.py` built in-process and compared
against — captured once during the python-retirement Phase 7 port and
frozen here. Freezing beats reimplementing the oracle in Rust: a Rust
oracle could drift toward the Rust implementation and quietly stop being
an independent check, whereas these bytes cannot change on their own.

The contract they pin (see `docs/architecture/cross-process-consumers.md`,
"Router reader commands"):

- key order and the exact six top-level keys — no `schema_version`, by
  contract decision, so the skill's "inspect only these fields" list
  stays true;
- missing fields render as `null` rather than being omitted;
- 3 lanes, 2 targets per lane, 6 targets overall, 3 verification steps;
- `subsystem` tagging (`role` when truthy, else the lane `id`, never
  overwriting an existing key) and its append-last position;
- `ensure_ascii` escaping, including astral characters as surrogate
  pairs.

Do not regenerate these files to make a failing test pass. A diff here
means the deployed skill's projection step changed shape in every
enhanced repo.
