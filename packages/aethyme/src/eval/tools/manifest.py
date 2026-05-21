"""Declarative TOML manifest loader for tool adapters.

A manifest fully specifies a tool's eval integration: source repo, install
commands, version probe, and per-condition behavior.  See
``packages/aethyme/evals/tools/graphify.toml`` for a worked example and
``packages/aethyme/docs/architecture/tool-adapters.md`` for the schema doc.

Template substitution
=====================

Three placeholders are expanded in command strings and ``workdir`` fields:

* ``{{TOOL_ROOT}}`` — absolute path to the cloned tool repo on disk.
* ``{{TARGET_REPO}}`` — absolute path to the target repo the eval runs against.
* ``{{TASK_SHELL_ESCAPED}}`` — the per-task prompt text, ``shlex.quote``-d.

Only ``{{TASK_SHELL_ESCAPED}}`` is condition-specific
(``task-conditioned`` only). The first two are stable across the run.

Methodology notes
=================

The ``[notes].condition_mapping`` field is **required** — the loader raises
:class:`ManifestValidationError` when it is absent or empty. This is a
deliberate friction: any manifest that lands in ``evals/tools/`` must
articulate how the tool's modes map to ``explore``/``leverage``/
``task-conditioned``. Sloppy mappings become visible at review time
instead of after publication.
"""

from __future__ import annotations

import shlex
import shutil
import subprocess
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .base import (
    ConditionResult,
    ToolConditionError,
    ToolError,
    ToolInstallError,
)

_TOOL_USING_CONDITIONS: frozenset[str] = frozenset(
    {"explore", "leverage", "task-conditioned"}
)
_CONDITION_TOML_KEY = {
    "explore": "explore",
    "leverage": "leverage",
    "task-conditioned": "task_conditioned",
}


class ManifestValidationError(ToolError):
    """Raised when a TOML manifest fails schema validation."""


@dataclass(frozen=True)
class CommandSpec:
    """One shell command extracted from a manifest section."""

    command: str
    workdir: str | None = None  # may contain {{TOOL_ROOT}} / {{TARGET_REPO}}


@dataclass(frozen=True)
class ConditionSpec:
    """Per-condition manifest section."""

    command: CommandSpec | None  # None for explore when strategy="tool_self_install"
    prompt_header: str = ""
    output_artifact: str | None = None  # path template; if set, file is read into addendum
    output_format: str = "raw"  # "raw" | "markdown" | "json"
    strategy: str | None = None  # explore-only: "tool_self_install" | "skill_doc" | "readme"
    skill_doc: str | None = None  # path relative to manifest, for explore.strategy="skill_doc"


@dataclass(frozen=True)
class ToolManifest:
    """Parsed and validated TOML manifest."""

    path: Path  # path to the .toml file
    name: str
    display_name: str
    homepage: str | None
    git_url: str | None  # None when in_tree=True (e.g. Aethyme itself)
    git_ref: str | None
    in_tree: bool  # True → no clone; tool_root resolves to the package root
    install_commands: tuple[CommandSpec, ...]
    register_commands: tuple[CommandSpec, ...]  # run per target-clone
    version_command: CommandSpec
    warm_command: CommandSpec | None
    conditions: dict[str, ConditionSpec]
    condition_mapping_note: str
    raw: dict[str, Any] = field(repr=False, compare=False, default_factory=dict)


