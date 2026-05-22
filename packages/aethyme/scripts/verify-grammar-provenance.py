#!/usr/bin/env python3
"""Verify tracked tree-sitter grammar asset provenance.

Default mode verifies the manifest shape and grammar.wasm checksums.
Release mode (`--require-pinned`) also requires exact upstream source refs and
tree-sitter CLI versions so generated parser binaries are reproducible.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import tomllib
from pathlib import Path


UNPINNED_VALUES = {"", "UNPINNED", "UNKNOWN", "TODO"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_pinned(value: object) -> bool:
    if not isinstance(value, str):
        return False
    return value.strip().upper() not in UNPINNED_VALUES


def verify_manifest(manifest_path: Path, require_pinned: bool) -> list[str]:
    errors: list[str] = []
    data = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    grammar_root = manifest_path.parent
    grammars = data.get("grammar", [])

    if data.get("manifest_version") != 1:
        errors.append("manifest_version must be 1")
    if not isinstance(grammars, list) or not grammars:
        errors.append("manifest must define at least one [[grammar]] entry")
        return errors

    seen_languages: set[str] = set()
    required_fields = {
        "language",
        "wasm",
        "queries",
        "upstream",
        "license",
        "source_ref",
        "tree_sitter_cli_version",
        "sha256",
    }

    for grammar in grammars:
        if not isinstance(grammar, dict):
            errors.append("grammar entries must be TOML tables")
            continue

        language = str(grammar.get("language", "<missing>"))
        missing = sorted(required_fields - set(grammar))
        if missing:
            errors.append(f"{language}: missing fields: {', '.join(missing)}")
            continue

        if language in seen_languages:
            errors.append(f"{language}: duplicate language entry")
        seen_languages.add(language)

        wasm_path = grammar_root / str(grammar["wasm"])
        query_path = grammar_root / str(grammar["queries"])
        if not wasm_path.is_file():
            errors.append(f"{language}: missing wasm file {wasm_path}")
        else:
            expected_sha256 = grammar["sha256"]
            if not isinstance(expected_sha256, str) or len(expected_sha256) != 64:
                errors.append(f"{language}: invalid sha256 value")
            elif sha256(wasm_path) != expected_sha256:
                errors.append(f"{language}: sha256 mismatch for {wasm_path}")

        if not query_path.is_file():
            errors.append(f"{language}: missing query file {query_path}")

        if grammar["license"] != "MIT":
            errors.append(f"{language}: expected MIT license, got {grammar['license']!r}")

        if require_pinned:
            if not is_pinned(grammar["source_ref"]):
                errors.append(f"{language}: source_ref is not pinned")
            if not is_pinned(grammar["tree_sitter_cli_version"]):
                errors.append(f"{language}: tree_sitter_cli_version is not pinned")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "rust" / "grammars" / "manifest.toml",
        help="Path to grammar provenance manifest.",
    )
    parser.add_argument(
        "--require-pinned",
        action="store_true",
        help="Fail unless every grammar has a pinned source_ref and tree_sitter_cli_version.",
    )
    args = parser.parse_args()

    errors = verify_manifest(args.manifest, args.require_pinned)
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1

    mode = "release" if args.require_pinned else "development"
    print(f"grammar provenance ok ({mode} mode)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
