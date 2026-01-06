"""
CLI Plugin System for RepoGraph.

Allows third-party plugins to extend CLI functionality.
"""

import os
import sys
import importlib
import importlib.util
from pathlib import Path
from typing import Dict, List, Any, Optional, Callable
import click
import structlog

logger = structlog.get_logger(__name__)


class PluginMetadata:
    """Metadata for a CLI plugin."""

    def __init__(
        self,
        name: str,
        version: str,
        description: str,
        author: str,
        commands: List[Callable],
    ):
        self.name = name
        self.version = version
        self.description = description
        self.author = author
        self.commands = commands


class PluginManager:
    """
    Manages CLI plugins.

    Plugins can be discovered from:
    - ~/.repograph/plugins/
    - ./plugins/ (current directory)
    - REPOGRAPH_PLUGIN_PATH environment variable
    """

    def __init__(self):
        self.plugins: Dict[str, PluginMetadata] = {}
        self.plugin_paths: List[Path] = self._get_plugin_paths()

    def _get_plugin_paths(self) -> List[Path]:
        """Get paths to search for plugins."""
        paths = []

        # User plugin directory
        user_plugins = Path.home() / ".repograph" / "plugins"
        if user_plugins.exists():
            paths.append(user_plugins)

        # Current directory plugins
        local_plugins = Path.cwd() / "plugins"
        if local_plugins.exists():
            paths.append(local_plugins)

        # Environment variable
        env_paths = os.environ.get("REPOGRAPH_PLUGIN_PATH", "")
        for path_str in env_paths.split(":"):
            if path_str:
                path = Path(path_str)
                if path.exists():
                    paths.append(path)

        return paths

    def discover_plugins(self):
        """Discover and load all available plugins."""
        for plugin_path in self.plugin_paths:
            logger.debug("Searching for plugins", path=str(plugin_path))

            for item in plugin_path.iterdir():
                if item.is_file() and item.suffix == ".py":
                    self._load_plugin_file(item)
                elif item.is_dir() and (item / "__init__.py").exists():
                    self._load_plugin_package(item)

    def _load_plugin_file(self, file_path: Path):
        """Load a plugin from a single Python file."""
        try:
            module_name = f"repograph_plugin_{file_path.stem}"
            spec = importlib.util.spec_from_file_location(module_name, file_path)
            if spec and spec.loader:
                module = importlib.util.module_from_spec(spec)
                sys.modules[module_name] = module
                spec.loader.exec_module(module)

                # Look for plugin metadata
                if hasattr(module, "PLUGIN_METADATA"):
                    metadata = module.PLUGIN_METADATA
                    self.plugins[metadata["name"]] = PluginMetadata(
                        name=metadata["name"],
                        version=metadata["version"],
                        description=metadata["description"],
                        author=metadata["author"],
                        commands=metadata.get("commands", []),
                    )
                    logger.info(
                        "Loaded plugin",
                        name=metadata["name"],
                        version=metadata["version"],
                    )
        except Exception as e:
            logger.error("Failed to load plugin", path=str(file_path), error=str(e))

    def _load_plugin_package(self, package_path: Path):
        """Load a plugin from a Python package."""
        try:
            module_name = f"repograph_plugin_{package_path.name}"
            spec = importlib.util.spec_from_file_location(
                module_name, package_path / "__init__.py"
            )
            if spec and spec.loader:
                module = importlib.util.module_from_spec(spec)
                sys.modules[module_name] = module
                spec.loader.exec_module(module)

                # Look for plugin metadata
                if hasattr(module, "PLUGIN_METADATA"):
                    metadata = module.PLUGIN_METADATA
                    self.plugins[metadata["name"]] = PluginMetadata(
                        name=metadata["name"],
                        version=metadata["version"],
                        description=metadata["description"],
                        author=metadata["author"],
                        commands=metadata.get("commands", []),
                    )
                    logger.info(
                        "Loaded plugin",
                        name=metadata["name"],
                        version=metadata["version"],
                    )
        except Exception as e:
            logger.error("Failed to load plugin", path=str(package_path), error=str(e))

    def register_commands(self, cli_group: click.Group):
        """Register plugin commands with the CLI."""
        for plugin_name, plugin in self.plugins.items():
            for command in plugin.commands:
                try:
                    cli_group.add_command(command)
                    logger.debug(
                        "Registered command",
                        plugin=plugin_name,
                        command=command.name,
                    )
                except Exception as e:
                    logger.error(
                        "Failed to register command",
                        plugin=plugin_name,
                        command=command.name,
                        error=str(e),
                    )

    def list_plugins(self) -> List[Dict[str, Any]]:
        """List all loaded plugins."""
        return [
            {
                "name": p.name,
                "version": p.version,
                "description": p.description,
                "author": p.author,
                "commands": len(p.commands),
            }
            for p in self.plugins.values()
        ]


# Global plugin manager instance
_plugin_manager: Optional[PluginManager] = None


def get_plugin_manager() -> PluginManager:
    """Get the global plugin manager instance."""
    global _plugin_manager
    if _plugin_manager is None:
        _plugin_manager = PluginManager()
        _plugin_manager.discover_plugins()
    return _plugin_manager
