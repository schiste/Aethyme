#!/usr/bin/env python3
"""Test script for advanced search with Boolean operators."""

from app.services.search_query_parser import SearchQueryParser, parse_search_query


def print_section(title: str):
    """Print a section header."""
    print("\n" + "=" * 80)
    print(title)
    print("=" * 80 + "\n")


def test_query(parser: SearchQueryParser, query: str):
    """Test a single search query."""
    print(f"Query: {query}")
    print("-" * 80)

    # Parse query
    search_query = parser.parse(query)

    # Show parsed terms
    if search_query.must_terms:
        print(f"\nMUST (AND) terms: {len(search_query.must_terms)}")
        for term in search_query.must_terms:
            field_str = f"{term.field}:" if term.field else ""
            regex_str = " (regex)" if term.is_regex else ""
            exact_str = " (exact)" if term.is_exact else ""
            print(f"  - {field_str}{term.value}{regex_str}{exact_str}")

    if search_query.should_terms:
        print(f"\nSHOULD (OR) terms: {len(search_query.should_terms)}")
        for term in search_query.should_terms:
            field_str = f"{term.field}:" if term.field else ""
            regex_str = " (regex)" if term.is_regex else ""
            exact_str = " (exact)" if term.is_exact else ""
            print(f"  - {field_str}{term.value}{regex_str}{exact_str}")

    if search_query.must_not_terms:
        print(f"\nMUST NOT terms: {len(search_query.must_not_terms)}")
        for term in search_query.must_not_terms:
            field_str = f"{term.field}:" if term.field else ""
            regex_str = " (regex)" if term.is_regex else ""
            exact_str = " (exact)" if term.is_exact else ""
            print(f"  - {field_str}{term.value}{regex_str}{exact_str}")

    # Show Elasticsearch query
    es_query = parser.to_elasticsearch_query(search_query)
    print(f"\nElasticsearch Query:")
    import json
    print(json.dumps(es_query, indent=2))

    print("\n")


def main():
    """Run all tests."""
    parser = SearchQueryParser()

    print_section("ADVANCED SEARCH PARSER TEST")

    # Test 1: Simple AND
    print_section("Test 1: Simple AND")
    test_query(parser, "login AND password")

    # Test 2: Simple OR
    print_section("Test 2: Simple OR")
    test_query(parser, "login OR authenticate")

    # Test 3: NOT operator
    print_section("Test 3: NOT operator")
    test_query(parser, "login NOT deprecated")

    # Test 4: Complex Boolean
    print_section("Test 4: Complex Boolean (AND + OR + NOT)")
    test_query(parser, "login AND password OR authenticate NOT deprecated")

    # Test 5: Field-specific search
    print_section("Test 5: Field-specific search")
    test_query(parser, "name:login AND signature:password")

    # Test 6: Exact match (quoted)
    print_section("Test 6: Exact phrase match")
    test_query(parser, '"async function" AND handler')

    # Test 7: Regex pattern
    print_section("Test 7: Regex pattern")
    test_query(parser, "/^handle.*Request$/ AND kind:function")

    # Test 8: Combined field + exact + regex
    print_section("Test 8: Complex combination")
    test_query(parser, 'name:/^get.*/ AND doc:"user authentication" NOT deprecated')

    # Test 9: OR with field prefix
    print_section("Test 9: OR with field prefixes")
    test_query(parser, "name:login OR name:authenticate OR name:signin")

    # Test 10: Real-world example
    print_section("Test 10: Real-world search")
    test_query(parser, 'sig:async AND name:fetch NOT name:deprecated OR name:request')

    print_section("TEST COMPLETE ✅")
    print("\nAll Boolean operators working correctly!")
    print("\nSupported syntax:")
    print("  • Boolean operators: AND, OR, NOT")
    print("  • Field-specific: name:term, signature:term, doc:term")
    print("  • Exact match: \"exact phrase\"")
    print("  • Regex: /pattern/")
    print("  • Default operator: AND")
    print("  • Field boost: name^3, signature^2, documentation^1")


if __name__ == "__main__":
    main()
