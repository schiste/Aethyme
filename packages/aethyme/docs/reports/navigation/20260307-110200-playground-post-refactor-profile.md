# Playground Post-Refactor Profile

- Last Updated: 2026-03-07
- Timestamp: 2026-03-07
- Repo: `/Users/christophehenner/Downloads/Repositories/Aethyme Playground`
- Mode: fresh cold build after cache clear

## Summary

The Rust repograph generator is materially faster on `Aethyme Playground` after:
- splitting and profiling the `code` pass
- moving hot-path relation resolution to interned/index-based lookup
- excluding `.security-logs` from discovery
- tightening config-role classification so generic JSON/YAML/TOML data no longer floods the config pipeline

The most important result:
- total cold build dropped from `52218 ms` to `25253 ms`
- `config_files` dropped from `2495` to `79`
- `configs_read` dropped from `22048 ms` to `30 ms`

## Timeline

### Before downstream config filtering

The build now completed fully and exposed the real downstream bottleneck:

```text
stage=discover_repo duration_ms=3785
stage=structure duration_ms=7526
stage=code_parse_files duration_ms=191
stage=code_normalize_symbols duration_ms=15
stage=code_resolve_imports duration_ms=42
stage=code_resolve_calls duration_ms=3442
stage=code_resolve_references duration_ms=3573
stage=configs_read duration_ms=22048
stage=configs_link duration_ms=2282
stage=docs_read duration_ms=327
stage=docs_link_areas duration_ms=1756
stage=docs_link_files duration_ms=359
stage=docs_link_configs duration_ms=97
stage=docs_link_symbols duration_ms=790
stage=edge_normalization duration_ms=403
stage=overlays duration_ms=304
stage=graph_nodes duration_ms=84
stage=graph_annotations duration_ms=1590
stage=graph_sort duration_ms=121
total_duration_ms=52218
repo_files=106096
source_files=4009
doc_files=1085
config_files=2495
graph_edges=1372552
graph_annotations=4562
```

### After excluding `.security-logs`

Discovery improved, but config inflation still dominated:

```text
stage=discover_repo duration_ms=3254
stage=structure duration_ms=7346
stage=code_parse_files duration_ms=165
stage=code_normalize_symbols duration_ms=12
stage=code_resolve_imports duration_ms=37
stage=code_resolve_calls duration_ms=3321
stage=code_resolve_references duration_ms=3542
stage=configs_read duration_ms=22048
```

This showed that the remaining config cost was not the log directory anymore. It was broad config classification.

### After tightening config-role classification

```text
stage=discover_repo duration_ms=3254
stage=structure duration_ms=7346
stage=code_parse_files duration_ms=165
stage=code_normalize_symbols duration_ms=12
stage=code_resolve_imports duration_ms=37
stage=code_resolve_calls duration_ms=3321
stage=code_resolve_references duration_ms=3542
stage=configs_read duration_ms=30
stage=configs_link duration_ms=19
stage=docs_read duration_ms=249
stage=docs_link_areas duration_ms=1506
stage=docs_link_files duration_ms=364
stage=docs_link_configs duration_ms=67
stage=docs_link_symbols duration_ms=771
stage=edge_normalization duration_ms=227
stage=overlays duration_ms=304
stage=graph_nodes duration_ms=59
stage=graph_annotations duration_ms=1258
stage=graph_sort duration_ms=63
total_duration_ms=25253
repo_files=106096
source_files=4008
doc_files=1085
config_files=79
graph_edges=876436
graph_annotations=2146
```

## Before / After

| Metric | Before | After |
|---|---:|---:|
| Total cold build | `52218 ms` | `25253 ms` |
| `discover_repo` | `3785 ms` | `3254 ms` |
| `structure` | `7526 ms` | `7346 ms` |
| Total `code_*` | `7221 ms` | `7077 ms` |
| `configs_read` | `22048 ms` | `30 ms` |
| `configs_link` | `2282 ms` | `19 ms` |
| `docs_*` total | `3329 ms` | `2957 ms` |
| `graph_annotations` | `1590 ms` | `1258 ms` |
| `config_files` | `2495` | `79` |
| `graph_edges` | `1372552` | `876436` |
| `graph_annotations` count | `4562` | `2146` |

## Interpretation

### What is now working well

- `discover_repo` is no longer the main problem.
- `structure` is heavy but bounded.
- the `code` pass is no longer opaque and no longer stalls the whole build.
- config processing is now cheap enough to stop mattering on this repo.

### What is still expensive

The current dominant costs are now:
- `structure`: `7346 ms`
- `code_resolve_calls`: `3321 ms`
- `code_resolve_references`: `3542 ms`
- `docs_link_areas`: `1506 ms`
- `graph_annotations`: `1258 ms`

The engine is now spending most of its time in:
1. semantic relation resolution
2. area/doc linking
3. annotation generation

### What we learned

- broad “all JSON is config” classification was a major architectural error for large mixed repos
- excluding obviously generated directories matters, but is not enough by itself
- cold-start viability depends as much on **classification discipline** as on parser speed
- the indexed `code` refactor was correct and remains worth keeping

## Key engineering conclusion

The next performance work should focus on:
1. reducing `code_resolve_calls`
2. reducing `code_resolve_references`
3. reducing `docs_link_areas`
4. reducing `graph_annotations`

The next work should **not** go back to:
- config tuning
- log-directory exclusions
- blind graph-normalization changes without data

## Validation

- `cargo test`: passed (`24 passed`)
- `pytest packages/aethyme/tests/docs -q`: passed (`13 passed`)
- `ruff check packages/aethyme/src packages/aethyme/tests`: passed
