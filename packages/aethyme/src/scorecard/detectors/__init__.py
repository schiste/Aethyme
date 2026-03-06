"""Detector modules for AI-readiness scorecard."""

from typing import TypeAlias

from .ability_coverage import AbilityCoverageDetector
from .base import BaseDetector
from .data_ui_coverage import DataUICoverageDetector
from .folder_docs import FolderDocsDetector
from .generated_files import GeneratedFilesDetector
from .i18n_gaps import I18nGapsDetector
from .relative_links import RelativeLinksDetector
from .route_coverage import RouteCoverageDetector
from .schema_drift import SchemaDriftDetector

DetectorClass: TypeAlias = type[BaseDetector]

# Registry of all detectors
ALL_DETECTORS: list[DetectorClass] = [
    DataUICoverageDetector,
    FolderDocsDetector,
    RelativeLinksDetector,
    I18nGapsDetector,
    GeneratedFilesDetector,
    SchemaDriftDetector,
    RouteCoverageDetector,
    AbilityCoverageDetector,
]

__all__ = [
    "BaseDetector",
    "DataUICoverageDetector",
    "FolderDocsDetector",
    "RelativeLinksDetector",
    "I18nGapsDetector",
    "GeneratedFilesDetector",
    "SchemaDriftDetector",
    "RouteCoverageDetector",
    "AbilityCoverageDetector",
    "ALL_DETECTORS",
]
