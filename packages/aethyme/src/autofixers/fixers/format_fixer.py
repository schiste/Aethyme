"""Format fixer for code formatting and linting issues."""

import subprocess
from pathlib import Path
from typing import Any

from .._log import get_logger
from .base import BaseFixer

logger = get_logger()


class FormatFixer(BaseFixer):
    """Applies automated formatting and linting fixes."""

    # Formatters by language
    FORMATTERS: dict[str, list[str]] = {
        'python': ['black', 'autopep8', 'yapf'],
        'javascript': ['prettier'],
        'typescript': ['prettier'],
        'go': ['gofmt'],
        'rust': ['rustfmt'],
    }

    # Linters with auto-fix
    LINTERS: dict[str, list[str]] = {
        'python': ['ruff', 'isort'],
        'javascript': ['eslint'],
        'typescript': ['eslint'],
    }

    def __init__(self, repo_path: Path, formatter: str | None = None):
        super().__init__(repo_path)
        self.formatter = formatter
        self.available_tools = self._check_available_tools()

    def get_fix_type(self) -> str:
        return "format_fix"

    def can_fix(self, file_path: Path) -> bool:
        """Check if we can format this file."""
        ext = file_path.suffix.lower()

        language_map = {
            '.py': 'python',
            '.js': 'javascript',
            '.jsx': 'javascript',
            '.ts': 'typescript',
            '.tsx': 'typescript',
            '.go': 'go',
            '.rs': 'rust',
        }

        language = language_map.get(ext)
        if not language:
            return False

        # Check if we have a formatter available
        return language in self.available_tools and len(self.available_tools[language]) > 0

    def fix(self, file_path: Path, content: str) -> str | None:
        """Apply formatting to content."""
        ext = file_path.suffix.lower()

        # Map extension to language
        language_map = {
            '.py': 'python',
            '.js': 'javascript',
            '.jsx': 'javascript',
            '.ts': 'typescript',
            '.tsx': 'typescript',
            '.go': 'go',
            '.rs': 'rust',
        }

        language = language_map.get(ext)
        if not language or language not in self.available_tools:
            return None

        # Try each available formatter
        for tool in self.available_tools[language]:
            try:
                formatted = self._run_formatter(tool, file_path, content)
                if formatted and formatted != content:
                    logger.info(
                        "Formatted file",
                        file=str(file_path),
                        tool=tool,
                        size_before=len(content),
                        size_after=len(formatted),
                    )
                    return formatted
            except Exception as e:
                logger.debug(
                    "Formatter failed",
                    file=str(file_path),
                    tool=tool,
                    error=str(e),
                )

        return None

    def _check_available_tools(self) -> dict[str, list[str]]:
        """Check which formatting tools are available."""
        available: dict[str, list[str]] = {}

        for language, tools in self.FORMATTERS.items():
            available[language] = []
            for tool in tools:
                if self._is_tool_available(tool):
                    available[language].append(tool)
                    logger.debug("Found formatter", language=language, tool=tool)

        return available

    def _is_tool_available(self, tool: str) -> bool:
        """Check if a tool is available in PATH."""
        try:
            result = subprocess.run(
                [tool, '--version'],
                capture_output=True,
                timeout=5,
            )
            return result.returncode == 0
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return False

    def _run_formatter(self, tool: str, file_path: Path, content: str) -> str | None:
        """Run a formatter and return formatted content."""
        if tool == 'black':
            return self._run_black(file_path, content)
        elif tool == 'prettier':
            return self._run_prettier(file_path, content)
        elif tool == 'gofmt':
            return self._run_gofmt(file_path, content)
        elif tool == 'rustfmt':
            return self._run_rustfmt(file_path, content)
        elif tool == 'autopep8':
            return self._run_autopep8(file_path, content)
        elif tool == 'ruff':
            return self._run_ruff(file_path, content)
        elif tool == 'isort':
            return self._run_isort(file_path, content)
        elif tool == 'eslint':
            return self._run_eslint(file_path, content)

        return None

    def _run_black(self, file_path: Path, content: str) -> str | None:
        """Run Black formatter."""
        try:
            result = subprocess.run(
                ['black', '--quiet', '-'],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("Black failed", error=str(e))

        return None

    def _run_prettier(self, file_path: Path, content: str) -> str | None:
        """Run Prettier formatter."""
        try:
            # Determine parser from file extension
            parser_map = {
                '.js': 'babel',
                '.jsx': 'babel',
                '.ts': 'typescript',
                '.tsx': 'typescript',
                '.json': 'json',
                '.css': 'css',
                '.md': 'markdown',
            }

            parser = parser_map.get(file_path.suffix, 'babel')

            result = subprocess.run(
                ['prettier', '--parser', parser],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("Prettier failed", error=str(e))

        return None

    def _run_gofmt(self, file_path: Path, content: str) -> str | None:
        """Run gofmt formatter."""
        try:
            result = subprocess.run(
                ['gofmt'],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("gofmt failed", error=str(e))

        return None

    def _run_rustfmt(self, file_path: Path, content: str) -> str | None:
        """Run rustfmt formatter."""
        try:
            result = subprocess.run(
                ['rustfmt', '--emit', 'stdout'],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("rustfmt failed", error=str(e))

        return None

    def _run_autopep8(self, file_path: Path, content: str) -> str | None:
        """Run autopep8 formatter."""
        try:
            result = subprocess.run(
                ['autopep8', '-'],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("autopep8 failed", error=str(e))

        return None

    def _run_ruff(self, file_path: Path, content: str) -> str | None:
        """Run ruff linter with auto-fix."""
        try:
            # Write to temp file since ruff needs file path
            import tempfile

            with tempfile.NamedTemporaryFile(
                mode='w',
                suffix='.py',
                delete=False,
                encoding='utf-8'
            ) as f:
                f.write(content)
                temp_path = f.name

            try:
                # Run ruff fix
                subprocess.run(
                    ['ruff', 'check', '--fix', temp_path],
                    capture_output=True,
                    timeout=30,
                )

                # Read fixed content
                with open(temp_path, encoding='utf-8') as f:
                    return f.read()

            finally:
                Path(temp_path).unlink(missing_ok=True)

        except Exception as e:
            logger.debug("ruff failed", error=str(e))

        return None

    def _run_isort(self, file_path: Path, content: str) -> str | None:
        """Run isort for import sorting."""
        try:
            result = subprocess.run(
                ['isort', '-'],
                input=content.encode('utf-8'),
                capture_output=True,
                timeout=30,
            )

            if result.returncode == 0:
                return result.stdout.decode('utf-8')

        except Exception as e:
            logger.debug("isort failed", error=str(e))

        return None

    def _run_eslint(self, file_path: Path, content: str) -> str | None:
        """Run ESLint with auto-fix."""
        try:
            import tempfile

            with tempfile.NamedTemporaryFile(
                mode='w',
                suffix=file_path.suffix,
                delete=False,
                encoding='utf-8'
            ) as f:
                f.write(content)
                temp_path = f.name

            try:
                # Run eslint fix
                subprocess.run(
                    ['eslint', '--fix', temp_path],
                    capture_output=True,
                    timeout=30,
                )

                # Read fixed content
                with open(temp_path, encoding='utf-8') as f:
                    return f.read()

            finally:
                Path(temp_path).unlink(missing_ok=True)

        except Exception as e:
            logger.debug("eslint failed", error=str(e))

        return None

    def get_formatter_info(self) -> dict[str, Any]:
        """Get information about available formatters."""
        return {
            "available_tools": self.available_tools,
            "languages_supported": list(self.available_tools.keys()),
        }
