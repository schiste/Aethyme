"""Typed logger shim for autofixers.

Uses structlog when available, with a stdlib fallback that preserves the
keyword-argument logging call style used by this package.
"""

from __future__ import annotations

import importlib
import logging
from typing import Any, Protocol, cast


class LoggerLike(Protocol):
    """Minimal structured logger protocol used in autofixers."""

    def debug(self, event: str, **kwargs: Any) -> Any: ...
    def info(self, event: str, **kwargs: Any) -> Any: ...
    def warning(self, event: str, **kwargs: Any) -> Any: ...
    def error(self, event: str, **kwargs: Any) -> Any: ...


class _StdlibLoggerAdapter:
    """Compatibility adapter that accepts structlog-style kwargs."""

    def __init__(self, logger: logging.Logger):
        self._logger = logger

    @staticmethod
    def _serialize(event: str, kwargs: dict[str, Any]) -> str:
        if not kwargs:
            return event
        ordered = ", ".join(f"{key}={value!r}" for key, value in sorted(kwargs.items()))
        return f"{event} | {ordered}"

    def debug(self, event: str, **kwargs: Any) -> None:
        self._logger.debug(self._serialize(event, kwargs))

    def info(self, event: str, **kwargs: Any) -> None:
        self._logger.info(self._serialize(event, kwargs))

    def warning(self, event: str, **kwargs: Any) -> None:
        self._logger.warning(self._serialize(event, kwargs))

    def error(self, event: str, **kwargs: Any) -> None:
        self._logger.error(self._serialize(event, kwargs))


def get_logger() -> LoggerLike:
    """Return a logger compatible with structlog-style method signatures."""
    try:
        structlog = importlib.import_module("structlog")
        getter = getattr(structlog, "get_logger", None)
        if callable(getter):
            return cast(LoggerLike, getter())
    except ImportError:
        pass

    return _StdlibLoggerAdapter(logging.getLogger("aethyme.autofixers"))