def load_manifest(manifest_path: Path) -> ToolManifest:
    """Parse and validate a TOML manifest file.

    Raises :class:`ManifestValidationError` on missing/invalid fields.
    """
    path = manifest_path.resolve()
    if not path.is_file():
        raise ManifestValidationError(f"manifest not found: {path}")

    with path.open("rb") as fh:
        try:
            raw = tomllib.load(fh)
        except tomllib.TOMLDecodeError as exc:
            raise ManifestValidationError(f"invalid TOML in {path}: {exc}") from exc

    name = _require_str(raw, "name", path)
    display_name = raw.get("display_name", name)
    homepage = raw.get("homepage")

    source = _require_table(raw, "source", path)
    in_tree = bool(source.get("in_tree", False))
    git_url: str | None
    git_ref: str | None
    if in_tree:
        # In-tree tool (Aethyme): no clone, tool_root = package root.
        git_url = None
        git_ref = None
    else:
        git_url = _require_str(source, "git", path, key_path="source.git")
        git_ref = _require_str(source, "ref", path, key_path="source.ref")

    install = _require_table(raw, "install", path)
    install_raw = _require_list(install, "commands", path, key_path="install.commands")
    install_commands = tuple(CommandSpec(command=str(c)) for c in install_raw)
    # Empty install_commands is permitted for in_tree tools (already installed
    # by virtue of being the host package). Required otherwise.
    if not install_commands and not in_tree:
        raise ManifestValidationError(
            f"{path}: [install].commands must be non-empty unless source.in_tree=true"
        )

    register_section = raw.get("register", {})
    register_raw = register_section.get("commands", []) if isinstance(register_section, dict) else []
    register_commands = tuple(CommandSpec(command=str(c)) for c in register_raw)

    version_section = _require_table(raw, "version", path)
    version_command = CommandSpec(
        command=_require_str(version_section, "command", path, key_path="version.command"),
    )

    warm_section = raw.get("warm")
    warm_command: CommandSpec | None = None
    if warm_section is not None:
        warm_command = CommandSpec(
            command=_require_str(warm_section, "command", path, key_path="warm.command"),
            workdir=warm_section.get("workdir"),
        )

    conditions_section = _require_table(raw, "conditions", path)
    conditions: dict[str, ConditionSpec] = {}
    for canonical_name in _TOOL_USING_CONDITIONS:
        toml_key = _CONDITION_TOML_KEY[canonical_name]
        if toml_key not in conditions_section:
            continue
        conditions[canonical_name] = _parse_condition(
            conditions_section[toml_key],
            condition_name=canonical_name,
            manifest_path=path,
        )

    if not conditions:
        raise ManifestValidationError(
            f"{path}: must implement at least one of "
            f"[conditions.explore] / [conditions.leverage] / [conditions.task_conditioned]"
        )

    notes = _require_table(raw, "notes", path)
    condition_mapping = notes.get("condition_mapping", "").strip()
    if not condition_mapping:
        raise ManifestValidationError(
            f"{path}: [notes].condition_mapping is required and must be non-empty. "
            "Articulate how this tool's modes map to explore/leverage/task-conditioned."
        )

    return ToolManifest(
        path=path,
        name=name,
        display_name=display_name,
        homepage=homepage,
        git_url=git_url,
        git_ref=git_ref,
        in_tree=in_tree,
        install_commands=install_commands,
        register_commands=register_commands,
        version_command=version_command,
        warm_command=warm_command,
        conditions=conditions,
        condition_mapping_note=condition_mapping,
        raw=raw,
    )


def _parse_condition(
    section: Any,
    *,
    condition_name: str,
    manifest_path: Path,
) -> ConditionSpec:
    if not isinstance(section, dict):
        raise ManifestValidationError(
            f"{manifest_path}: [conditions.{condition_name}] must be a table"
        )

    strategy = section.get("strategy")
    skill_doc = section.get("skill_doc")
    command_raw = section.get("command")
    workdir = section.get("workdir")

    if condition_name == "explore":
        # explore is the discoverability test; "command" is optional —
        # most of the work is done by the install step. The strategy
        # field declares which skill-doc path was chosen so the report
        # can render it transparently.
        if strategy is None and skill_doc is None and command_raw is None:
            raise ManifestValidationError(
                f"{manifest_path}: [conditions.explore] must declare at least one of "
                "strategy / skill_doc / command"
            )

    command_spec: CommandSpec | None = None
    if command_raw is not None:
        if not isinstance(command_raw, str):
            raise ManifestValidationError(
                f"{manifest_path}: [conditions.{condition_name}].command must be a string"
            )
        command_spec = CommandSpec(command=command_raw, workdir=workdir)

    return ConditionSpec(
        command=command_spec,
        prompt_header=section.get("prompt_header", ""),
        output_artifact=section.get("output_artifact"),
        output_format=section.get("output_format", "raw"),
        strategy=strategy,
        skill_doc=skill_doc,
    )


