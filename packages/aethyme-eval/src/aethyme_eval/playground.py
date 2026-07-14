"""Playground setup command helpers."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class PlaygroundSetup:
    source: str
    name: str
    commit: str
    dest: Path
    force: bool = False

    def command(self, aethyme_root: Path) -> tuple[str, ...]:
        script = aethyme_root / "scripts" / "eval" / "setup-playground.sh"
        command = [
            str(script),
            "--source",
            self.source,
            "--name",
            self.name,
            "--commit",
            self.commit,
            "--dest",
            str(self.dest),
        ]
        if self.force:
            command.append("--force")
        return tuple(command)
