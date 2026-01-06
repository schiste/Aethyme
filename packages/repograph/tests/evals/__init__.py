"""
Evaluation Harnesses for RepoGraph.

Provides comprehensive evaluation frameworks for:
- Retrieval quality (precision, recall, MRR, nDCG)
- Autofix correctness
- Scorecard precision
"""

from .retrieval_eval import (
    RetrievalEvaluator,
    RetrievalTestCase,
    RetrievalMetrics,
    run_retrieval_eval,
)
from .autofix_eval import (
    AutofixEvaluator,
    AutofixTestCase,
    AutofixMetrics,
    run_autofix_eval,
)
from .scorecard_eval import (
    ScorecardEvaluator,
    ScorecardTestCase,
    ScorecardMetrics,
    run_scorecard_eval,
)

__all__ = [
    # Retrieval
    "RetrievalEvaluator",
    "RetrievalTestCase",
    "RetrievalMetrics",
    "run_retrieval_eval",
    # Autofix
    "AutofixEvaluator",
    "AutofixTestCase",
    "AutofixMetrics",
    "run_autofix_eval",
    # Scorecard
    "ScorecardEvaluator",
    "ScorecardTestCase",
    "ScorecardMetrics",
    "run_scorecard_eval",
]
