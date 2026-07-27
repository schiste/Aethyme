"""Surface-freeze guard for the python-retirement migration (Phase 0).

Regenerates the CLI surface via scripts/migration/dump-cli-surface.py and
diffs it against the committed docs/architecture/cli-surface-v1.md. Any
surface change — added/removed command, renamed flag, changed default —
fails here until the freeze doc is regenerated and committed alongside a
deliberate decision (see python-retirement-plan.md).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = PACKAGE_ROOT / "scripts" / "migration" / "dump-cli-surface.py"
FROZEN = PACKAGE_ROOT / "docs" / "architecture" / "cli-surface-v1.md"


def test_cli_surface_matches_frozen_doc() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT)],
        capture_output=True,
        text=True,
        cwd=PACKAGE_ROOT,
    )
    assert result.returncode == 0, f"surface dump failed: {result.stderr}"
    current = result.stdout.strip()
    frozen = FROZEN.read_text(encoding="utf-8").strip()
    assert current == frozen, (
        "CLI surface drifted from docs/architecture/cli-surface-v1.md.\n"
        "If this change is deliberate, regenerate the freeze doc\n"
        "(.venv/bin/python scripts/migration/dump-cli-surface.py > "
        "docs/architecture/cli-surface-v1.md)\n"
        "and record the decision in python-retirement-plan.md."
    )
