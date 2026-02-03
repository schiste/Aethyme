"""Java source code parser."""

import re
from typing import List, Optional
from .base_parser import (
    BaseParser,
    ParseResult,
    ParsedSymbol,
    ParsedRelationship,
    SymbolKind,
)


class JavaParser(BaseParser):
    """Parser for Java source code."""

    def __init__(self):
        """Initialize Java parser."""
        super().__init__("java")

    def parse_file(self, file_path: str, content: str) -> ParseResult:
        """Parse a Java source file.

        Args:
            file_path: Path to the Java file
            content: File content

        Returns:
            ParseResult with symbols and relationships
        """
        symbols = self.extract_symbols(content, file_path)
        relationships = self.extract_relationships(content, file_path, symbols)
        imports = self.extract_imports(content)

        return ParseResult(
            file_path=file_path,
            language=self.language,
            symbols=symbols,
            relationships=relationships,
            imports=imports,
            errors=[],
            metadata={"package": self._extract_package(content)},
        )

    def extract_symbols(self, content: str, file_path: str) -> List[ParsedSymbol]:
        """Extract symbols from Java code.

        Args:
            content: Java source code
            file_path: Path to the file

        Returns:
            List of parsed symbols
        """
        symbols = []

        # Extract package name
        package_name = self._extract_package(content)

        # Extract classes
        class_pattern = r'(?:public\s+|private\s+|protected\s+)?(?:abstract\s+|final\s+)?class\s+(\w+)(?:\s+extends\s+(\w+))?(?:\s+implements\s+([\w,\s]+))?'
        for match in re.finditer(class_pattern, content):
            class_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=class_name,
                kind=SymbolKind.CLASS,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                modifiers=self._extract_modifiers(match.group(0)),
                parent=package_name,
                metadata={
                    "extends": match.group(2) if match.group(2) else None,
                    "implements": [i.strip() for i in match.group(3).split(',')] if match.group(3) else [],
                }
            )
            symbols.append(symbol)

        # Extract interfaces
        interface_pattern = r'(?:public\s+)?interface\s+(\w+)(?:\s+extends\s+([\w,\s]+))?'
        for match in re.finditer(interface_pattern, content):
            interface_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=interface_name,
                kind=SymbolKind.INTERFACE,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                modifiers=self._extract_modifiers(match.group(0)),
                parent=package_name,
            )
            symbols.append(symbol)

        # Extract methods
        method_pattern = r'(?:public\s+|private\s+|protected\s+)?(?:static\s+)?(?:final\s+)?(?:synchronized\s+)?(\w+(?:<[\w,\s<>]+>)?)\s+(\w+)\s*\((.*?)\)'
        for match in re.finditer(method_pattern, content):
            return_type = match.group(1)
            method_name = match.group(2)
            params_str = match.group(3)

            # Skip if this looks like a class declaration
            if method_name in ('class', 'interface', 'enum'):
                continue

            line = content[:match.start()].count('\n') + 1

            # Find parent class
            parent_class = self._find_parent_class(content, match.start())

            # Parse parameters
            parameters = self._parse_parameters(params_str)

            symbol = ParsedSymbol(
                name=method_name,
                kind=SymbolKind.METHOD,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                signature=match.group(0),
                return_type=return_type,
                parameters=parameters,
                modifiers=self._extract_modifiers(match.group(0)),
                parent=parent_class,
            )
            symbols.append(symbol)

        # Extract fields
        field_pattern = r'(?:public\s+|private\s+|protected\s+)?(?:static\s+)?(?:final\s+)?(\w+(?:<[\w,\s<>]+>)?)\s+(\w+)\s*(?:=|;)'
        for match in re.finditer(field_pattern, content):
            field_type = match.group(1)
            field_name = match.group(2)

            # Skip if this is a parameter or local variable (inside a method)
            if self._is_inside_method(content, match.start()):
                continue

            line = content[:match.start()].count('\n') + 1
            parent_class = self._find_parent_class(content, match.start())

            symbol = ParsedSymbol(
                name=field_name,
                kind=SymbolKind.FIELD,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=line,
                end_column=0,
                return_type=field_type,
                modifiers=self._extract_modifiers(match.group(0)),
                parent=parent_class,
            )
            symbols.append(symbol)

        return symbols

    def extract_relationships(
        self, content: str, file_path: str, symbols: List[ParsedSymbol]
    ) -> List[ParsedRelationship]:
        """Extract relationships between Java symbols.

        Args:
            content: Java source code
            file_path: Path to the file
            symbols: Previously extracted symbols

        Returns:
            List of relationships
        """
        relationships = []

        # Extract method calls
        call_pattern = r'(\w+)\s*\('
        for match in re.finditer(call_pattern, content):
            called_method = match.group(1)
            line = content[:match.start()].count('\n') + 1

            # Find the calling method
            calling_method = self._find_containing_method(symbols, line)
            if calling_method:
                relationships.append(
                    ParsedRelationship(
                        source_symbol=calling_method.name,
                        target_symbol=called_method,
                        relationship_type="calls",
                        source_file=file_path,
                        target_file="",  # Unknown without full project context
                        line=line,
                        column=match.start() - content.rfind('\n', 0, match.start()),
                    )
                )

        return relationships

    def extract_imports(self, content: str) -> List[str]:
        """Extract import statements from Java code.

        Args:
            content: Java source code

        Returns:
            List of imported classes/packages
        """
        imports = []
        import_pattern = r'import\s+(?:static\s+)?([\w.]+(?:\.\*)?)\s*;'

        for match in re.finditer(import_pattern, content):
            imports.append(match.group(1))

        return imports

    def _extract_package(self, content: str) -> Optional[str]:
        """Extract package name from Java code."""
        package_match = re.search(r'package\s+([\w.]+)\s*;', content)
        return package_match.group(1) if package_match else None

    def _extract_modifiers(self, declaration: str) -> List[str]:
        """Extract modifiers from a declaration."""
        modifiers = []
        modifier_keywords = ['public', 'private', 'protected', 'static', 'final', 'abstract', 'synchronized']

        for keyword in modifier_keywords:
            if re.search(r'\b' + keyword + r'\b', declaration):
                modifiers.append(keyword)

        return modifiers

    def _parse_parameters(self, params_str: str) -> List[dict]:
        """Parse method parameters."""
        if not params_str.strip():
            return []

        parameters = []
        # Split by comma (but not inside generics)
        param_parts = self._smart_split(params_str, ',')

        for part in param_parts:
            part = part.strip()
            if not part:
                continue

            # Parse "Type name" or "Type<Generic> name"
            match = re.match(r'([\w<>,\s]+)\s+(\w+)', part)
            if match:
                parameters.append({
                    "name": match.group(2),
                    "type": match.group(1).strip(),
                })

        return parameters

    def _smart_split(self, text: str, delimiter: str) -> List[str]:
        """Split text by delimiter, ignoring delimiters inside <>."""
        parts = []
        current = []
        depth = 0

        for char in text:
            if char == '<':
                depth += 1
                current.append(char)
            elif char == '>':
                depth -= 1
                current.append(char)
            elif char == delimiter and depth == 0:
                parts.append(''.join(current))
                current = []
            else:
                current.append(char)

        if current:
            parts.append(''.join(current))

        return parts

    def _find_parent_class(self, content: str, position: int) -> Optional[str]:
        """Find the class that contains a given position."""
        # Look backwards for class declaration
        before_position = content[:position]
        class_matches = list(re.finditer(r'class\s+(\w+)', before_position))

        if not class_matches:
            return None

        # Get the last class before this position
        last_class = class_matches[-1]
        class_name = last_class.group(1)

        # Make sure we're still inside this class (not past its closing brace)
        # Simplified: just return the last class found
        return class_name

    def _find_end_line(self, content: str, start_pos: int) -> int:
        """Find the end line of a symbol (simplified)."""
        # Look for closing brace
        brace_count = 0
        found_opening = False

        for i in range(start_pos, len(content)):
            if content[i] == '{':
                found_opening = True
                brace_count += 1
            elif content[i] == '}':
                brace_count -= 1
                if found_opening and brace_count == 0:
                    return content[:i].count('\n') + 1

        # If no closing brace found, just add a few lines
        return content[:start_pos].count('\n') + 10

    def _is_inside_method(self, content: str, position: int) -> bool:
        """Check if a position is inside a method body."""
        # Simplified: check if there's a method declaration before this position
        before = content[:position]
        method_pattern = r'(?:public\s+|private\s+|protected\s+)?\w+\s+\w+\s*\([^)]*\)\s*\{'

        method_matches = list(re.finditer(method_pattern, before))
        if not method_matches:
            return False

        # Check if we're before the closing brace
        last_method_start = method_matches[-1].end()
        braces_after_method = content[last_method_start:position]

        open_count = braces_after_method.count('{')
        close_count = braces_after_method.count('}')

        return open_count > close_count

    def _find_containing_method(self, symbols: List[ParsedSymbol], line: int) -> Optional[ParsedSymbol]:
        """Find the method that contains a given line."""
        for symbol in symbols:
            if symbol.kind == SymbolKind.METHOD:
                if symbol.line <= line <= symbol.end_line:
                    return symbol
        return None
