"""Elasticsearch integration for code search."""

import os
from typing import List, Optional
from elasticsearch import Elasticsearch, helpers


# Elasticsearch client (singleton)
_es_client: Optional[Elasticsearch] = None


def get_elasticsearch_client() -> Elasticsearch:
    """Get or create Elasticsearch client."""
    global _es_client

    if _es_client is None:
        es_url = os.getenv("ELASTICSEARCH_URL", "http://localhost:9202")
        _es_client = Elasticsearch([es_url])

    return _es_client


class CodeIndexer:
    """Index code files into Elasticsearch."""

    def __init__(self):
        self.client = get_elasticsearch_client()
        self.index_prefix = os.getenv("ELASTICSEARCH_INDEX_PREFIX", "aethyme_code_")

    def get_index_name(self, repository_id: str) -> str:
        """
        Get index name for a repository.

        Args:
            repository_id: Repository ID

        Returns:
            Elasticsearch index name
        """
        return f"{self.index_prefix}{repository_id}"

    def create_index(self, repository_id: str) -> None:
        """
        Create Elasticsearch index for a repository.

        Args:
            repository_id: Repository ID
        """
        index_name = self.get_index_name(repository_id)

        # Delete existing index if exists
        if self.client.indices.exists(index=index_name):
            self.client.indices.delete(index=index_name)

        # Create index with mappings and settings
        index_config = {
            "settings": {
                "number_of_shards": 1,
                "number_of_replicas": 0,
                "analysis": {
                    "analyzer": {
                        "code_analyzer": {
                            "type": "custom",
                            "tokenizer": "standard",
                            "filter": ["lowercase", "code_word_delimiter"]
                        }
                    },
                    "filter": {
                        "code_word_delimiter": {
                            "type": "word_delimiter",
                            "preserve_original": True,
                            "catenate_all": True
                        }
                    }
                }
            },
            "mappings": {
                "properties": {
                    "repository_id": {
                        "type": "keyword"
                    },
                    "file_path": {
                        "type": "keyword"
                    },
                    "file_name": {
                        "type": "text",
                        "fields": {
                            "keyword": {
                                "type": "keyword"
                            }
                        }
                    },
                    "language": {
                        "type": "keyword"
                    },
                    "extension": {
                        "type": "keyword"
                    },
                    "content": {
                        "type": "text",
                        "analyzer": "code_analyzer"
                    },
                    "line_count": {
                        "type": "integer"
                    },
                    "code_lines": {
                        "type": "integer"
                    },
                    "size_bytes": {
                        "type": "integer"
                    },
                    "content_hash": {
                        "type": "keyword"
                    },
                    "indexed_at": {
                        "type": "date"
                    }
                }
            }
        }

        self.client.indices.create(index=index_name, body=index_config)

    def index_file(
        self,
        repository_id: str,
        file_data: dict
    ) -> None:
        """
        Index a single file.

        Args:
            repository_id: Repository ID
            file_data: File data dictionary
        """
        index_name = self.get_index_name(repository_id)

        # Add repository_id to document
        doc = {
            **file_data,
            "repository_id": repository_id
        }

        # Index document
        self.client.index(
            index=index_name,
            id=file_data["content_hash"],
            document=doc
        )

    def bulk_index_files(
        self,
        repository_id: str,
        files_data: List[dict]
    ) -> dict:
        """
        Bulk index multiple files.

        Args:
            repository_id: Repository ID
            files_data: List of file data dictionaries

        Returns:
            Bulk indexing response statistics
        """
        index_name = self.get_index_name(repository_id)

        # Prepare bulk actions
        actions = []
        for file_data in files_data:
            action = {
                "_index": index_name,
                "_id": file_data["content_hash"],
                "_source": {
                    **file_data,
                    "repository_id": repository_id
                }
            }
            actions.append(action)

        # Execute bulk indexing
        success, failed = helpers.bulk(
            self.client,
            actions,
            raise_on_error=False,
            raise_on_exception=False
        )

        return {
            "success": success,
            "failed": failed
        }

    def delete_repository_index(self, repository_id: str) -> None:
        """
        Delete index for a repository.

        Args:
            repository_id: Repository ID
        """
        index_name = self.get_index_name(repository_id)

        if self.client.indices.exists(index=index_name):
            self.client.indices.delete(index=index_name)

    def search_code(
        self,
        repository_id: str,
        query: str,
        language: Optional[str] = None,
        file_path: Optional[str] = None,
        limit: int = 50
    ) -> dict:
        """
        Search code in a repository.

        Args:
            repository_id: Repository ID
            query: Search query
            language: Filter by language
            file_path: Filter by file path
            limit: Maximum results

        Returns:
            Search results
        """
        index_name = self.get_index_name(repository_id)

        # Build query
        must_clauses = [
            {
                "multi_match": {
                    "query": query,
                    "fields": ["content^2", "file_name", "file_path"]
                }
            }
        ]

        # Add filters
        if language:
            must_clauses.append({"term": {"language": language}})

        if file_path:
            must_clauses.append({"wildcard": {"file_path": f"*{file_path}*"}})

        search_body = {
            "query": {
                "bool": {
                    "must": must_clauses
                }
            },
            "highlight": {
                "fields": {
                    "content": {
                        "fragment_size": 150,
                        "number_of_fragments": 3
                    }
                }
            },
            "size": limit
        }

        # Execute search
        response = self.client.search(index=index_name, body=search_body)

        # Format results
        results = []
        for hit in response["hits"]["hits"]:
            result = {
                "file_path": hit["_source"]["file_path"],
                "file_name": hit["_source"]["file_name"],
                "language": hit["_source"]["language"],
                "score": hit["_score"],
                "highlights": hit.get("highlight", {}).get("content", [])
            }
            results.append(result)

        return {
            "total": response["hits"]["total"]["value"],
            "results": results
        }

    def get_all_files(self, repository_id: str) -> List[dict]:
        """
        Get all indexed files from a repository.

        Args:
            repository_id: Repository ID

        Returns:
            List of file documents with file_path, language, size_bytes, line_count
        """
        index_name = self.get_index_name(repository_id)

        # Check if index exists
        if not self.client.indices.exists(index=index_name):
            return []

        # Scroll through all documents
        search_body = {
            "query": {
                "match_all": {}
            },
            "_source": ["file_path", "language", "size_bytes", "line_count"],
            "size": 1000  # Fetch 1000 docs at a time
        }

        results = []
        response = self.client.search(index=index_name, body=search_body, scroll="2m")
        scroll_id = response["_scroll_id"]

        # Process initial batch
        for hit in response["hits"]["hits"]:
            results.append(hit["_source"])

        # Scroll through remaining results
        while len(response["hits"]["hits"]) > 0:
            response = self.client.scroll(scroll_id=scroll_id, scroll="2m")
            scroll_id = response["_scroll_id"]

            for hit in response["hits"]["hits"]:
                results.append(hit["_source"])

        # Clear scroll
        self.client.clear_scroll(scroll_id=scroll_id)

        return results
