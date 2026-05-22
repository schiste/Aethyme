# Tree-Sitter Grammar Assets

This directory contains generated parser binaries and Aethyme-specific query
files used by the Rust engine.

`manifest.toml` is the machine-readable source of truth for upstream project,
license, checksum, and source-pin status.

## Tracked Assets

| Language | Parser asset | Query file | Upstream project | License |
| --- | --- | --- | --- | --- |
| JavaScript | `javascript/grammar.wasm` | `javascript/queries.scm` | `tree-sitter/tree-sitter-javascript` | MIT |
| PHP | `php/grammar.wasm` | `php/queries.scm` | `tree-sitter/tree-sitter-php` | MIT |
| Python | `python/grammar.wasm` | `python/queries.scm` | `tree-sitter/tree-sitter-python` | MIT |
| Rust | `rust/grammar.wasm` | `rust/queries.scm` | `tree-sitter/tree-sitter-rust` | MIT |
| TypeScript | `typescript/grammar.wasm` | `typescript/queries.scm` | `tree-sitter/tree-sitter-typescript` | MIT |

## Source Pin Status

The checked-in `grammar.wasm` files currently do not record the exact upstream
commit, release tag, tree-sitter CLI version, or generation command.

Before publishing binary releases, regenerate these files from pinned upstream
grammar revisions or replace them with package-managed grammar artifacts.

Development checksum verification:

```bash
packages/aethyme/.venv/bin/python packages/aethyme/scripts/verify-grammar-provenance.py
```

Release verification, expected to fail until all source refs are pinned:

```bash
packages/aethyme/.venv/bin/python packages/aethyme/scripts/verify-grammar-provenance.py --require-pinned
```

## Checksums

```text
fee5c525ac935d9c89bdce41520c023adb091c712492aa3d59059786c2aabd09  javascript/grammar.wasm
24a3fefb6ed747864b8b8a669482fa4987702361532eae99cbf6d467615d73bb  php/grammar.wasm
93310edbdbf785e412a56e6605bee61f401d6e258070df309c7a6e870d2e367b  python/grammar.wasm
252ad74d3ff41aa7214a52ba83be4087e83aac9150061ec81a99cfe504170227  rust/grammar.wasm
c6da2a11fb4d8554d7ea019ebe0780a65917bace9cf198c9e102dd28cd20ed87  typescript/grammar.wasm
```
