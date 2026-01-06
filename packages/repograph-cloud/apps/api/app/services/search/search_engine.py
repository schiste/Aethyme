"""Advanced search engine for code symbols and repositories."""

from typing import List, Dict, Any, Optional
from dataclasses import dataclass
from enum import Enum
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, or_, and_, func
from sqlalchemy.orm import selectinload

from app.models.code_graph import SCIPSymbol, CodeRelationship, SymbolMetadata


class SearchScope(str, Enum):
    """Search scope options."""

    ALL = "all"
    SYMBOLS = "symbols"
    FILES = "files"
    CLASSES = "classes"
    FUNCTIONS = "functions"
    METHODS = "methods"
    VARIABLES = "variables"


class SearchMode(str, Enum):
    """Search mode options."""

    EXACT = "exact"
    FUZZY = "fuzzy"
    REGEX = "regex"
    PREFIX = "prefix"
    CONTAINS = "contains"


@dataclass
class SearchFilter:
    """Search filter criteria."""

    language: Optional[str] = None
    kind: Optional[str] = None  # class, function, method, etc.
    visibility: Optional[str] = None  # public, private, protected
    file_path: Optional[str] = None
    has_docstring: Optional[bool] = None
    min_complexity: Optional[int] = None
    max_complexity: Optional[int] = None
    min_lines: Optional[int] = None
    max_lines: Optional[int] = None
    repository_id: Optional[str] = None


@dataclass
class SearchResult:
    """Individual search result."""

    symbol_id: str
    name: str
    kind: str
    file_path: str
    line: int
    column: int
    signature: Optional[str]
    docstring: Optional[str]
    language: str
    repository_id: str
    score: float  # Relevance score
    highlights: List[str] = None  # Highlighted snippets

    def __post_init__(self):
        if self.highlights is None:
            self.highlights = []


@dataclass
class SearchResponse:
    """Search response with results and metadata."""

    results: List[SearchResult]
    total_count: int
    page: int
    page_size: int
    query: str
    filters: Dict[str, Any]
    facets: Dict[str, Dict[str, int]]  # Faceted search results
    took_ms: float  # Query execution time


