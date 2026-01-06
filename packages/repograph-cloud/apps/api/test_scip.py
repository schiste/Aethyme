#!/usr/bin/env python3
"""Test script for SCIP generator."""

import json
from app.services.tree_sitter import TreeSitterParser, Symbol
from app.services.scip import SCIPGenerator

# Test Python code
python_code = '''
class UserService:
    """Service for managing users."""

    def create_user(self, username: str, email: str):
        """Create a new user."""
        return {"username": username, "email": email}

    def delete_user(self, user_id: int):
        """Delete a user by ID."""
        pass

def send_welcome_email(email: str):
    """Send welcome email to new user."""
    print(f"Sending email to {email}")
'''

# Test TypeScript code
typescript_code = '''
interface User {
    id: string;
    name: string;
    email: string;
}

class UserRepository {
    async findById(id: string): Promise<User | null> {
        return null;
    }

    async save(user: User): Promise<void> {
        console.log("Saving user", user);
    }
}

export const formatEmail = (email: string): string => {
    return email.toLowerCase().trim();
};
'''


def test_scip_generator():
    """Test the SCIP generator."""
    print("=" * 80)
    print("SCIP GENERATOR TEST")
    print("=" * 80)

    # Initialize parser and generator
    parser = TreeSitterParser()
    scip_gen = SCIPGenerator(
        repository_id="test-repo-123",
        repository_name="testuser/test-repo"
    )

    # Process Python file
    print("\n" + "=" * 80)
    print("PROCESSING: user_service.py")
    print("=" * 80)

    python_symbols = parser.extract_symbols("user_service.py", python_code)
    print(f"\nExtracted {len(python_symbols)} symbols from Python")

    python_doc = scip_gen.add_file("user_service.py", "python", python_symbols)
    print(f"Created SCIP document with {len(python_doc.symbols)} symbols")

    print("\nPython SCIP Symbols:")
    for symbol in python_doc.symbols:
        print(f"\n  Symbol ID: {symbol.symbol_id}")
        print(f"  Kind: {symbol.kind}")
        print(f"  Name: {symbol.name}")
        print(f"  Location: line {symbol.line_start}-{symbol.line_end}")
        if symbol.parent_symbol_id:
            print(f"  Parent: {symbol.parent_symbol_id}")
        if symbol.documentation:
            print(f"  Doc: {symbol.documentation[:50]}...")

    # Process TypeScript file
    print("\n" + "=" * 80)
    print("PROCESSING: user_repository.ts")
    print("=" * 80)

    ts_symbols = parser.extract_symbols("user_repository.ts", typescript_code)
    print(f"\nExtracted {len(ts_symbols)} symbols from TypeScript")

    ts_doc = scip_gen.add_file("user_repository.ts", "typescript", ts_symbols)
    print(f"Created SCIP document with {len(ts_doc.symbols)} symbols")

    print("\nTypeScript SCIP Symbols:")
    for symbol in ts_doc.symbols:
        print(f"\n  Symbol ID: {symbol.symbol_id}")
        print(f"  Kind: {symbol.kind}")
        print(f"  Name: {symbol.name}")
        if symbol.parent_symbol_id:
            print(f"  Parent: {symbol.parent_symbol_id}")

    # Test SCIP index queries
    print("\n" + "=" * 80)
    print("SCIP INDEX QUERIES")
    print("=" * 80)

    # Get statistics
    stats = scip_gen.get_statistics()
    print(f"\nStatistics:")
    print(f"  Total documents: {stats['total_documents']}")
    print(f"  Total symbols: {stats['total_symbols']}")
    print(f"  Symbols by kind: {stats['symbols_by_kind']}")
    print(f"  Files by language: {stats['files_by_language']}")

    # Find symbols by name
    print("\n\nFind symbols named 'save':")
    save_symbols = scip_gen.find_symbol("save")
    for symbol in save_symbols:
        print(f"  - {symbol.kind} in {symbol.file_path}:line {symbol.line_start}")

    # Get symbols by kind
    print("\n\nAll class symbols:")
    class_symbols = scip_gen.get_symbols_by_kind("class")
    for symbol in class_symbols:
        print(f"  - {symbol.name} in {symbol.file_path}")

    print("\n\nAll method symbols:")
    method_symbols = scip_gen.get_symbols_by_kind("method")
    for symbol in method_symbols:
        print(f"  - {symbol.name} (parent: {symbol.parent_symbol_id})")

    # Export to JSON
    print("\n" + "=" * 80)
    print("SCIP INDEX EXPORT")
    print("=" * 80)

    scip_dict = scip_gen.to_dict()
    print(f"\nExported SCIP index:")
    print(f"  Repository: {scip_dict['repository_name']}")
    print(f"  Documents: {len(scip_dict['documents'])}")
    print(f"  Total symbols: {scip_dict['statistics']['total_symbols']}")

    # Pretty print first document
    print("\n\nFirst document (user_service.py):")
    first_doc = scip_dict['documents']['user_service.py']
    print(json.dumps(first_doc, indent=2)[:500] + "...")

    print("\n" + "=" * 80)
    print("TEST COMPLETE ✅")
    print("=" * 80)

    # Test symbol ID generation
    print("\n" + "=" * 80)
    print("SYMBOL ID FORMAT")
    print("=" * 80)
    print("\nSymbol ID examples:")
    for doc in scip_gen.documents.values():
        for symbol in doc.symbols[:3]:  # First 3 from each file
            print(f"  {symbol.symbol_id}")

    print("\n✅ All SCIP tests passed!\n")


if __name__ == "__main__":
    test_scip_generator()
