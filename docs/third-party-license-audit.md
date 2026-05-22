# Third-Party License Audit

Last updated: 2026-05-22

## Scope

This audit covers the current open-source publication risk areas:

- tracked generated grammar assets under `packages/aethyme/rust/grammars/`
- direct Rust engine/indexer dependency license metadata from `Cargo.lock`
- known gaps before publishing binary artifacts

It does not replace a full dependency audit for Python, pnpm, or container
images.

## Tracked Grammar Assets

The repository tracks five `grammar.wasm` parser binaries plus local query files:

| Asset | Upstream project | License | SHA-256 |
| --- | --- | --- | --- |
| `packages/aethyme/rust/grammars/javascript/grammar.wasm` | `tree-sitter/tree-sitter-javascript` | MIT | `fee5c525ac935d9c89bdce41520c023adb091c712492aa3d59059786c2aabd09` |
| `packages/aethyme/rust/grammars/php/grammar.wasm` | `tree-sitter/tree-sitter-php` | MIT | `24a3fefb6ed747864b8b8a669482fa4987702361532eae99cbf6d467615d73bb` |
| `packages/aethyme/rust/grammars/python/grammar.wasm` | `tree-sitter/tree-sitter-python` | MIT | `93310edbdbf785e412a56e6605bee61f401d6e258070df309c7a6e870d2e367b` |
| `packages/aethyme/rust/grammars/rust/grammar.wasm` | `tree-sitter/tree-sitter-rust` | MIT | `252ad74d3ff41aa7214a52ba83be4087e83aac9150061ec81a99cfe504170227` |
| `packages/aethyme/rust/grammars/typescript/grammar.wasm` | `tree-sitter/tree-sitter-typescript` | MIT | `c6da2a11fb4d8554d7ea019ebe0780a65917bace9cf198c9e102dd28cd20ed87` |

The query files in the same directories appear to be Aethyme-specific minimal
symbol/import queries rather than copied upstream query bundles.

The machine-readable attribution and checksum manifest is
`packages/aethyme/rust/grammars/manifest.toml`.

## Source Pin Gap

The tracked `grammar.wasm` files do not currently record:

- upstream commit or release tag
- generation command
- tree-sitter CLI version
- reproducibility check
- copied upstream license text adjacent to the generated artifact

Before a public binary release, either:

1. regenerate the WASM files from pinned upstream grammar revisions and commit a
   manifest, or
2. remove checked-in parser binaries and fetch/build them from package-managed
   dependencies during the build.

The development verifier checks current file integrity:

```bash
packages/aethyme/.venv/bin/python packages/aethyme/scripts/verify-grammar-provenance.py
```

The release verifier additionally requires pinned upstream refs and CLI
versions:

```bash
packages/aethyme/.venv/bin/python packages/aethyme/scripts/verify-grammar-provenance.py --require-pinned
```

## Direct Rust Dependency License Snapshot

License metadata was read from the current `packages/aethyme/rust/Cargo.lock`
package set and local Cargo registry metadata.

| Package | Version | License |
| --- | ---: | --- |
| `bincode` | `1.3.3` | MIT |
| `blake3` | `1.8.5` | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception |
| `clap` | `4.6.1` | MIT OR Apache-2.0 |
| `data-encoding` | `2.11.0` | MIT |
| `dhat` | `0.3.3` | MIT OR Apache-2.0 |
| `libc` | `0.2.183` | MIT OR Apache-2.0 |
| `once_cell` | `1.21.4` | MIT OR Apache-2.0 |
| `oxc_allocator` | `0.125.0` | MIT |
| `oxc_ast` | `0.125.0` | MIT |
| `oxc_parser` | `0.125.0` | MIT |
| `oxc_span` | `0.125.0` | MIT |
| `ra_ap_syntax` | `0.0.331` | MIT OR Apache-2.0 |
| `rayon` | `1.11.0` | MIT OR Apache-2.0 |
| `redb` | `2.6.3` | MIT OR Apache-2.0 |
| `regex` | `1.12.3` | MIT OR Apache-2.0 |
| `rustpython-ruff_python_ast` | `0.15.8` | MIT |
| `rustpython-ruff_python_parser` | `0.15.8` | MIT |
| `rustpython-ruff_text_size` | `0.15.8` | MIT |
| `serde` | `1.0.228` | MIT OR Apache-2.0 |
| `serde_json` | `1.0.149` | MIT OR Apache-2.0 |
| `sha2` | `0.10.9` | MIT OR Apache-2.0 |
| `tempfile` | `3.27.0` | MIT OR Apache-2.0 |
| `tree-sitter` | `0.25.10` | MIT |
| `tree-sitter-language` | `0.1.7` | MIT |
| `tree-sitter-php` | `0.23.11` | MIT |
| `walkdir` | `2.5.0` | Unlicense/MIT |

No copyleft direct Rust dependency was identified in this pass.

## Follow-Up Before Public Release

- Replace all `UNPINNED` grammar manifest entries with exact upstream refs,
  tree-sitter CLI versions, and regeneration commands.
- Run a full transitive Rust license audit with `cargo metadata` or `cargo deny`
  in an environment with complete dependency cache/network access.
- Run Python and pnpm license audits for cloud, eval UI, and SDK packages.
- Decide whether tracked parser binaries should stay in source control or be
  generated during release builds.