class AdvancedSearchEngine:
    """Advanced search engine for code symbols."""

    def __init__(self, db: AsyncSession):
        """Initialize search engine.

        Args:
            db: Database session
        """
        self.db = db

    async def search(
        self,
        query: str,
        scope: SearchScope = SearchScope.ALL,
        mode: SearchMode = SearchMode.CONTAINS,
        filters: Optional[SearchFilter] = None,
        page: int = 1,
        page_size: int = 20,
        include_facets: bool = True,
    ) -> SearchResponse:
        """Execute advanced search query.

        Args:
            query: Search query string
            scope: Search scope (symbols, files, classes, etc.)
            mode: Search mode (exact, fuzzy, regex, etc.)
            filters: Additional filter criteria
            page: Page number (1-indexed)
            page_size: Results per page
            include_facets: Include faceted search results

        Returns:
            SearchResponse with results and metadata
        """
        import time
        start_time = time.time()

        # Build base query
        stmt = select(SCIPSymbol).options(selectinload(SCIPSymbol.metadata))

        # Apply search mode
        stmt = self._apply_search_mode(stmt, query, mode)

        # Apply scope filter
        stmt = self._apply_scope_filter(stmt, scope)

        # Apply additional filters
        if filters:
            stmt = self._apply_filters(stmt, filters)

        # Count total results
        count_stmt = select(func.count()).select_from(stmt.subquery())
        total_result = await self.db.execute(count_stmt)
        total_count = total_result.scalar() or 0

        # Apply pagination
        offset = (page - 1) * page_size
        stmt = stmt.offset(offset).limit(page_size)

        # Execute query
        result = await self.db.execute(stmt)
        symbols = result.scalars().all()

        # Calculate relevance scores and convert to search results
        search_results = []
        for symbol in symbols:
            score = self._calculate_relevance_score(symbol, query, mode)
            highlights = self._generate_highlights(symbol, query, mode)

            search_results.append(SearchResult(
                symbol_id=symbol.symbol_id,
                name=symbol.name,
                kind=symbol.kind,
                file_path=symbol.file_path,
                line=symbol.line,
                column=symbol.column,
                signature=symbol.signature,
                docstring=symbol.docstring,
                language=symbol.language,
                repository_id=symbol.repository_id,
                score=score,
                highlights=highlights,
            ))

        # Sort by relevance score
        search_results.sort(key=lambda x: x.score, reverse=True)

        # Calculate facets
        facets = {}
        if include_facets and total_count > 0:
            facets = await self._calculate_facets(stmt, filters)

        # Calculate execution time
        took_ms = (time.time() - start_time) * 1000

        return SearchResponse(
            results=search_results,
            total_count=total_count,
            page=page,
            page_size=page_size,
            query=query,
            filters=filters.__dict__ if filters else {},
            facets=facets,
            took_ms=round(took_ms, 2),
        )

    def _apply_search_mode(self, stmt, query: str, mode: SearchMode):
        """Apply search mode to query."""
        if not query:
            return stmt

        if mode == SearchMode.EXACT:
            # Exact match
            stmt = stmt.where(SCIPSymbol.name == query)

        elif mode == SearchMode.PREFIX:
            # Prefix match
            stmt = stmt.where(SCIPSymbol.name.like(f"{query}%"))

        elif mode == SearchMode.CONTAINS:
            # Contains (default)
            stmt = stmt.where(SCIPSymbol.name.ilike(f"%{query}%"))

        elif mode == SearchMode.FUZZY:
            # Fuzzy match using similarity
            # This requires pg_trgm extension in PostgreSQL
            # For now, use ILIKE with variations
            variations = [
                f"%{query}%",
                f"%{query.lower()}%",
                f"%{query.upper()}%",
                f"%{query.capitalize()}%",
            ]
            conditions = [SCIPSymbol.name.ilike(v) for v in variations]
            stmt = stmt.where(or_(*conditions))

        elif mode == SearchMode.REGEX:
            # Regex match (PostgreSQL specific)
            stmt = stmt.where(SCIPSymbol.name.op("~")(query))

        return stmt

    def _apply_scope_filter(self, stmt, scope: SearchScope):
        """Apply scope filter to query."""
        if scope == SearchScope.ALL:
            return stmt

        scope_kinds = {
            SearchScope.SYMBOLS: None,  # All symbols
            SearchScope.FILES: ["file"],
            SearchScope.CLASSES: ["class", "interface", "struct"],
            SearchScope.FUNCTIONS: ["function"],
            SearchScope.METHODS: ["method"],
            SearchScope.VARIABLES: ["variable", "field", "property"],
        }

        kinds = scope_kinds.get(scope)
        if kinds:
            stmt = stmt.where(SCIPSymbol.kind.in_(kinds))

        return stmt

    def _apply_filters(self, stmt, filters: SearchFilter):
        """Apply additional filters to query."""
        if filters.language:
            stmt = stmt.where(SCIPSymbol.language == filters.language)

        if filters.kind:
            stmt = stmt.where(SCIPSymbol.kind == filters.kind)

        if filters.file_path:
            stmt = stmt.where(SCIPSymbol.file_path.ilike(f"%{filters.file_path}%"))

        if filters.repository_id:
            stmt = stmt.where(SCIPSymbol.repository_id == filters.repository_id)

        if filters.has_docstring is not None:
            if filters.has_docstring:
                stmt = stmt.where(SCIPSymbol.docstring.isnot(None))
            else:
                stmt = stmt.where(SCIPSymbol.docstring.is_(None))

        # Metadata filters (requires join with SymbolMetadata)
        metadata_filters = []

        if filters.min_complexity is not None:
            metadata_filters.append(SymbolMetadata.cyclomatic_complexity >= filters.min_complexity)

        if filters.max_complexity is not None:
            metadata_filters.append(SymbolMetadata.cyclomatic_complexity <= filters.max_complexity)

        if filters.min_lines is not None:
            metadata_filters.append(SymbolMetadata.lines_of_code >= filters.min_lines)

        if filters.max_lines is not None:
            metadata_filters.append(SymbolMetadata.lines_of_code <= filters.max_lines)

        if metadata_filters:
            stmt = stmt.join(SymbolMetadata).where(and_(*metadata_filters))

        return stmt

    def _calculate_relevance_score(
        self, symbol: SCIPSymbol, query: str, mode: SearchMode
    ) -> float:
        """Calculate relevance score for a search result.

        Score factors:
        - Exact match: 100 points
        - Starts with query: 80 points
        - Contains query: 50 points
        - Has docstring: +10 points
        - Symbol kind (class/function): +5 points
        - Name length (shorter is better): bonus
        """
        score = 0.0
        name_lower = symbol.name.lower()
        query_lower = query.lower()

        # Match quality
        if symbol.name == query:
            score += 100
        elif name_lower == query_lower:
            score += 90
        elif symbol.name.startswith(query):
            score += 80
        elif name_lower.startswith(query_lower):
            score += 70
        elif query in symbol.name:
            score += 60
        elif query_lower in name_lower:
            score += 50
        else:
            score += 30

        # Has documentation
        if symbol.docstring:
            score += 10

        # Symbol kind importance
        kind_scores = {
            "class": 5,
            "function": 5,
            "method": 4,
            "interface": 4,
            "variable": 2,
        }
        score += kind_scores.get(symbol.kind, 0)

        # Name length bonus (shorter names are often more relevant)
        if len(symbol.name) < 20:
            score += 5

        # Normalize to 0-100
        return min(100.0, score)

    def _generate_highlights(
        self, symbol: SCIPSymbol, query: str, mode: SearchMode
    ) -> List[str]:
        """Generate highlighted snippets for search results."""
        highlights = []

        # Highlight in name
        if query.lower() in symbol.name.lower():
            highlights.append(f"Name: {symbol.name}")

        # Highlight in signature
        if symbol.signature and query.lower() in symbol.signature.lower():
            # Extract snippet around match
            idx = symbol.signature.lower().find(query.lower())
            start = max(0, idx - 20)
            end = min(len(symbol.signature), idx + len(query) + 20)
            snippet = symbol.signature[start:end]
            if start > 0:
                snippet = "..." + snippet
            if end < len(symbol.signature):
                snippet = snippet + "..."
            highlights.append(f"Signature: {snippet}")

        # Highlight in docstring
        if symbol.docstring and query.lower() in symbol.docstring.lower():
            # Extract first sentence or 100 chars
            snippet = symbol.docstring[:100]
            if len(symbol.docstring) > 100:
                snippet += "..."
            highlights.append(f"Docs: {snippet}")

        return highlights

    async def _calculate_facets(self, stmt, filters: Optional[SearchFilter]) -> Dict[str, Dict[str, int]]:
        """Calculate faceted search results."""
        facets = {}

        # Language facet
        lang_stmt = select(
            SCIPSymbol.language,
            func.count(SCIPSymbol.symbol_id).label('count')
        ).select_from(stmt.subquery()).group_by(SCIPSymbol.language)

        result = await self.db.execute(lang_stmt)
        facets['language'] = {row[0]: row[1] for row in result if row[0]}

        # Kind facet
        kind_stmt = select(
            SCIPSymbol.kind,
            func.count(SCIPSymbol.symbol_id).label('count')
        ).select_from(stmt.subquery()).group_by(SCIPSymbol.kind)

        result = await self.db.execute(kind_stmt)
        facets['kind'] = {row[0]: row[1] for row in result if row[0]}

        return facets

    async def search_code_content(
        self,
        query: str,
        repository_id: str,
        file_pattern: Optional[str] = None,
        regex: bool = False,
        case_sensitive: bool = False,
    ) -> List[Dict[str, Any]]:
        """Search within code file content.

        This is a basic implementation. For production, use a
        dedicated full-text search engine like Elasticsearch.

        Args:
            query: Code content to search for
            repository_id: Repository to search in
            file_pattern: Optional file path pattern
            regex: Use regex search
            case_sensitive: Case-sensitive search

        Returns:
            List of matches with file, line, and content
        """
        # This would require storing file contents in the database
        # or reading from the filesystem/git repository
        # For now, return a placeholder
        return []
