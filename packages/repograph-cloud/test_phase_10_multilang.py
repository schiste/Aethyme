"""Test Phase 10: Multi-Language Support"""

# Test Language Detection
from apps.api.app.services.language_detector import LanguageDetector, Language

def test_language_detection():
    """Test language detection from file paths and content."""
    print("\n=== Testing Language Detection ===\n")

    # Test extension-based detection
    test_cases = [
        ("Example.java", None, Language.JAVA),
        ("main.rs", None, Language.RUST),
        ("app.rb", None, Language.RUBY),
        ("script.py", None, Language.PYTHON),
        ("app.ts", None, Language.TYPESCRIPT),
        ("server.go", None, Language.GO),
        ("Program.cs", None, Language.CSHARP),
    ]

    print("Extension-based detection:")
    for file_path, content, expected in test_cases:
        detected = LanguageDetector.detect(file_path, content)
        status = "✅" if detected == expected else "❌"
        print(f"  {status} {file_path}: {detected.value} (expected: {expected.value})")

    # Test content-based detection
    print("\nContent-based detection:")

    java_code = """
package com.example;

public class HelloWorld {
    public static void main(String[] args) {
        System.out.println("Hello, World!");
    }
}
"""
    detected = LanguageDetector.detect_from_content(java_code)
    print(f"  {'✅' if detected == Language.JAVA else '❌'} Java code detected as: {detected.value}")

    rust_code = """
fn main() {
    println!("Hello, world!");
}

struct Point {
    x: i32,
    y: i32,
}
"""
    detected = LanguageDetector.detect_from_content(rust_code)
    print(f"  {'✅' if detected == Language.RUST else '❌'} Rust code detected as: {detected.value}")

    ruby_code = """
class User
  attr_accessor :name, :email

  def initialize(name, email)
    @name = name
    @email = email
  end

  def greet
    puts "Hello, #{@name}!"
  end
end
"""
    detected = LanguageDetector.detect_from_content(ruby_code)
    print(f"  {'✅' if detected == Language.RUBY else '❌'} Ruby code detected as: {detected.value}")

    print("\nLanguage Info:")
    for lang in [Language.JAVA, Language.RUST, Language.RUBY]:
        info = LanguageDetector.get_language_info(lang)
        print(f"  {lang.value}:")
        print(f"    - Name: {info['name']}")
        print(f"    - Type System: {info['type_system']}")
        print(f"    - Paradigms: {', '.join(info['paradigm'])}")


def test_java_parser():
    """Test Java parser."""
    print("\n\n=== Testing Java Parser ===\n")

    from apps.api.app.services.parsers.java_parser import JavaParser

    java_code = """
package com.example.app;

import java.util.List;
import java.util.ArrayList;

public class UserService {
    private List<User> users;

    public UserService() {
        this.users = new ArrayList<>();
    }

    public void addUser(User user) {
        users.add(user);
        notifyObservers();
    }

    private void notifyObservers() {
        // Notify all observers
    }

    public List<User> getUsers() {
        return users;
    }
}
"""

    parser = JavaParser()
    result = parser.parse_file("UserService.java", java_code)

    print(f"File: {result.file_path}")
    print(f"Language: {result.language}")
    print(f"Package: {result.metadata.get('package')}")
    print(f"\nSymbols found: {len(result.symbols)}")

    for symbol in result.symbols:
        print(f"  - {symbol.kind.value}: {symbol.name} (line {symbol.line})")
        if symbol.modifiers:
            print(f"    Modifiers: {', '.join(symbol.modifiers)}")
        if symbol.parameters:
            print(f"    Parameters: {len(symbol.parameters)}")

    print(f"\nRelationships found: {len(result.relationships)}")
    for rel in result.relationships[:5]:  # Show first 5
        print(f"  - {rel.source_symbol} {rel.relationship_type} {rel.target_symbol}")

    print(f"\nImports found: {len(result.imports)}")
    for imp in result.imports:
        print(f"  - {imp}")


def test_ruby_parser():
    """Test Ruby parser."""
    print("\n\n=== Testing Ruby Parser ===\n")

    from apps.api.app.services.parsers.ruby_parser import RubyParser

    ruby_code = """
module App
  class User
    attr_accessor :name, :email
    attr_reader :id

    def initialize(name, email)
      @name = name
      @email = email
      @id = generate_id
    end

    def greet
      puts "Hello, #{@name}!"
      send_welcome_email
    end

    def self.find_by_email(email)
      # Find user by email
    end

    private

    def generate_id
      rand(10000)
    end

    def send_welcome_email
      # Send email
    end
  end
end
"""

    parser = RubyParser()
    result = parser.parse_file("user.rb", ruby_code)

    print(f"File: {result.file_path}")
    print(f"Language: {result.language}")
    print(f"Module: {result.metadata.get('module')}")
    print(f"\nSymbols found: {len(result.symbols)}")

    for symbol in result.symbols:
        print(f"  - {symbol.kind.value}: {symbol.name} (line {symbol.line})")
        if symbol.parent:
            print(f"    Parent: {symbol.parent}")
        if symbol.parameters:
            params = [p['name'] for p in symbol.parameters]
            print(f"    Parameters: {', '.join(params)}")

    print(f"\nRelationships found: {len(result.relationships)}")


