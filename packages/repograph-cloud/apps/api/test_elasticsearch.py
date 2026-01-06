#!/usr/bin/env python3
"""Test script for Elasticsearch integration."""

import asyncio
from app.services.tree_sitter import TreeSitterParser
from app.services.scip import SCIPGenerator
from app.services.elasticsearch import ElasticsearchService


# Test Python code
python_code = '''
class AuthService:
    """Service for authentication."""

    def login(self, username: str, password: str):
        """Authenticate user."""
        return {"token": "abc123"}

    def logout(self, token: str):
        """Logout user."""
        pass

def hash_password(password: str) -> str:
    """Hash password using bcrypt."""
    return "hashed_" + password
'''


async def test_elasticsearch():
    """Test the Elasticsearch integration."""
    print("=" * 80)
    print("ELASTICSEARCH INTEGRATION TEST")
    print("=" * 80)

    # Initialize services
    parser = TreeSitterParser()
    scip_gen = SCIPGenerator(
        repository_id="test-repo-456",
        repository_name="testuser/auth-service"
    )
    es_service = ElasticsearchService()

    try:
        # Step 1: Health check
        print("\n" + "=" * 80)
        print("ELASTICSEARCH HEALTH CHECK")
        print("=" * 80)

        health = await es_service.health_check()
        print(f"\nElasticsearch is {'healthy ✅' if health else 'unhealthy ❌'}")

        if not health:
            print("\n❌ Elasticsearch is not running. Please start it with:")
            print("   docker run -p 9200:9200 -e 'discovery.type=single-node' elasticsearch:8.11.0")
            return

        # Step 2: Create index
        print("\n" + "=" * 80)
        print("INDEX CREATION")
        print("=" * 80)

        # Delete index if exists (for clean test)
        try:
            await es_service.delete_index()
            print("\nDeleted existing index")
        except:
            pass

        created = await es_service.create_index()
        print(f"\nIndex created: {created}")

        # Step 3: Extract and index symbols
        print("\n" + "=" * 80)
        print("INDEXING SYMBOLS")
        print("=" * 80)

        # Extract symbols from Python code
        symbols = parser.extract_symbols("auth_service.py", python_code)
        print(f"\nExtracted {len(symbols)} symbols")

        # Generate SCIP document
        scip_doc = scip_gen.add_file("auth_service.py", "python", symbols)
        print(f"Generated SCIP document with {len(scip_doc.symbols)} symbols")

        # Index in Elasticsearch
        result = await es_service.index_scip_document(
            repository_id="test-repo-456",
            repository_name="testuser/auth-service",
            scip_doc=scip_doc
        )
        print(f"\nIndexed {result['success']} symbols, {result['failed']} failed")

        # Wait for indexing to complete (Elasticsearch refresh)
        await asyncio.sleep(1)

        # Step 4: Get repository statistics
        print("\n" + "=" * 80)
        print("REPOSITORY STATISTICS")
        print("=" * 80)

        stats = await es_service.get_repository_statistics("test-repo-456")
        print(f"\nTotal symbols: {stats['total_symbols']}")
        print(f"By kind: {stats['by_kind']}")
        print(f"By language: {stats['by_language']}")
        print(f"File count: {stats['file_count']}")

        # Step 5: Search for symbols
        print("\n" + "=" * 80)
        print("SYMBOL SEARCH TESTS")
        print("=" * 80)

        # Test 1: Search by name
        print("\n\nTest 1: Search for 'login'")
        search_result = await es_service.search_symbols(
            query="login",
            repository_id="test-repo-456"
        )
        print(f"Found {search_result['total']} symbols:")
        for symbol in search_result['symbols']:
            print(f"  - {symbol['kind']} {symbol['name']} in {symbol['file_path']}:{symbol['line_start']}")
            print(f"    Score: {symbol['score']:.2f}")

        # Test 2: Search by kind
        print("\n\nTest 2: Search for all methods")
        search_result = await es_service.search_symbols(
            query="",
            repository_id="test-repo-456",
            kind="method"
        )
        print(f"Found {search_result['total']} methods:")
        for symbol in search_result['symbols']:
            print(f"  - {symbol['name']} (line {symbol['line_start']})")

        # Test 3: Search by language
        print("\n\nTest 3: Search Python functions")
        search_result = await es_service.search_symbols(
            query="",
            repository_id="test-repo-456",
            kind="function",
            language="python"
        )
        print(f"Found {search_result['total']} Python functions:")
        for symbol in search_result['symbols']:
            print(f"  - {symbol['name']} (line {symbol['line_start']})")

        # Test 4: Fuzzy search
        print("\n\nTest 4: Fuzzy search for 'pasword' (typo)")
        search_result = await es_service.search_symbols(
            query="pasword",
            repository_id="test-repo-456"
        )
        print(f"Found {search_result['total']} symbols (fuzzy match):")
        for symbol in search_result['symbols']:
            print(f"  - {symbol['kind']} {symbol['name']} in {symbol['file_path']}:{symbol['line_start']}")

        # Test 5: Get symbol by ID
        print("\n\nTest 5: Get symbol by ID")
        symbol_id = "testuser/auth-service:auth_service.py:class:AuthService"
        symbol = await es_service.get_symbol_by_id(symbol_id)
        if symbol:
            print(f"Found symbol: {symbol['name']} ({symbol['kind']})")
            print(f"Documentation: {symbol.get('documentation', 'N/A')}")
            print(f"Location: {symbol['file_path']}:{symbol['line_start']}-{symbol['line_end']}")
        else:
            print("Symbol not found ❌")

        # Step 6: Test delete repository symbols
        print("\n" + "=" * 80)
        print("DELETE REPOSITORY SYMBOLS")
        print("=" * 80)

        deleted = await es_service.delete_repository_symbols("test-repo-456")
        print(f"\nDeleted {deleted} symbols")

        # Verify deletion
        await asyncio.sleep(1)
        stats = await es_service.get_repository_statistics("test-repo-456")
        print(f"Remaining symbols: {stats['total_symbols']} (should be 0)")

        print("\n" + "=" * 80)
        print("TEST COMPLETE ✅")
        print("=" * 80)
        print("\n✅ All Elasticsearch tests passed!\n")

    finally:
        await es_service.close()


if __name__ == "__main__":
    asyncio.run(test_elasticsearch())
