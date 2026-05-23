"""Methodology hashing + golden snapshots for AethymeBench.

The *methodology_hash* is a content fingerprint over the framework
contract — not the implementation. Two runs with the same hash are
directly comparable; two runs with different hashes are by definition
not directly comparable. See ``docs/architecture/methodology.md`` §7
for the canonical input list.

Inputs (in canonical-JSON order):

* ``framework_version`` — semver/CalVer string from ``pyproject.toml``
  (manually mirrored here until Stage C swaps to CalVer)
* ``contract_versions`` — persisted schema versions from
  ``src.contracts.versions.contract_versions()``
* ``conditions`` — serialized form of ``CONDITIONS`` from the
  orchestrator (the 5+1 condition matrix)
* ``prompts_sha256`` — whitespace-normalized SHA256 of
  ``src/eval/prompts.py``
* ``scoring_sha256`` — whitespace-normalized SHA256 of
  ``src/eval/scoring.py``
* ``manifests`` — for every ``evals/tools/*.toml``, the
  whitespace-normalized SHA256 of its content, keyed by filename stem

The hash is the 12-character hex prefix of SHA256 over a canonical
JSON document of those inputs. The framework writes a golden snapshot
to ``docs/methodology/snapshots/<hash>.json`` so the inputs that
produced any given hash are auditable forever.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import asdict, is_dataclass
from pathlib import Path
from typing import Any

from src.contracts.versions import contract_versions

# ---------------------------------------------------------------------------
# Module constants
# ---------------------------------------------------------------------------

# Until Stage C swaps to CalVer, this mirrors the version in
# ``pyproject.toml``. Bump in lockstep with the package version so the
# methodology_hash flips when the framework version flips, even if no
# other input changed.
FRAMEWORK_VERSION = "0.1.0"

# 12 hex chars ≈ 48 bits — collision-resistant within the human-readable
# space of "framework versions ever shipped" (orders of magnitude
# fewer than 2^48). Keeps the hash short enough to paste into report
# headers without being unwieldy.
_HASH_PREFIX_LEN = 12

_PACKAGE_ROOT = Path(__file__).resolve().parents[2]
_PROMPT_FILE = _PACKAGE_ROOT / "src" / "eval" / "prompts.py"
_SCORING_FILE = _PACKAGE_ROOT / "src" / "eval" / "scoring.py"
_MANIFEST_DIR = _PACKAGE_ROOT / "evals" / "tools"
SNAPSHOT_DIR = _PACKAGE_ROOT / "docs" / "methodology" / "snapshots"


# ---------------------------------------------------------------------------
# Whitespace normalization + digest helpers
# ---------------------------------------------------------------------------


def _normalize_text(text: str) -> str:
    """Return *text* normalized so cosmetic whitespace edits don't flip the hash.

    Specifically:

    * strip a leading UTF-8 BOM if present
    * convert CRLF and CR line endings to LF
    * strip trailing whitespace from every line
    * collapse runs of blank lines to a single blank line
    * strip trailing blank lines (no terminal newline either)

    Intentional non-goals: this is NOT a code formatter. Substantive
    edits (renaming a function, reordering scorer logic, rewording a
    prompt template) all flip the hash. Only purely cosmetic edits
    (a stray trailing space, an extra blank line between paragraphs)
    are filtered out — those carry no methodological meaning.
    """
    if text.startswith("﻿"):
        text = text[1:]
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    lines = [line.rstrip() for line in text.split("\n")]
    out: list[str] = []
    prev_blank = False
    for line in lines:
        if line == "":
            if prev_blank:
                continue
            prev_blank = True
        else:
            prev_blank = False
        out.append(line)
    while out and out[-1] == "":
        out.pop()
    return "\n".join(out)


def _sha256_text(text: str) -> str:
    """Hex SHA256 of *text* encoded as UTF-8."""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _normalize_and_digest(path: Path) -> str:
    """Return the whitespace-normalized SHA256 of *path*'s text content."""
    return _sha256_text(_normalize_text(path.read_text(encoding="utf-8")))


# ---------------------------------------------------------------------------
# Input collection
# ---------------------------------------------------------------------------


def _serialize_conditions() -> list[dict[str, Any]]:
    """Return the orchestrator's CONDITIONS tuple as a list of plain dicts.

    Imported lazily to avoid pulling the full orchestrator module at
    import time (this module is imported by ``report.py`` early in the
    eval-run lifecycle).
    """
    from .orchestrator import CONDITIONS

    out: list[dict[str, Any]] = []
    for cond in CONDITIONS:
        if is_dataclass(cond):
            out.append(asdict(cond))
        else:  # defensive — shouldn't happen with current ConditionSpec
            out.append(dict(cond.__dict__))
    return out


