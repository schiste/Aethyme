"""Exercise actual Aexeo and Aethyme executables against content-gate v1.

Usage: python3 test-aexeo-v1-interop.py /absolute/path/aexeo-cli /absolute/path/aethyme
Both executables must be built independently. No site or CMS data is modified.
"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def invoke(args, expected, env=None):
    result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=60, check=False)
    if result.returncode != expected:
        raise RuntimeError(f"{args[0]}: expected exit {expected}, got {result.returncode}: {result.stderr}")
    return result


def run_case(root, aexeo, aethyme, name, text, media_type, expected):
    case = root / name
    case.mkdir()
    digest = hashlib.sha256(text.encode()).hexdigest()
    change = {
        "schema": "content-gate.change/v1", "change_id": name,
        "created_at": "2026-01-01T00:00:00Z", "origin": {"kind": "manual"},
        "base": {"kind": "opaque", "revision": "r1"},
        "entries": [{"entity_id": "article", "operation": "create", "media_type": media_type,
                     "after": {"inline": text, "sha256": digest}}],
    }
    (case / "change.json").write_text(json.dumps(change), encoding="utf-8")
    (case / "article.txt").write_text(text, encoding="utf-8")
    (case / "preset.toml").write_text(
        "version=1\nname='test'\n[[gates]]\nid='terms'\nkind='forbidden_terms'\nterms=['bad']\n",
        encoding="utf-8",
    )
    (case / "options.json").write_text("{}", encoding="utf-8")
    validator = [str(aexeo), "content-change", "--preset", "test", "--preset-source", f"test={case}/preset.toml"]
    env = dict(os.environ, CONTENT_GATE_CHANGE_FILE=str(case / "change.json"),
               CONTENT_GATE_VERDICT_FILE=str(case / "verdict.json"))
    invoke(validator, expected, env)
    verdict = json.loads((case / "verdict.json").read_text(encoding="utf-8"))
    if verdict.get("status") != name:
        raise RuntimeError(f"{name}: unexpected verdict status {verdict.get('status')!r}")
    manifest = {
        "change": str(case / "change.json"), "policy": str(case / "preset.toml"),
        "options": str(case / "options.json"), "collection": None, "external": None,
        "additional": {}, "content": {"article": str(case / "article.txt")},
        "validator": validator, "env": {"PATH": "/usr/bin:/bin"}, "timeout_seconds": 45,
    }
    path = case / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    command = [str(aethyme), "content-gate", "run", str(path)]
    invoke(command, 0 if expected == 0 else 2)
    if name == "pass":
        (case / "article.txt").write_text("Changed after snapshot.", encoding="utf-8")
        invoke(command, 2)
    print(f"{name}: Aexeo exit {expected}; Aethyme {'passed and rejected a changed candidate' if expected == 0 else 'blocked'}")


def main():
    if len(sys.argv) != 3 or not all(Path(arg).is_absolute() and Path(arg).is_file() for arg in sys.argv[1:]):
        raise SystemExit(__doc__)
    aexeo, aethyme = map(Path, sys.argv[1:])
    with tempfile.TemporaryDirectory(prefix="aethyme-aexeo-v1-") as directory:
        root = Path(directory)
        run_case(root, aexeo, aethyme, "pass", "A clear article.", "text/plain", 0)
        run_case(root, aexeo, aethyme, "fail", "A bad article.", "text/plain", 1)
        run_case(root, aexeo, aethyme, "not_checked", "{}", "application/json", 2)


if __name__ == "__main__":
    main()
