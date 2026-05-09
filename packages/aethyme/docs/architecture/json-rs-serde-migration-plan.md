# `json.rs` → `serde_json` migration plan

Last Updated: 2026-05-09

## Context

`rust/crates/aethyme-engine/src/json.rs` is a 1,177-line hand-written
JSON serializer that produces the JSON output for several Aethyme
commands: `aethyme analyze`, `aethyme query`, `aethyme graph`, and
the legacy `inspect` paths. It does NOT serialize `aethyme explore`
output — that path uses `serde_json::to_string_pretty` directly on
`#[derive(Serialize)]`-annotated structs.

The 2026-05-08 cleanup pass surfaced one real correctness bug in the
hand-roll: `escape()` was incomplete per RFC 8259 §7. The fix was a
20-line patch + 30 unit tests; serde_json gets the same case right
by default.

## Why migrate

1. **Eliminate the `escape()` bug class for good.** Any future
   contributor adding a new field to a hand-written builder has to
   remember to call `escape()` correctly. With serde, `#[derive(Serialize)]`
   handles every field uniformly.
2. **Drop ~1,000 lines.** Most of `json.rs` is mechanical field-by-field
   writers that serde generates from struct definitions.
3. **Match the rest of the codebase.** `explore.rs` already uses
   `#[derive(Serialize)]`; the engine response layer is already half
   migrated. Finishing the work removes the seam.

## Why NOT migrate today

The hand-written builders produce specific output formats: exact key
ordering, exact float precision, no whitespace gaps. Downstream
consumers (Python eval scoring code, deployed skills, eval reports)
parse this output. **Switching to serde without byte-compat verification
risks silent breakage** that surfaces only when someone re-runs an old
eval and the new output doesn't match the old reference.

We don't have a baseline-capture mechanism today. Yesterday's 30 unit
tests cover the primitives (`escape`, `string`, enum converters,
`search_hits`) but not the big builders (`repository_map`,
`context_pack`, `inspect_brief`).

## Migration strategy (when scheduled)

### Phase 0 — capture baseline (~3 hours)

For each big public builder (`write_repository_map`, `context_pack`,
`repository_map`, `inspect_brief`, `inspect_structure`,
`write_context_pack`, `write_inspect_structure`):

- Build a minimal but realistic input fixture — typically by calling
  `RepositoryMap::build()` against a fixture repo under `tests/fixtures/`.
- Capture the exact output as a `.json` snapshot under
  `tests/fixtures/json-rs-baselines/`.
- Add a test that re-runs the builder and asserts byte-equivalence
  with the snapshot.

Outcome: a regression net that catches accidental output drift before
the migration starts.

### Phase 1 — migrate the structs (~2 hours)

For each source struct currently formatted by hand:

```rust
// Before (in json.rs):
fn anchor(anchor: &Anchor) -> String {
    format!(
        "{{\"id\":{},\"file\":{},\"reason\":{}}}",
        string(&anchor.id),
        string(&anchor.file),
        string(&anchor.reason),
    )
}

// After (in context_pack.rs / wherever Anchor lives):
#[derive(Serialize)]
pub struct Anchor {
    pub id: String,
    pub file: String,
    pub reason: String,
    // ...
}
```

For fields whose JSON name differs from the Rust name, use
`#[serde(rename = "json_key")]`. For optional fields the hand-roll
emits as `null`, leave default behavior. For optional fields the
hand-roll OMITS, add `#[serde(skip_serializing_if = "Option::is_none")]`.

After each struct's derive lands, run the Phase-0 snapshot tests.
A diff fails the test; iterate until byte-equivalent.

### Phase 2 — replace the builders (~2 hours)

For each public builder in `json.rs`:

```rust
// Before:
pub fn context_pack(pack: &ContextPack) -> String {
    format!("{{\"snapshot\":{},...}}", pack_snapshot(&pack.snapshot), ...)
}

// After:
pub fn context_pack(pack: &ContextPack) -> String {
    serde_json::to_string(pack).expect("ContextPack should serialize")
}
```

Run snapshot tests after each replacement. Don't batch.

### Phase 3 — delete `json.rs` (~30 min)

Once every public builder is forwarding to `serde_json::to_string`,
the helper functions (`escape`, `string`, `string_array`, enum
converters, `write_array`, etc.) become unused. Delete them. The
file shrinks from 1,200+ lines to ~30 (just the public re-exports
or, ideally, gone entirely with the `pub fn`s moved to per-module
locations).

### Phase 4 — eval validation (~1 hour)

Run the full eval suite (GRC bug-fix + MediaWiki bug-fix-1 +
MediaWiki dead-code) end-to-end. The dead-code eval is the most
sensitive to JSON output drift because `dead_code_eval_json` is
scorer input.

If outputs match, ship. If not, the snapshot tests should have
caught it; debug the snapshot diff and roll back the offending
struct change.

## Rough effort & dependencies

| Phase | Effort | Risk | Depends on |
|---|---|---|---|
| 0 — capture baseline | ~3 h | None (just tests) | — |
| 1 — derive structs | ~2 h | Low (snapshot caught) | Phase 0 |
| 2 — replace builders | ~2 h | Medium (still snapshot caught) | Phase 1 |
| 3 — delete dead code | ~30 m | None | Phase 2 |
| 4 — eval validation | ~1 h | Final regression net | Phase 3 |

**Total: ~8.5 hours.** Best done in one focused session so the
intermediate states don't sit on `main`. The mid-migration state has
some structs serde-derived and some still hand-written; both produce
the same JSON, but the readability is worst-case.

## Don't repeat

- **Don't migrate without Phase 0.** Every byte-compat issue I'm
  worried about is invisible until snapshot tests exist. Without
  them, "it compiles" means "downstream consumers may or may not
  still work" and there's no way to tell.
- **Don't batch struct migrations.** One struct, one snapshot
  diff, one commit. Mid-migration debug is easier when the diff
  surface is small.
- **Don't skip Phase 4.** The 30 unit tests + the snapshot tests
  catch contract drift; only a real eval catches behavioral drift
  (e.g. a field that was always present is now `null`-skipped, and
  a downstream consumer counted on its presence).

## Tracked as task #79.