def _manifest_digests() -> dict[str, str]:
    """Return ``{manifest_name: sha256}`` for every TOML in evals/tools/.

    Keys are filename stems (``aethyme`` from ``aethyme.toml``) so the
    canonical-JSON form sorts predictably across platforms.

    All manifests are included — methodology_hash describes the
    *framework*, not any specific invocation. Adding a new manifest is
    a methodology change even if no current eval-run references it.
    """
    digests: dict[str, str] = {}
    if not _MANIFEST_DIR.is_dir():
        return digests
    for path in sorted(_MANIFEST_DIR.glob("*.toml")):
        digests[path.stem] = _normalize_and_digest(path)
    return digests


def methodology_inputs() -> dict[str, Any]:
    """Return the dict of inputs the methodology_hash is computed over.

    Keep this stable: any change to the *shape* of this dict is itself
    a methodology change. Adding a new top-level key is a versioning
    event — bump ``FRAMEWORK_VERSION`` so old snapshots remain
    interpretable against their original schema.
    """
    return {
        "framework_version": FRAMEWORK_VERSION,
        "contract_versions": contract_versions(),
        "conditions": _serialize_conditions(),
        "prompts_sha256": _normalize_and_digest(_PROMPT_FILE),
        "scoring_sha256": _normalize_and_digest(_SCORING_FILE),
        "manifests": _manifest_digests(),
    }


# ---------------------------------------------------------------------------
# Hash + snapshot
# ---------------------------------------------------------------------------


def _canonical_json(value: Any) -> str:
    """Serialize *value* in the canonical form used for hashing.

    ``sort_keys=True`` enforces deterministic key order; the compact
    ``separators`` strip cosmetic whitespace. The two together mean the
    hash is invariant under dict-insertion order and Python version.
    """
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def methodology_hash() -> str:
    """Return the 12-char hex methodology_hash for the current framework state."""
    digest = hashlib.sha256(_canonical_json(methodology_inputs()).encode("utf-8")).hexdigest()
    return digest[:_HASH_PREFIX_LEN]


def write_snapshot(snapshot_dir: Path | None = None) -> Path:
    """Write a golden snapshot of the current methodology inputs.

    The snapshot filename is ``<methodology_hash>.json``. If a snapshot
    with that hash already exists the call is a no-op (the file is left
    alone — same hash means same inputs). Returns the snapshot path.

    Snapshots are the audit trail: every methodology_hash that ever
    appears in run metadata should have a corresponding committed
    snapshot in ``docs/methodology/snapshots/``. CI should fail if a
    run produces a hash whose snapshot is missing — that means
    methodology drifted without a commit.
    """
    target_dir = snapshot_dir or SNAPSHOT_DIR
    target_dir.mkdir(parents=True, exist_ok=True)
    inputs = methodology_inputs()
    digest = hashlib.sha256(_canonical_json(inputs).encode("utf-8")).hexdigest()
    hash_short = digest[:_HASH_PREFIX_LEN]
    snapshot_path = target_dir / f"{hash_short}.json"
    if snapshot_path.exists():
        return snapshot_path
    payload = {
        "methodology_hash": hash_short,
        "sha256_full": digest,
        "inputs": inputs,
    }
    snapshot_path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return snapshot_path


def load_snapshot(hash_or_path: str) -> dict[str, Any]:
    """Load a snapshot by its 12-char hash or by explicit path.

    Accepts either ``<hash>`` (resolved against SNAPSHOT_DIR) or a
    full path string. Raises FileNotFoundError otherwise.
    """
    candidate = Path(hash_or_path)
    if not candidate.is_file():
        candidate = SNAPSHOT_DIR / f"{hash_or_path}.json"
    if not candidate.is_file():
        raise FileNotFoundError(f"No methodology snapshot for {hash_or_path!r}")
    return json.loads(candidate.read_text(encoding="utf-8"))


def diff_snapshots(a: str, b: str) -> list[str]:
    """Return a human-readable list of changed-input lines between two snapshots.

    Each output line names one input field that differs. Intended to
    answer "why did the hash flip between commit X and commit Y" with a
    line-by-line breakdown — not for programmatic diffing (use
    ``load_snapshot`` for that).
    """
    snap_a = load_snapshot(a)
    snap_b = load_snapshot(b)
    inputs_a = snap_a.get("inputs", {})
    inputs_b = snap_b.get("inputs", {})
    lines: list[str] = []
    keys = sorted(set(inputs_a) | set(inputs_b))
    for key in keys:
        va = inputs_a.get(key)
        vb = inputs_b.get(key)
        if va == vb:
            continue
        if isinstance(va, dict) and isinstance(vb, dict):
            sub_keys = sorted(set(va) | set(vb))
            for sk in sub_keys:
                sva = va.get(sk)
                svb = vb.get(sk)
                if sva != svb:
                    lines.append(f"{key}.{sk}: {sva!r} → {svb!r}")
        elif isinstance(va, list) and isinstance(vb, list):
            lines.append(f"{key}: list changed ({len(va)} → {len(vb)} entries)")
        else:
            lines.append(f"{key}: {va!r} → {vb!r}")
    return lines