def test_rust_parser():
    """Test Rust parser."""
    print("\n\n=== Testing Rust Parser ===\n")

    from apps.api.app.services.parsers.rust_parser import RustParser

    rust_code = """
use std::collections::HashMap;

pub struct User {
    pub id: u64,
    pub name: String,
    email: String,
}

impl User {
    pub fn new(id: u64, name: String, email: String) -> Self {
        User { id, name, email }
    }

    pub fn greet(&self) {
        println!("Hello, {}!", self.name);
        self.log_action("greeted");
    }

    fn log_action(&self, action: &str) {
        println!("User {} {}", self.id, action);
    }
}

pub trait Notifiable {
    fn send_notification(&self, message: &str);
}

impl Notifiable for User {
    fn send_notification(&self, message: &str) {
        println!("Sending to {}: {}", self.email, message);
    }
}

pub fn main() {
    let user = User::new(1, String::from("Alice"), String::from("alice@example.com"));
    user.greet();
}
"""

    parser = RustParser()
    result = parser.parse_file("main.rs", rust_code)

    print(f"File: {result.file_path}")
    print(f"Language: {result.language}")
    print(f"\nSymbols found: {len(result.symbols)}")

    for symbol in result.symbols:
        print(f"  - {symbol.kind.value}: {symbol.name} (line {symbol.line})")
        if symbol.modifiers:
            print(f"    Modifiers: {', '.join(symbol.modifiers)}")
        if symbol.return_type:
            print(f"    Returns: {symbol.return_type}")

    print(f"\nRelationships found: {len(result.relationships)}")
    for rel in result.relationships[:5]:
        print(f"  - {rel.source_symbol} {rel.relationship_type} {rel.target_symbol}")

    print(f"\nImports found: {len(result.imports)}")
    for imp in result.imports:
        print(f"  - {imp}")


def test_parser_registry():
    """Test parser registry."""
    print("\n\n=== Testing Parser Registry ===\n")

    from apps.api.app.services.parsers.parser_registry import ParserRegistry

    supported = ParserRegistry.get_supported_languages()
    print(f"Supported languages: {len(supported)}")
    for lang in supported:
        print(f"  - {lang.value}")

    # Test parsing via registry
    print("\nParsing via registry:")

    java_code = "public class Test { }"
    result = ParserRegistry.parse_file("Test.java", java_code)
    if result:
        print(f"  ✅ Java: Parsed successfully ({len(result.symbols)} symbols)")
    else:
        print(f"  ❌ Java: Parse failed")

    ruby_code = "class User\nend"
    result = ParserRegistry.parse_file("user.rb", ruby_code)
    if result:
        print(f"  ✅ Ruby: Parsed successfully ({len(result.symbols)} symbols)")
    else:
        print(f"  ❌ Ruby: Parse failed")

    rust_code = "fn main() {}"
    result = ParserRegistry.parse_file("main.rs", rust_code)
    if result:
        print(f"  ✅ Rust: Parsed successfully ({len(result.symbols)} symbols)")
    else:
        print(f"  ❌ Rust: Parse failed")


def test_language_metrics():
    """Test language-specific metrics."""
    print("\n\n=== Testing Language Metrics ===\n")

    from apps.api.app.services.language_metrics import LanguageMetrics

    # Test complexity calculation
    java_code = """
    public void processData(List<Item> items) {
        for (Item item : items) {
            if (item.isValid()) {
                if (item.getType() == Type.PREMIUM) {
                    processPremium(item);
                } else {
                    processStandard(item);
                }
            }
        }
    }
    """

    complexity = LanguageMetrics.calculate_cyclomatic_complexity(java_code, Language.JAVA)
    print(f"Java code cyclomatic complexity: {complexity}")

    # Test maintainability index
    mi = LanguageMetrics.estimate_maintainability_index(
        cyclomatic_complexity=8,
        lines_of_code=50,
        language=Language.JAVA
    )
    print(f"Maintainability Index: {mi:.1f}/100")

    # Test language statistics
    print("\nLanguage Statistics:")
    for lang in [Language.PYTHON, Language.JAVA, Language.RUST, Language.GO]:
        stats = LanguageMetrics.get_language_statistics(lang)
        print(f"  {lang.value}:")
        print(f"    - Complexity Weight: {stats['complexity_weight']}")
        print(f"    - Typical Function LOC: {stats['typical_function_loc']}")
        print(f"    - Test Coverage Target: {stats['test_coverage_target']}%")


if __name__ == "__main__":
    print("=" * 60)
    print("PHASE 10: MULTI-LANGUAGE SUPPORT - TEST SUITE")
    print("=" * 60)

    try:
        test_language_detection()
        test_java_parser()
        test_ruby_parser()
        test_rust_parser()
        test_parser_registry()
        test_language_metrics()

        print("\n" + "=" * 60)
        print("✅ ALL TESTS COMPLETED")
        print("=" * 60)

    except Exception as e:
        print(f"\n❌ TEST FAILED: {e}")
        import traceback
        traceback.print_exc()
