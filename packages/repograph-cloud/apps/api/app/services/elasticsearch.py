"""Elasticsearch service for code indexing and search."""

import logging
from typing import List, Dict, Any, Optional
from datetime import datetime
from elasticsearch import AsyncElasticsearch, helpers
from app.core.config import settings
from app.services.scip import SCIPDocument, SCIPSymbol
from app.services.search_query_parser import search_query_to_elasticsearch

logger = logging.getLogger(__name__)


class ElasticsearchService:
    """Service for Elasticsearch indexing and search operations."""

    def __init__(self):
        """Initialize Elasticsearch client."""
        self.client = AsyncElasticsearch(
            hosts=[str(settings.ELASTICSEARCH_URL)],
            verify_certs=False,  # Disable for local development
        )
        self.index_name = "code_symbols"

    async def close(self):
        """Close Elasticsearch client."""
        await self.client.close()

    async def create_index(self) -> bool:
        """Create the code symbols index with proper mappings."""
        if await self.client.indices.exists(index=self.index_name):
            logger.info(f"Index {self.index_name} already exists")
            return False

        mappings = {
            "properties": {
                # Repository metadata
                "repository_id": {"type": "keyword"},
                "repository_name": {"type": "keyword"},
                "file_path": {
                    "type": "text",
                    "fields": {"keyword": {"type": "keyword"}},
                },
                "language": {"type": "keyword"},
                # Symbol metadata
                "symbol_id": {"type": "keyword"},
                "kind": {"type": "keyword"},  # function, class, method, variable, import
                "name": {
                    "type": "text",
                    "fields": {"keyword": {"type": "keyword"}},
                    "analyzer": "standard",
                },
                # Location
                "line_start": {"type": "integer"},
                "line_end": {"type": "integer"},
                "col_start": {"type": "integer"},
                "col_end": {"type": "integer"},
                # Content
                "signature": {
                    "type": "text",
                    "fields": {"keyword": {"type": "keyword"}},
                },
                "documentation": {"type": "text"},
                "content": {"type": "text"},  # Full symbol content
                # Relationships
                "parent_symbol_id": {"type": "keyword"},
                "references": {
                    "type": "nested",
                    "properties": {
                        "type": {"type": "keyword"},  # implements, extends, calls, imports
                        "target_symbol_id": {"type": "keyword"},
                    },
                },
                # Timestamps
                "indexed_at": {"type": "date"},
            }
        }

        settings_dict = {
            "number_of_shards": 1,
            "number_of_replicas": 0,  # No replicas for local dev
            "analysis": {
                "analyzer": {
                    "code_analyzer": {
                        "type": "custom",
                        "tokenizer": "standard",
                        "filter": ["lowercase", "asciifolding"],
                    }
                }
            },
        }

        await self.client.indices.create(
            index=self.index_name, mappings=mappings, settings=settings_dict
        )
        logger.info(f"Created index {self.index_name}")
        return True

    async def delete_index(self) -> bool:
        """Delete the code symbols index."""
        if not await self.client.indices.exists(index=self.index_name):
            logger.warning(f"Index {self.index_name} does not exist")
            return False

        await self.client.indices.delete(index=self.index_name)
        logger.info(f"Deleted index {self.index_name}")
        return True

    async def index_symbol(
        self,
        repository_id: str,
        repository_name: str,
        symbol: SCIPSymbol,
    ) -> bool:
        """Index a single symbol."""
        document = {
            "repository_id": repository_id,
            "repository_name": repository_name,
            "file_path": symbol.file_path,
            "language": symbol.language or "unknown",
            "symbol_id": symbol.symbol_id,
            "kind": symbol.kind,
            "name": symbol.name,
            "line_start": symbol.line_start,
            "line_end": symbol.line_end,
            "col_start": symbol.col_start,
            "col_end": symbol.col_end,
            "signature": symbol.signature,
            "documentation": symbol.documentation,
            "content": symbol.content,
            "parent_symbol_id": symbol.parent_symbol_id,
            "references": symbol.references,
            "indexed_at": datetime.utcnow(),
        }

        try:
            await self.client.index(
                index=self.index_name, id=symbol.symbol_id, document=document
            )
            return True
        except Exception as e:
            logger.error(f"Error indexing symbol {symbol.symbol_id}: {e}")
            return False

    async def bulk_index_symbols(
        self,
        repository_id: str,
        repository_name: str,
        symbols: List[SCIPSymbol],
    ) -> Dict[str, int]:
        """Bulk index multiple symbols."""
        actions = []
        for symbol in symbols:
            document = {
                "_index": self.index_name,
                "_id": symbol.symbol_id,
                "_source": {
                    "repository_id": repository_id,
                    "repository_name": repository_name,
                    "file_path": symbol.file_path,
                    "language": symbol.language or "unknown",
                    "symbol_id": symbol.symbol_id,
                    "kind": symbol.kind,
                    "name": symbol.name,
                    "line_start": symbol.line_start,
                    "line_end": symbol.line_end,
                    "col_start": symbol.col_start,
                    "col_end": symbol.col_end,
                    "signature": symbol.signature,
                    "documentation": symbol.documentation,
                    "content": symbol.content,
                    "parent_symbol_id": symbol.parent_symbol_id,
                    "references": symbol.references,
                    "indexed_at": datetime.utcnow(),
                },
            }
            actions.append(document)

        if not actions:
            return {"success": 0, "failed": 0}

        try:
            success, failed = await helpers.async_bulk(
                self.client, actions, raise_on_error=False, chunk_size=500
            )
            logger.info(
                f"Bulk indexed {success} symbols, {len(failed)} failed for repository {repository_name}"
            )
            return {"success": success, "failed": len(failed)}
        except Exception as e:
            logger.error(f"Error during bulk indexing: {e}")
            return {"success": 0, "failed": len(actions)}

    async def index_scip_document(
        self,
        repository_id: str,
        repository_name: str,
        scip_doc: SCIPDocument,
    ) -> Dict[str, int]:
        """Index all symbols from a SCIP document."""
        return await self.bulk_index_symbols(
            repository_id=repository_id,
            repository_name=repository_name,
            symbols=scip_doc.symbols,
        )

    async def delete_repository_symbols(self, repository_id: str) -> int:
        """Delete all symbols for a repository."""
        query = {"query": {"term": {"repository_id": repository_id}}}

        try:
            result = await self.client.delete_by_query(index=self.index_name, body=query)
            deleted = result.get("deleted", 0)
            logger.info(f"Deleted {deleted} symbols for repository {repository_id}")
            return deleted
        except Exception as e:
            logger.error(f"Error deleting symbols for repository {repository_id}: {e}")
            return 0

    async def delete_file_symbols(
        self, repository_id: str, file_path: str
    ) -> int:
        """Delete all symbols for a specific file in a repository."""
        query = {
            "query": {
                "bool": {
                    "must": [
                        {"term": {"repository_id": repository_id}},
                        {"term": {"file_path.keyword": file_path}},
                    ]
                }
            }
        }

        try:
            result = await self.client.delete_by_query(index=self.index_name, body=query)
            deleted = result.get("deleted", 0)
            logger.info(
                f"Deleted {deleted} symbols for file {file_path} in repository {repository_id}"
            )
            return deleted
        except Exception as e:
            logger.error(
                f"Error deleting symbols for file {file_path} in repository {repository_id}: {e}"
            )
            return 0

    async def search_symbols(
        self,
        query: str,
        repository_id: Optional[str] = None,
        kind: Optional[str] = None,
        language: Optional[str] = None,
        file_path: Optional[str] = None,
        limit: int = 100,
        offset: int = 0,
        use_advanced_parser: bool = True,
    ) -> Dict[str, Any]:
        """Search for symbols with filters and advanced query syntax.

        Args:
            query: Search query (supports Boolean operators if use_advanced_parser=True)
            repository_id: Filter by repository
            kind: Filter by symbol kind
            language: Filter by programming language
            file_path: Filter by file path pattern
            limit: Max results to return
            offset: Pagination offset
            use_advanced_parser: Enable Boolean operators (AND, OR, NOT)

        Returns:
            Dict with symbols list and metadata
        """
        # Build the query using advanced parser if enabled
        if use_advanced_parser and query:
            # Parse query with Boolean operators
            query_clause = search_query_to_elasticsearch(query)
        elif query:
            # Simple multi-match query
            query_clause = {
                "multi_match": {
                    "query": query,
                    "fields": ["name^3", "signature^2", "documentation"],
                    "type": "best_fields",
                    "fuzziness": "AUTO",
                }
            }
        else:
            # No query, match all
            query_clause = {"match_all": {}}

        # Build filters
        filter_clauses = []
        if repository_id:
            filter_clauses.append({"term": {"repository_id": repository_id}})
        if kind:
            filter_clauses.append({"term": {"kind": kind}})
        if language:
            filter_clauses.append({"term": {"language": language}})
        if file_path:
            filter_clauses.append({"wildcard": {"file_path.keyword": f"*{file_path}*"}})

        # Wrap query_clause in bool if it's not already
        if "bool" in query_clause:
            # Advanced query already has bool structure
            # Add filters to it
            if filter_clauses:
                if "filter" in query_clause["bool"]:
                    query_clause["bool"]["filter"].extend(filter_clauses)
                else:
                    query_clause["bool"]["filter"] = filter_clauses
            final_query = query_clause
        else:
            # Simple query, wrap in bool with filters
            final_query = {
                "bool": {
                    "must": [query_clause],
                    "filter": filter_clauses,
                }
            }

        search_body = {
            "query": final_query,
            "from": offset,
            "size": limit,
            "sort": [{"_score": {"order": "desc"}}, {"name.keyword": {"order": "asc"}}],
        }

        try:
            result = await self.client.search(index=self.index_name, body=search_body)
            hits = result["hits"]["hits"]
            total = result["hits"]["total"]["value"]

            symbols = [
                {
                    "symbol_id": hit["_source"]["symbol_id"],
                    "kind": hit["_source"]["kind"],
                    "name": hit["_source"]["name"],
                    "file_path": hit["_source"]["file_path"],
                    "line_start": hit["_source"]["line_start"],
                    "line_end": hit["_source"]["line_end"],
                    "signature": hit["_source"].get("signature"),
                    "documentation": hit["_source"].get("documentation"),
                    "repository_name": hit["_source"]["repository_name"],
                    "language": hit["_source"]["language"],
                    "score": hit["_score"],
                }
                for hit in hits
            ]

            return {
                "symbols": symbols,
                "total": total,
                "limit": limit,
                "offset": offset,
            }
        except Exception as e:
            logger.error(f"Error searching symbols: {e}")
            return {"symbols": [], "total": 0, "limit": limit, "offset": offset}

    async def get_symbol_by_id(self, symbol_id: str) -> Optional[Dict[str, Any]]:
        """Get a symbol by its ID."""
        try:
            result = await self.client.get(index=self.index_name, id=symbol_id)
            return result["_source"]
        except Exception as e:
            logger.warning(f"Symbol {symbol_id} not found: {e}")
            return None

    async def get_repository_statistics(self, repository_id: str) -> Dict[str, Any]:
        """Get statistics for a repository."""
        query = {
            "query": {"term": {"repository_id": repository_id}},
            "aggs": {
                "by_kind": {"terms": {"field": "kind"}},
                "by_language": {"terms": {"field": "language"}},
                "by_file": {"terms": {"field": "file_path.keyword", "size": 1000}},
            },
            "size": 0,
        }

        try:
            result = await self.client.search(index=self.index_name, body=query)
            total = result["hits"]["total"]["value"]

            kinds = {
                bucket["key"]: bucket["doc_count"]
                for bucket in result["aggregations"]["by_kind"]["buckets"]
            }
            languages = {
                bucket["key"]: bucket["doc_count"]
                for bucket in result["aggregations"]["by_language"]["buckets"]
            }
            files = {
                bucket["key"]: bucket["doc_count"]
                for bucket in result["aggregations"]["by_file"]["buckets"]
            }

            return {
                "total_symbols": total,
                "by_kind": kinds,
                "by_language": languages,
                "file_count": len(files),
                "files": files,
            }
        except Exception as e:
            logger.error(f"Error getting repository statistics: {e}")
            return {
                "total_symbols": 0,
                "by_kind": {},
                "by_language": {},
                "file_count": 0,
                "files": {},
            }

    async def health_check(self) -> bool:
        """Check if Elasticsearch is healthy."""
        try:
            health = await self.client.cluster.health()
            return health["status"] in ["green", "yellow"]
        except Exception as e:
            logger.error(f"Elasticsearch health check failed: {e}")
            return False
