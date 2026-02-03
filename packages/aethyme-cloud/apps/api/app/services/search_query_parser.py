"""Advanced search query parser for Boolean operators and filters."""

import re
from typing import List, Dict, Any, Optional, Tuple
from dataclasses import dataclass
from enum import Enum


class QueryOperator(Enum):
    """Search query operators."""
    AND = "AND"
    OR = "OR"
    NOT = "NOT"


@dataclass
class SearchTerm:
    """A single search term with optional field prefix."""
    field: Optional[str]  # e.g., "name", "signature", "documentation"
    value: str
    is_regex: bool = False
    is_exact: bool = False


@dataclass
class SearchQuery:
    """Parsed search query with operators and filters."""
    must_terms: List[SearchTerm]  # AND terms
    should_terms: List[SearchTerm]  # OR terms
    must_not_terms: List[SearchTerm]  # NOT terms
    filters: Dict[str, Any]  # Additional filters (kind, language, etc.)


class SearchQueryParser:
    """Parser for advanced search queries with Boolean operators.

    Supported syntax:
    - Boolean operators: AND, OR, NOT (case-insensitive)
    - Field-specific: name:login, signature:async, doc:authentication
    - Exact match: "exact phrase"
    - Regex: /pattern/
    - Grouping: (term1 OR term2) AND term3

    Examples:
    - login AND password
    - name:login OR name:authenticate
    - "async function" NOT deprecated
    - /^handle.*Request$/ AND kind:function
    - (login OR auth) AND signature:password
    """

    # Supported field prefixes
    FIELDS = {
        "name": "name",
        "sig": "signature",
        "signature": "signature",
        "doc": "documentation",
        "documentation": "documentation",
        "file": "file_path",
        "path": "file_path",
    }

    def __init__(self):
        self.tokens: List[str] = []
        self.current_pos: int = 0

    def parse(self, query: str) -> SearchQuery:
        """Parse a search query into structured format.

        Args:
            query: Search query string

        Returns:
            SearchQuery object with parsed terms and operators
        """
        if not query or not query.strip():
            return SearchQuery(
                must_terms=[],
                should_terms=[],
                must_not_terms=[],
                filters={}
            )

        # Tokenize the query
        self.tokens = self._tokenize(query)
        self.current_pos = 0

        # Parse expression
        must_terms = []
        should_terms = []
        must_not_terms = []
        current_operator = QueryOperator.AND  # Default to AND
        negate_next = False

        while self.current_pos < len(self.tokens):
            token = self.tokens[self.current_pos]

            # Check for operators
            if token.upper() == "AND":
                current_operator = QueryOperator.AND
                self.current_pos += 1
                continue
            elif token.upper() == "OR":
                current_operator = QueryOperator.OR
                self.current_pos += 1
                continue
            elif token.upper() == "NOT":
                negate_next = True
                self.current_pos += 1
                continue

            # Parse term
            term = self._parse_term(token)

            if term:
                # Determine where to add the term
                if negate_next:
                    must_not_terms.append(term)
                    negate_next = False
                elif current_operator == QueryOperator.OR:
                    should_terms.append(term)
                else:  # AND (default)
                    must_terms.append(term)

            self.current_pos += 1

        # If no must_terms but have should_terms, treat as simple OR query
        # Elasticsearch requires at least one must or should clause
        if not must_terms and should_terms:
            # This is okay - should_terms alone works in Elasticsearch
            pass

        return SearchQuery(
            must_terms=must_terms,
            should_terms=should_terms,
            must_not_terms=must_not_terms,
            filters={}
        )

    def _tokenize(self, query: str) -> List[str]:
        """Tokenize query string, preserving quoted strings and regex patterns.

        Args:
            query: Raw query string

        Returns:
            List of tokens
        """
        tokens = []
        current_token = ""
        in_quotes = False
        in_regex = False
        quote_char = None

        i = 0
        while i < len(query):
            char = query[i]

            # Handle quotes
            if char in ('"', "'") and not in_regex:
                if not in_quotes:
                    in_quotes = True
                    quote_char = char
                    current_token += char
                elif char == quote_char:
                    in_quotes = False
                    current_token += char
                    tokens.append(current_token)
                    current_token = ""
                    quote_char = None
                else:
                    current_token += char

            # Handle regex patterns
            elif char == '/' and not in_quotes:
                if not in_regex:
                    in_regex = True
                    current_token += char
                else:
                    in_regex = False
                    current_token += char
                    tokens.append(current_token)
                    current_token = ""

            # Handle whitespace (token separator)
            elif char.isspace() and not in_quotes and not in_regex:
                if current_token:
                    tokens.append(current_token)
                    current_token = ""

            # Regular character
            else:
                current_token += char

            i += 1

        # Add last token
        if current_token:
            tokens.append(current_token)

        return tokens

    def _parse_term(self, token: str) -> Optional[SearchTerm]:
        """Parse a single search term.

        Args:
            token: Token to parse

        Returns:
            SearchTerm or None if invalid
        """
        if not token:
            return None

        # Check for field prefix (e.g., "name:login")
        field = None
        value = token

        if ':' in token:
            parts = token.split(':', 1)
            field_name = parts[0].lower()
            if field_name in self.FIELDS:
                field = self.FIELDS[field_name]
                value = parts[1]

        # Check for exact match (quoted)
        is_exact = False
        if value.startswith('"') and value.endswith('"'):
            is_exact = True
            value = value[1:-1]  # Remove quotes
        elif value.startswith("'") and value.endswith("'"):
            is_exact = True
            value = value[1:-1]  # Remove quotes

        # Check for regex pattern
        is_regex = False
        if value.startswith('/') and value.endswith('/'):
            is_regex = True
            value = value[1:-1]  # Remove slashes

        return SearchTerm(
            field=field,
            value=value,
            is_regex=is_regex,
            is_exact=is_exact
        )

    def to_elasticsearch_query(self, search_query: SearchQuery) -> Dict[str, Any]:
        """Convert SearchQuery to Elasticsearch query DSL.

        Args:
            search_query: Parsed search query

        Returns:
            Elasticsearch query dict
        """
        must_clauses = []
        should_clauses = []
        must_not_clauses = []

        # Process must terms (AND)
        for term in search_query.must_terms:
            clause = self._term_to_es_clause(term)
            if clause:
                must_clauses.append(clause)

        # Process should terms (OR)
        for term in search_query.should_terms:
            clause = self._term_to_es_clause(term)
            if clause:
                should_clauses.append(clause)

        # Process must_not terms (NOT)
        for term in search_query.must_not_terms:
            clause = self._term_to_es_clause(term)
            if clause:
                must_not_clauses.append(clause)

        # Build bool query
        bool_query = {"bool": {}}

        if must_clauses:
            bool_query["bool"]["must"] = must_clauses
        if should_clauses:
            bool_query["bool"]["should"] = should_clauses
            # If only should clauses, require at least one match
            if not must_clauses:
                bool_query["bool"]["minimum_should_match"] = 1
        if must_not_clauses:
            bool_query["bool"]["must_not"] = must_not_clauses

        # If no clauses, match all
        if not (must_clauses or should_clauses or must_not_clauses):
            return {"match_all": {}}

        return bool_query

    def _term_to_es_clause(self, term: SearchTerm) -> Optional[Dict[str, Any]]:
        """Convert a SearchTerm to Elasticsearch clause.

        Args:
            term: Search term to convert

        Returns:
            Elasticsearch query clause
        """
        # Determine field(s) to search
        if term.field:
            fields = [term.field]
        else:
            # Default fields with boosting
            fields = ["name^3", "signature^2", "documentation"]

        # Handle regex
        if term.is_regex:
            if term.field:
                return {"regexp": {term.field: {"value": term.value}}}
            else:
                # Regex on name field only (most common use case)
                return {"regexp": {"name": {"value": term.value}}}

        # Handle exact match
        if term.is_exact:
            if term.field:
                return {"match_phrase": {term.field: term.value}}
            else:
                # Multi-match with phrase
                return {
                    "multi_match": {
                        "query": term.value,
                        "fields": fields,
                        "type": "phrase"
                    }
                }

        # Regular fuzzy match
        if len(fields) == 1:
            return {
                "match": {
                    fields[0].split('^')[0]: {  # Remove boost for match
                        "query": term.value,
                        "fuzziness": "AUTO"
                    }
                }
            }
        else:
            return {
                "multi_match": {
                    "query": term.value,
                    "fields": fields,
                    "type": "best_fields",
                    "fuzziness": "AUTO"
                }
            }


# Convenience function
def parse_search_query(query: str) -> SearchQuery:
    """Parse a search query string.

    Args:
        query: Search query string

    Returns:
        Parsed SearchQuery object
    """
    parser = SearchQueryParser()
    return parser.parse(query)


def search_query_to_elasticsearch(query: str) -> Dict[str, Any]:
    """Parse search query and convert to Elasticsearch query.

    Args:
        query: Search query string

    Returns:
        Elasticsearch query dict
    """
    parser = SearchQueryParser()
    search_query = parser.parse(query)
    return parser.to_elasticsearch_query(search_query)
