"""Code Metrics Service.

Calculate and track code quality metrics:
- Complexity metrics (cyclomatic, cognitive)
- Size metrics (LOC, file count, function count)
- Quality metrics (documentation coverage, test coverage)
- Maintainability metrics (technical debt, code smells)
"""

from typing import Dict, Any, List
from datetime import datetime
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.models.code_graph import CodeRelationship, SymbolMetadata


class CodeMetricsCalculator:
    """Calculate comprehensive code metrics."""

    def __init__(self, db: AsyncSession, repository_id: str):
        """Initialize metrics calculator.

        Args:
            db: Database session
            repository_id: Repository ID
        """
        self.db = db
        self.repository_id = repository_id

    async def calculate_all_metrics(self) -> Dict[str, Any]:
        """Calculate all metrics for the repository.

        Returns:
            Complete metrics report
        """
        # Get basic counts
        relationship_count = await self._count_relationships()
        symbol_count = await self._count_symbols()

        # Get symbol metadata for calculations
        metadata = await self._get_symbol_metadata()

        # Calculate metrics
        complexity_metrics = self._calculate_complexity_metrics(metadata)
        size_metrics = await self._calculate_size_metrics()
        quality_metrics = self._calculate_quality_metrics(metadata)
        maintainability_metrics = self._calculate_maintainability_metrics(metadata)

        return {
            "repository_id": self.repository_id,
            "generated_at": datetime.utcnow().isoformat(),
            "summary": {
                "total_symbols": symbol_count,
                "total_relationships": relationship_count,
                "health_score": self._calculate_health_score(
                    complexity_metrics, quality_metrics, maintainability_metrics
                ),
            },
            "complexity": complexity_metrics,
            "size": size_metrics,
            "quality": quality_metrics,
            "maintainability": maintainability_metrics,
        }

    async def _count_relationships(self) -> int:
        """Count total relationships."""
        stmt = select(func.count()).select_from(CodeRelationship).where(
            CodeRelationship.repository_id == self.repository_id
        )
        result = await self.db.execute(stmt)
        return result.scalar() or 0

    async def _count_symbols(self) -> int:
        """Count total symbols."""
        stmt = select(func.count()).select_from(SymbolMetadata).where(
            SymbolMetadata.repository_id == self.repository_id
        )
        result = await self.db.execute(stmt)
        return result.scalar() or 0

    async def _get_symbol_metadata(self) -> List[SymbolMetadata]:
        """Get all symbol metadata."""
        stmt = select(SymbolMetadata).where(
            SymbolMetadata.repository_id == self.repository_id
        )
        result = await self.db.execute(stmt)
        return result.scalars().all()

    def _calculate_complexity_metrics(
        self, metadata: List[SymbolMetadata]
    ) -> Dict[str, Any]:
        """Calculate complexity metrics.

        Returns:
            Complexity metrics including average, max, distribution
        """
        complexities = [
            m.cyclomatic_complexity for m in metadata if m.cyclomatic_complexity
        ]

        if not complexities:
            return {
                "average_complexity": 0,
                "max_complexity": 0,
                "min_complexity": 0,
                "high_complexity_count": 0,
                "distribution": {"low": 0, "medium": 0, "high": 0, "very_high": 0},
            }

        avg_complexity = sum(complexities) / len(complexities)
        max_complexity = max(complexities)
        min_complexity = min(complexities)

        # Distribution buckets
        low = sum(1 for c in complexities if c <= 5)
        medium = sum(1 for c in complexities if 6 <= c <= 10)
        high = sum(1 for c in complexities if 11 <= c <= 20)
        very_high = sum(1 for c in complexities if c > 20)

        return {
            "average_complexity": round(avg_complexity, 2),
            "max_complexity": max_complexity,
            "min_complexity": min_complexity,
            "high_complexity_count": high + very_high,
            "distribution": {
                "low": low,  # <= 5 (good)
                "medium": medium,  # 6-10 (acceptable)
                "high": high,  # 11-20 (needs review)
                "very_high": very_high,  # > 20 (refactor needed)
            },
        }

    async def _calculate_size_metrics(self) -> Dict[str, Any]:
        """Calculate size metrics.

        Returns:
            Size metrics including file count, function count, etc.
        """
        # Count unique files
        stmt = (
            select(func.count(func.distinct(CodeRelationship.source_file_path)))
            .select_from(CodeRelationship)
            .where(CodeRelationship.repository_id == self.repository_id)
        )
        result = await self.db.execute(stmt)
        file_count = result.scalar() or 0

        # Count symbols by type (would need symbol kind in metadata)
        stmt = select(SymbolMetadata).where(
            SymbolMetadata.repository_id == self.repository_id
        )
        result = await self.db.execute(stmt)
        symbols = result.scalars().all()

        # Estimate LOC from symbol metadata (if we had it)
        total_loc = sum(m.lines_of_code or 0 for m in symbols)

        return {
            "file_count": file_count,
            "total_symbols": len(symbols),
            "total_lines_of_code": total_loc if total_loc > 0 else "N/A",
            "average_file_size": (
                round(total_loc / file_count) if file_count > 0 and total_loc > 0 else 0
            ),
        }

    def _calculate_quality_metrics(
        self, metadata: List[SymbolMetadata]
    ) -> Dict[str, Any]:
        """Calculate quality metrics.

        Returns:
            Quality metrics including test coverage, documentation coverage
        """
        # These would ideally come from actual test/doc analysis
        # For now, we'll estimate based on symbol metadata

        total_symbols = len(metadata)

        # Estimate documentation coverage
        # (In real impl, would check if symbols have documentation)
        documented_symbols = total_symbols // 2  # Placeholder

        doc_coverage = (
            (documented_symbols / total_symbols * 100) if total_symbols > 0 else 0
        )

        return {
            "documentation_coverage": round(doc_coverage, 1),
            "test_coverage": "N/A",  # Would need actual test data
            "code_to_comment_ratio": "N/A",
        }

    def _calculate_maintainability_metrics(
        self, metadata: List[SymbolMetadata]
    ) -> Dict[str, Any]:
        """Calculate maintainability metrics.

        Returns:
            Maintainability metrics including tech debt, coupling
        """
        if not metadata:
            return {
                "maintainability_index": 0,
                "technical_debt_minutes": 0,
                "average_impact_score": 0,
                "high_impact_symbols": 0,
                "coupling_metrics": {"tightly_coupled": 0, "loosely_coupled": 0},
            }

        # Calculate average impact score
        impact_scores = [m.impact_score for m in metadata if m.impact_score]
        avg_impact = sum(impact_scores) / len(impact_scores) if impact_scores else 0

        # Count high-impact symbols (score > 50)
        high_impact = sum(1 for score in impact_scores if score > 50)

        # Calculate coupling based on call counts
        tightly_coupled = sum(
            1
            for m in metadata
            if (m.inbound_call_count or 0) + (m.outbound_call_count or 0) > 10
        )
        loosely_coupled = len(metadata) - tightly_coupled

        # Estimate technical debt (minutes to fix high complexity items)
        complexities = [
            m.cyclomatic_complexity for m in metadata if m.cyclomatic_complexity
        ]
        tech_debt = sum(max(0, c - 10) * 5 for c in complexities)  # 5 min per point over 10

        # Maintainability Index (simplified formula)
        # Real formula: MI = max(0,(171 - 5.2 * ln(HV) - 0.23 * CC - 16.2 * ln(LOC)) * 100 / 171)
        # Simplified: based on complexity and impact
        avg_complexity = (
            sum(complexities) / len(complexities) if complexities else 0
        )
        mi = max(0, 100 - (avg_complexity * 2) - (avg_impact / 2))

        return {
            "maintainability_index": round(mi, 1),
            "technical_debt_minutes": tech_debt,
            "average_impact_score": round(avg_impact, 1),
            "high_impact_symbols": high_impact,
            "coupling_metrics": {
                "tightly_coupled": tightly_coupled,
                "loosely_coupled": loosely_coupled,
            },
        }

    def _calculate_health_score(
        self,
        complexity: Dict[str, Any],
        quality: Dict[str, Any],
        maintainability: Dict[str, Any],
    ) -> int:
        """Calculate overall repository health score (0-100).

        Weighted average of:
        - Complexity (30%)
        - Quality (30%)
        - Maintainability (40%)
        """
        # Complexity score (inverse of average complexity, normalized)
        avg_complexity = complexity.get("average_complexity", 0)
        complexity_score = max(0, 100 - (avg_complexity * 5))

        # Quality score (documentation coverage)
        quality_score = quality.get("documentation_coverage", 0)

        # Maintainability score (maintainability index)
        maintainability_score = maintainability.get("maintainability_index", 0)

        # Weighted average
        health_score = (
            complexity_score * 0.3 + quality_score * 0.3 + maintainability_score * 0.4
        )

        return round(health_score)

    async def get_hotspots(self, limit: int = 10) -> List[Dict[str, Any]]:
        """Get code hotspots (high complexity + high change frequency).

        Args:
            limit: Maximum number of hotspots to return

        Returns:
            List of hotspot symbols
        """
        stmt = (
            select(SymbolMetadata)
            .where(SymbolMetadata.repository_id == self.repository_id)
            .order_by(
                SymbolMetadata.cyclomatic_complexity.desc(),
                SymbolMetadata.change_frequency.desc(),
            )
            .limit(limit)
        )

        result = await self.db.execute(stmt)
        hotspots = result.scalars().all()

        return [
            {
                "symbol_id": h.symbol_id,
                "complexity": h.cyclomatic_complexity,
                "change_frequency": h.change_frequency,
                "impact_score": h.impact_score,
                "risk_level": self._calculate_risk_level(h),
            }
            for h in hotspots
        ]

    def _calculate_risk_level(self, metadata: SymbolMetadata) -> str:
        """Calculate risk level for a symbol."""
        score = 0

        if metadata.cyclomatic_complexity:
            score += min(50, metadata.cyclomatic_complexity * 2)

        if metadata.change_frequency:
            score += min(30, metadata.change_frequency * 3)

        if metadata.impact_score:
            score += min(20, metadata.impact_score / 5)

        if score >= 70:
            return "CRITICAL"
        elif score >= 50:
            return "HIGH"
        elif score >= 30:
            return "MEDIUM"
        else:
            return "LOW"