def _require_str(
    table: dict[str, Any],
    key: str,
    manifest_path: Path,
    *,
    key_path: str | None = None,
) -> str:
    value = table.get(key)
    if not isinstance(value, str) or not value:
        raise ManifestValidationError(
            f"{manifest_path}: {key_path or key} must be a non-empty string"
        )
    return value


def _require_table(
    table: dict[str, Any],
    key: str,
    manifest_path: Path,
) -> dict[str, Any]:
    value = table.get(key)
    if not isinstance(value, dict):
        raise ManifestValidationError(
            f"{manifest_path}: [{key}] table is required"
        )
    return value


def _require_list(
    table: dict[str, Any],
    key: str,
    manifest_path: Path,
    *,
    key_path: str | None = None,
) -> list[Any]:
    value = table.get(key)
    if not isinstance(value, list):
        raise ManifestValidationError(
            f"{manifest_path}: {key_path or key} must be an array"
        )
    return value


# ---------------------------------------------------------------------------
# Adapter
# ---------------------------------------------------------------------------


class ManifestToolAdapter:
    """A :class:`ToolAdapter` driven by a parsed :class:`ToolManifest`.

    State machine:

    1. :meth:`ensure_cloned` — git-clones the tool into ``tool_cache_dir`` and
       runs the install commands.  Called once per eval run, regardless of
       how many condition clones exist.
    2. :meth:`install` — registers the tool into a target repo (e.g. via
       ``graphify install --claude``).  Called once per condition-clone.
    3. :meth:`warm` — optional pre-eval warming.  Called once per
       condition-clone.
    4. :meth:`run_condition` — per (condition, clone, task).
    """

    def __init__(
        self,
        manifest: ToolManifest,
        tool_cache_dir: Path,
    ) -> None:
        self.manifest = manifest
        self.tool_cache_dir = Path(tool_cache_dir).resolve()
        self._installed = False

    @property
    def name(self) -> str:
        return self.manifest.name

    @property
    def display_name(self) -> str:
        return self.manifest.display_name

    @property
    def tool_root(self) -> Path:
        if self.manifest.in_tree:
            # In-tree tool: package root is the source of truth, no clone.
            # _PACKAGE_ROOT lives in registry.py; we derive it locally to
            # avoid an import cycle.
            return Path(__file__).resolve().parents[3]
        return self.tool_cache_dir / self.manifest.name

    def render_command(
        self,
        spec_or_template: CommandSpec | str,
        *,
        target_repo: Path | None = None,
        task: str | None = None,
    ) -> str:
        """Render a manifest command through the single shell-safe path."""
        template = (
            spec_or_template.command
            if isinstance(spec_or_template, CommandSpec)
            else spec_or_template
        )
        return self._substitute(template, target_repo=target_repo, task=task)

    def render_workdir(
        self,
        workdir_template: str | None,
        *,
        target_repo: Path,
    ) -> Path:
        """Render a manifest workdir through the single non-shell path."""
        return self._resolve_workdir(workdir_template, target_repo=target_repo)

    def render_path(
        self,
        path_template: str,
        *,
        target_repo: Path,
    ) -> Path:
        """Render a manifest path that will be passed as a filesystem path."""
        return Path(
            self._substitute(
                path_template,
                target_repo=target_repo,
                quote_paths=False,
            )
        )

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def ensure_cloned(self) -> None:
        """Clone (or fast-forward) the tool source and run its install commands.

        For ``in_tree=True`` tools, skips cloning entirely — the package root
        is the tool root by definition. Install commands still run if present
        (e.g. ``pipx install <self>`` style bootstrapping).
        """
        if self._installed:
            return

        root = self.tool_root

        if not self.manifest.in_tree:
            if not root.exists():
                root.parent.mkdir(parents=True, exist_ok=True)
                self._run_or_raise(
                    ["git", "clone", "--depth", "1", "--branch", self.manifest.git_ref,
                     self.manifest.git_url, str(root)],
                    cwd=root.parent,
                    phase="clone",
                )

        for spec in self.manifest.install_commands:
            cmd_str = self.render_command(spec, target_repo=None)
            self._run_or_raise(
                cmd_str,
                cwd=root,
                phase="install",
                shell=True,
            )

        self._installed = True

    def version(self) -> str:
        self.ensure_cloned()
        cmd = self.render_command(self.manifest.version_command, target_repo=None)
        try:
            out = subprocess.check_output(
                cmd, cwd=self.tool_root, shell=True, text=True, timeout=30,
            )
        except subprocess.CalledProcessError as exc:
            raise ToolInstallError(
                f"version probe failed for {self.name}: {exc}"
            ) from exc
        return out.strip()

    def install(self, target_repo: Path) -> None:
        """Run the ``[register].commands`` against ``target_repo``.

        ``[install]`` is for tool-level setup (clone, build, install deps).
        ``[register]`` is for per-clone setup that registers the tool with
        a specific target repo (e.g. ``graphify install --claude --workdir
        <target>``, ``aethyme repo deploy-skills <target>``).

        Tools that don't need per-clone registration omit the ``[register]``
        block entirely — this method becomes a no-op after ensuring the
        tool itself is cloned/installed.
        """
        self.ensure_cloned()
        target = Path(target_repo).resolve()
        for spec in self.manifest.register_commands:
            cmd_str = self.render_command(spec, target_repo=target)
            workdir = self.render_workdir(spec.workdir, target_repo=target)
            self._run_or_raise(cmd_str, cwd=workdir, phase="register", shell=True)

    def warm(self, target_repo: Path) -> None:
        self.ensure_cloned()
        if self.manifest.warm_command is None:
            return
        cmd = self.render_command(self.manifest.warm_command, target_repo=target_repo)
        workdir = self.render_workdir(
            self.manifest.warm_command.workdir, target_repo=target_repo
        )
        self._run_or_raise(cmd, cwd=workdir, phase="warm", shell=True)

    def implements(self, condition: str) -> bool:
        return condition in self.manifest.conditions

    def run_condition(
        self,
        condition: str,
        target_repo: Path,
        task: str,
    ) -> ConditionResult:
        if condition not in self.manifest.conditions:
            raise ToolConditionError(
                f"tool {self.name!r} does not implement condition {condition!r}"
            )
        spec = self.manifest.conditions[condition]

        if condition == "explore":
            # Discoverability test — no prompt addendum, by design.
            return ConditionResult(
                prompt_addendum="",
                metadata={"strategy": spec.strategy or "command"},
            )

        if spec.command is None:
            raise ToolConditionError(
                f"{self.name!r}: [conditions.{condition}] has no command"
            )

        cmd = self.render_command(spec.command, target_repo=target_repo, task=task)
        workdir = self.render_workdir(spec.command.workdir, target_repo=target_repo)

        try:
            result = subprocess.run(
                cmd,
                cwd=workdir,
                shell=True,
                text=True,
                capture_output=True,
                timeout=300,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise ToolConditionError(
                f"{self.name!r} {condition} timed out after 300s"
            ) from exc

        if result.returncode != 0:
            raise ToolConditionError(
                f"{self.name!r} {condition} failed (exit {result.returncode}): "
                f"{result.stderr.strip()[:500]}"
            )

        addendum_body = self._resolve_addendum_body(spec, target_repo, result.stdout)
        prompt_addendum = (spec.prompt_header or "") + addendum_body

        artifact_path: Path | None = None
        if spec.output_artifact:
            artifact_path = self.render_path(spec.output_artifact, target_repo=target_repo)

        return ConditionResult(
            prompt_addendum=prompt_addendum,
            raw_output=addendum_body,
            artifact_path=artifact_path,
            metadata={"exit_code": result.returncode},
        )

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _resolve_addendum_body(
        self,
        spec: ConditionSpec,
        target_repo: Path,
        stdout: str,
    ) -> str:
        if spec.output_artifact:
            artifact = self.render_path(spec.output_artifact, target_repo=target_repo)
            if artifact.is_file():
                return artifact.read_text(encoding="utf-8")
            # Artifact promised but not produced — surface stdout so the
            # operator can see what happened, but flag it.
            return f"<!-- expected artifact {artifact} not produced -->\n{stdout}"
        return stdout

    def _substitute(
        self,
        template: str,
        *,
        target_repo: Path | None,
        task: str | None = None,
        quote_paths: bool = True,
    ) -> str:
        """Expand the ``{{...}}`` placeholders in a command/path string.

        ``quote_paths`` controls whether ``{{TOOL_ROOT}}`` and
        ``{{TARGET_REPO}}`` are shlex-quoted on substitution. Default
        True — appropriate for shell-command strings, where a path with
        spaces (e.g. ``Mockup - Aethyme``) would otherwise be parsed as
        multiple tokens by the shell. Pass False when substituting into
        a string that becomes a non-shell argument (e.g. a ``workdir``
        that's later passed as ``cwd=`` to subprocess) — there the path
        is delivered verbatim to ``chdir`` and quoting would corrupt it.
        """
        tool_root_str = str(self.tool_root)
        target_repo_str = (
            str(Path(target_repo).resolve()) if target_repo is not None else None
        )
        if quote_paths:
            tool_root_str = shlex.quote(tool_root_str)
            if target_repo_str is not None:
                target_repo_str = shlex.quote(target_repo_str)

        out = template.replace("{{TOOL_ROOT}}", tool_root_str)
        if target_repo_str is not None:
            out = out.replace("{{TARGET_REPO}}", target_repo_str)
        if task is not None:
            out = out.replace("{{TASK_SHELL_ESCAPED}}", shlex.quote(task))
        return out

    def _resolve_workdir(
        self,
        workdir_template: str | None,
        *,
        target_repo: Path,
    ) -> Path:
        if workdir_template is None:
            return self.tool_root
        # workdir becomes a Path arg to subprocess(cwd=...), not a shell
        # token — paths must NOT be shlex-quoted here or chdir fails on
        # any path with spaces.
        return Path(
            self._substitute(
                workdir_template, target_repo=target_repo, quote_paths=False
            )
        )

    def _run_or_raise(
        self,
        cmd: str | list[str],
        *,
        cwd: Path,
        phase: str,
        shell: bool = False,
    ) -> None:
        try:
            subprocess.run(
                cmd,
                cwd=cwd,
                shell=shell,
                check=True,
                timeout=600,
            )
        except subprocess.CalledProcessError as exc:
            raise ToolInstallError(
                f"{self.name!r} {phase} failed (exit {exc.returncode}): {cmd!r}"
            ) from exc
        except subprocess.TimeoutExpired as exc:
            raise ToolInstallError(
                f"{self.name!r} {phase} timed out after 600s: {cmd!r}"
            ) from exc

    # ------------------------------------------------------------------
    # Debug
    # ------------------------------------------------------------------

    def uninstall_cache(self) -> None:
        """Remove the cloned tool dir.  Idempotent; used by cleanup phase."""
        if self.tool_root.exists():
            shutil.rmtree(self.tool_root)
        self._installed = False
