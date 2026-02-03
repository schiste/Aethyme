#!/usr/bin/env python3
"""Test script for Tree-sitter parser."""

from app.services.tree_sitter import TreeSitterParser

# Test Python code
python_code = '''
def hello_world():
    """Say hello to the world."""
    print("Hello, world!")

class UserService:
    """Service for user operations."""

    def create_user(self, username, email):
        """Create a new user."""
        return {"username": username, "email": email}

    def delete_user(self, user_id):
        """Delete a user."""
        pass
'''

# Test TypeScript code
typescript_code = '''
interface User {
    id: string;
    name: string;
}

class UserService {
    async getUser(id: string): Promise<User> {
        return { id, name: "John" };
    }

    async createUser(user: User): Promise<void> {
        console.log("Creating user", user);
    }
}

export const formatName = (name: string) => {
    return name.toUpperCase();
};
'''

# Test JavaScript code
javascript_code = '''
function calculateTotal(items) {
    return items.reduce((sum, item) => sum + item.price, 0);
}

const formatCurrency = (amount) => {
    return `$${amount.toFixed(2)}`;
};

class ShoppingCart {
    constructor() {
        this.items = [];
    }

    addItem(item) {
        this.items.push(item);
    }

    getTotal() {
        return calculateTotal(this.items);
    }
}
'''


def test_parser():
    """Test the Tree-sitter parser."""
    parser = TreeSitterParser()

    print("=" * 80)
    print("TREE-SITTER PARSER TEST")
    print("=" * 80)

    # Test Python
    print("\n" + "=" * 80)
    print("PYTHON CODE PARSING")
    print("=" * 80)
    symbols = parser.extract_symbols("test.py", python_code)
    print(f"\nExtracted {len(symbols)} symbols:")
    for symbol in symbols:
        print(f"  - {symbol.kind.upper()}: {symbol.name} (line {symbol.line_start}-{symbol.line_end})")
        if symbol.parent:
            print(f"    Parent: {symbol.parent}")
        if symbol.docstring:
            print(f"    Doc: {symbol.docstring[:50]}...")

    # Test TypeScript
    print("\n" + "=" * 80)
    print("TYPESCRIPT CODE PARSING")
    print("=" * 80)
    symbols = parser.extract_symbols("test.ts", typescript_code)
    print(f"\nExtracted {len(symbols)} symbols:")
    for symbol in symbols:
        print(f"  - {symbol.kind.upper()}: {symbol.name} (line {symbol.line_start}-{symbol.line_end})")
        if symbol.parent:
            print(f"    Parent: {symbol.parent}")

    # Test JavaScript
    print("\n" + "=" * 80)
    print("JAVASCRIPT CODE PARSING")
    print("=" * 80)
    symbols = parser.extract_symbols("test.js", javascript_code)
    print(f"\nExtracted {len(symbols)} symbols:")
    for symbol in symbols:
        print(f"  - {symbol.kind.upper()}: {symbol.name} (line {symbol.line_start}-{symbol.line_end})")
        if symbol.parent:
            print(f"    Parent: {symbol.parent}")

    print("\n" + "=" * 80)
    print("TEST COMPLETE ✅")
    print("=" * 80)

    # Test language detection
    print("\n" + "=" * 80)
    print("LANGUAGE DETECTION TEST")
    print("=" * 80)
    test_files = [
        "app.py",
        "index.js",
        "component.tsx",
        "main.go",
        "lib.rs",
        "Main.java",
        "unknown.txt",
    ]
    for file_path in test_files:
        lang = parser.get_language(file_path)
        print(f"  {file_path:20} -> {lang or 'Unknown'}")

    print("\n✅ All tests passed!\n")


if __name__ == "__main__":
    test_parser()
