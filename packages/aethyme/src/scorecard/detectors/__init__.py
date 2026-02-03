"""Detector modules for AI-readiness scorecard."""

from .base import BaseDetector
from .data_ui_coverage import DataUICoverageDetector
from .folder_docs import FolderDocsDetector
from .relative_links import RelativeLinksDetector
from .i18n_gaps import I18nGapsDetector
from .generated_files import GeneratedFilesDetector
from .schema_drift import SchemaDriftDetector
from .route_coverage import RouteCoverageDetector
from .ability_coverage import AbilityCoverageDetector

# Registry of all detectors
ALL_DETECTORS = [
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
