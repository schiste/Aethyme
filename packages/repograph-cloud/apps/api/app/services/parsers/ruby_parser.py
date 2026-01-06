"""Ruby source code parser."""

import re
from typing import List, Optional
from .base_parser import (
    BaseParser,
    ParseResult,
    ParsedSymbol,
    ParsedRelationship,
    SymbolKind,
)


class RubyParser(BaseParser):
    """Parser for Ruby source code."""

    def __init__(self):
        """Initialize Ruby parser."""
        super().__init__("ruby")

    def parse_file(self, file_path: str, content: str) -> ParseResult:
        """Parse a Ruby source file.

        Args:
            file_path: Path to the Ruby file
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
            metadata={"module": self._extract_module(content)},
        )

    def extract_symbols(self, content: str, file_path: str) -> List[ParsedSymbol]:
        """Extract symbols from Ruby code.

        Args:
            content: Ruby source code
            file_path: Path to the file

        Returns:
            List of parsed symbols
        """
        symbols = []

        # Extract modules
        module_pattern = r'module\s+(\w+(?:::\w+)*)'
        for match in re.finditer(module_pattern, content):
            module_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=module_name,
                kind=SymbolKind.MODULE,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start(), 'end'),
                end_column=0,
            )
            symbols.append(symbol)

        # Extract classes
        class_pattern = r'class\s+(\w+)(?:\s+<\s+(\w+(?:::\w+)*))?'
        for match in re.finditer(class_pattern, content):
            class_name = match.group(1)
            parent_class = match.group(2)
            line = content[:match.start()].count('\n') + 1

            # Find parent module
            parent_module = self._find_parent_module(content, match.start())

            symbol = ParsedSymbol(
                name=class_name,
                kind=SymbolKind.CLASS,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start(), 'end'),
                end_column=0,
                parent=parent_module,
                metadata={
                    "extends": parent_class if parent_class else None,
                },
            )
            symbols.append(symbol)

        # Extract methods/functions
        method_pattern = r'def\s+(?:self\.)?(\w+[!?]?)\s*(?:\((.*?)\))?'
        for match in re.finditer(method_pattern, content):
            method_name = match.group(1)
            params_str = match.group(2) if match.group(2) else ""
            line = content[:match.start()].count('\n') + 1

            # Check if it's a class method (def self.method_name)
            is_class_method = 'self.' in content[max(0, match.start()-10):match.start()]

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
                end_line=self._find_end_line(content, match.start(), 'end'),
                end_column=0,
                signature=match.group(0),
                parameters=parameters,
                modifiers=['class_method'] if is_class_method else [],
                parent=parent_class,
            )
            symbols.append(symbol)

        # Extract instance variables (attributes)
        # Match attr_accessor :name, :email, :age or attr_reader :id
        attr_pattern = r'attr_(?:reader|writer|accessor)\s+((?::[\w]+(?:,\s*)?)+)'
        for match in re.finditer(attr_pattern, content):
            attrs_str = match.group(1)
            line = content[:match.start()].count('\n') + 1
            parent_class = self._find_parent_class(content, match.start())

            # Extract all attribute names from the line
            attr_names = re.findall(r':(\w+)', attrs_str)

            for attr_name in attr_names:
                symbol = ParsedSymbol(
                    name=attr_name,
                    kind=SymbolKind.PROPERTY,
                    file_path=file_path,
                    line=line,
                    column=match.start() - content.rfind('\n', 0, match.start()),
                    end_line=line,
                    end_column=0,
                    parent=parent_class,
                )
                symbols.append(symbol)

        # Extract constants
        constant_pattern = r'^([A-Z][A-Z0-9_]*)\s*='
        for match in re.finditer(constant_pattern, content, re.MULTILINE):
            constant_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            # Skip if inside a method
            if self._is_inside_method(content, match.start()):
                continue

            parent = self._find_parent_class(content, match.start()) or \
                     self._find_parent_module(content, match.start())

            symbol = ParsedSymbol(
                name=constant_name,
                kind=SymbolKind.CONSTANT,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=line,
                end_column=0,
                parent=parent,
            )
            symbols.append(symbol)

        return symbols

    def extract_relationships(
        self, content: str, file_path: str, symbols: List[ParsedSymbol]
    ) -> List[ParsedRelationship]:
        """Extract relationships between Ruby symbols.

        Args:
            content: Ruby source code
            file_path: Path to the file
            symbols: Previously extracted symbols

        Returns:
            List of relationships
        """
        relationships = []

        # Extract method calls (simplified)
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
                        target_file="",
                        line=line,
                        column=match.start() - content.rfind('\n', 0, match.start()),
                    )
                )

        # Extract inheritance relationships
        for symbol in symbols:
            if symbol.kind == SymbolKind.CLASS and symbol.metadata.get("extends"):
                relationships.append(
                    ParsedRelationship(
                        source_symbol=symbol.name,
                        target_symbol=symbol.metadata["extends"],
                        relationship_type="inherits",
                        source_file=file_path,
                        target_file="",
                        line=symbol.line,
                        column=symbol.column,
                    )
                )

        return relationships

    def extract_imports(self, content: str) -> List[str]:
        """Extract require/load statements from Ruby code.

        Args:
            content: Ruby source code

        Returns:
            List of required files/gems
        """
        imports = []

        # require 'module_name'
        require_pattern = r'require\s+[\'"]([^\'"]+)[\'"]'
        for match in re.finditer(require_pattern, content):
            imports.append(match.group(1))

        # require_relative 'file'
        require_relative_pattern = r'require_relative\s+[\'"]([^\'"]+)[\'"]'
        for match in re.finditer(require_relative_pattern, content):
            imports.append(match.group(1))

        # load 'file.rb'
        load_pattern = r'load\s+[\'"]([^\'"]+)[\'"]'
        for match in re.finditer(load_pattern, content):
            imports.append(match.group(1))

        return imports

    def _extract_module(self, content: str) -> Optional[str]:
        """Extract top-level module name from Ruby code."""
        module_match = re.search(r'^module\s+(\w+(?:::\w+)*)', content, re.MULTILINE)
        return module_match.group(1) if module_match else None

    def _parse_parameters(self, params_str: str) -> List[dict]:
        """Parse method parameters."""
        if not params_str.strip():
            return []

        parameters = []
        param_parts = [p.strip() for p in params_str.split(',')]

        for part in param_parts:
            if not part:
                continue

            # Handle different parameter types
            if part.startswith('*'):
                # Splat parameter
                param_name = part[1:]
                param_type = 'splat'
            elif part.startswith('**'):
                # Double splat (kwargs)
                param_name = part[2:]
                param_type = 'kwargs'
            elif part.startswith('&'):
                # Block parameter
                param_name = part[1:]
                param_type = 'block'
            elif '=' in part:
                # Default parameter
                param_name = part.split('=')[0].strip()
                param_type = 'default'
            else:
                param_name = part
                param_type = 'required'

            parameters.append({
                "name": param_name,
                "type": param_type,
            })

        return parameters

    def _find_parent_class(self, content: str, position: int) -> Optional[str]:
        """Find the class that contains a given position."""
        before_position = content[:position]
        class_matches = list(re.finditer(r'class\s+(\w+)', before_position))

        if not class_matches:
            return None

        # Get the last class before this position
        last_class = class_matches[-1]
        class_name = last_class.group(1)

        # Make sure we haven't passed the 'end' of this class
        # Simplified: just return the last class found
        return class_name

    def _find_parent_module(self, content: str, position: int) -> Optional[str]:
        """Find the module that contains a given position."""
        before_position = content[:position]
        module_matches = list(re.finditer(r'module\s+(\w+(?:::\w+)*)', before_position))

        if not module_matches:
            return None

        last_module = module_matches[-1]
        return last_module.group(1)

    def _find_end_line(self, content: str, start_pos: int, end_keyword: str = 'end') -> int:
        """Find the end line of a Ruby block."""
        # Count 'def/class/module' and 'end' keywords
        block_count = 1  # We've already seen one block start
        search_start = start_pos

        # Find the opening keyword line
        for i in range(start_pos, len(content)):
            if content[i] == '\n':
                search_start = i + 1
                break

        # Now count nested blocks
        lines = content[search_start:].split('\n')
        current_line = content[:start_pos].count('\n') + 1

        for line_content in lines:
            current_line += 1

            # Count block starts (def, class, module, if, etc.)
            if re.search(r'\b(def|class|module|if|unless|while|until|for|begin|case)\b', line_content):
                # Don't count inline conditionals
                if not re.search(r'\b(def|class|module)\b.*\bend\b', line_content):
                    block_count += 1

            # Count block ends
            if re.search(r'\bend\b', line_content):
                block_count -= 1

            if block_count == 0:
                return current_line

        # If we didn't find the end, estimate
        return current_line + 10

    def _is_inside_method(self, content: str, position: int) -> bool:
        """Check if a position is inside a method body."""
        before = content[:position]
        method_matches = list(re.finditer(r'def\s+\w+', before))

        if not method_matches:
            return False

        # Count 'def' and 'end' keywords
        def_count = len(method_matches)
        end_count = len(re.findall(r'\bend\b', before))

        # If more defs than ends, we're inside a method
        return def_count > end_count

    def _find_containing_method(self, symbols: List[ParsedSymbol], line: int) -> Optional[ParsedSymbol]:
        """Find the method that contains a given line."""
        for symbol in symbols:
            if symbol.kind == SymbolKind.METHOD:
                if symbol.line <= line <= symbol.end_line:
                    return symbol
        return None
