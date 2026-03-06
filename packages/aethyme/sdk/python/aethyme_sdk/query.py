"""Query API for Aethyme SDK."""

from typing import TYPE_CHECKING

from .models import EgoGraphResult, ImpactAnalysisResult, SearchResult

if TYPE_CHECKING:
    from .client import AethymeClient


class QueryAPI:
    """
    Query API interface.

    Provides methods for searching code and analyzing relationships.
    """

    def __init__(self, client: "AethymeClient"):
        self.client = client

    def search(
        self,
        term: str,
        kind: str | None = None,
        lang: str | None = None,
        limit: int = 20,
        search_type: str = "hybrid",
    ) -> list[SearchResult]:
        """
        Search for symbols in the code graph.

        Args:
            term: Search term
            kind: Filter by symbol kind (function, class, etc.)
            lang: Filter by language
            limit: Maximum number of results
            search_type: Type of search (exact, fuzzy, hybrid)

        Returns:
            List of search results

        Example:
            >>> results = client.query.search("UserService")
            >>> for result in results:
            ...     print(f"{result.symbol} in {result.file_path}")
        """
        params = {
            "query": term,
            "limit": limit,
            "type": search_type,
        }
        if kind:
            params["kind"] = kind
        if lang:
            params["lang"] = lang

        response = self.client.get("/api/v1/search", params=params)
        return [SearchResult(**item) for item in response.get("results", [])]

    def ego_graph(
        self,
        symbol: str,
        depth: int = 2,
        limit: int = 100,
    ) -> EgoGraphResult:
        """
        Get ego graph for a symbol.

        Args:
            symbol: Symbol to analyze
            depth: Graph traversal depth
            limit: Maximum number of nodes

        Returns:
            Ego graph result

        Example:
            >>> ego = client.query.ego_graph("UserService", depth=3)
            >>> print(f"Found {ego.total_nodes} related symbols")
        """
        response = self.client.get(
            f"/api/v1/ego/{symbol}",
            params={"depth": depth, "limit": limit},
        )
        return EgoGraphResult(**response)

    def impact_analysis(
        self,
        symbol: str,
        max_depth: int = 10,
    ) -> ImpactAnalysisResult:
        """
        Analyze impact of changes to a symbol.

        Args:
            symbol: Symbol to analyze
            max_depth: Maximum traversal depth

        Returns:
            Impact analysis result

        Example:
            >>> impact = client.query.impact_analysis("UserService")
            >>> print(f"{impact.total_impacted} symbols would be affected")
        """
        response = self.client.get(
            f"/api/v1/impact/{symbol}",
            params={"max_depth": max_depth},
        )
        return ImpactAnalysisResult(**response)
