"""Rust source code parser."""

import re
from typing import List, Optional
from .base_parser import (
    BaseParser,
    ParseResult,
    ParsedSymbol,
    ParsedRelationship,
    SymbolKind,
)


class RustParser(BaseParser):
    """Parser for Rust source code."""

    def __init__(self):
        """Initialize Rust parser."""
        super().__init__("rust")

    def parse_file(self, file_path: str, content: str) -> ParseResult:
        """Parse a Rust source file.

        Args:
            file_path: Path to the Rust file
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
            metadata={},
        )

    def extract_symbols(self, content: str, file_path: str) -> List[ParsedSymbol]:
        """Extract symbols from Rust code.

        Args:
            content: Rust source code
            file_path: Path to the file

        Returns:
            List of parsed symbols
        """
        symbols = []

        # Extract structs
        struct_pattern = r'(?:pub\s+)?struct\s+(\w+)(?:<[^>]+>)?\s*(?:\{|;|\()'
        for match in re.finditer(struct_pattern, content):
            struct_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=struct_name,
                kind=SymbolKind.STRUCT,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                modifiers=self._extract_modifiers(match.group(0)),
            )
            symbols.append(symbol)

        # Extract enums
        enum_pattern = r'(?:pub\s+)?enum\s+(\w+)(?:<[^>]+>)?\s*\{'
        for match in re.finditer(enum_pattern, content):
            enum_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=enum_name,
                kind=SymbolKind.ENUM,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                modifiers=self._extract_modifiers(match.group(0)),
            )
            symbols.append(symbol)

        # Extract traits
        trait_pattern = r'(?:pub\s+)?trait\s+(\w+)(?:<[^>]+>)?\s*(?:\{|:)'
        for match in re.finditer(trait_pattern, content):
            trait_name = match.group(1)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=trait_name,
                kind=SymbolKind.INTERFACE,  # Traits are like interfaces
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                modifiers=self._extract_modifiers(match.group(0)),
            )
            symbols.append(symbol)

        # Extract functions
        fn_pattern = r'(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+(\w+)(?:<[^>]*>)?\s*\((.*?)\)(?:\s*->\s*([\w<>,\s]+))?'
        for match in re.finditer(fn_pattern, content):
            fn_name = match.group(1)
            params_str = match.group(2) if match.group(2) else ""
            return_type = match.group(3) if match.group(3) else None
            line = content[:match.start()].count('\n') + 1

            # Determine if it's a method (inside impl block) or standalone function
            parent = self._find_parent_impl(content, match.start())
            kind = SymbolKind.METHOD if parent else SymbolKind.FUNCTION

            # Parse parameters
            parameters = self._parse_parameters(params_str)

            symbol = ParsedSymbol(
                name=fn_name,
                kind=kind,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=self._find_end_line(content, match.start()),
                end_column=0,
                signature=match.group(0),
                return_type=return_type,
                parameters=parameters,
                modifiers=self._extract_modifiers(match.group(0)),
                parent=parent,
            )
            symbols.append(symbol)

        # Extract constants
        const_pattern = r'(?:pub\s+)?const\s+(\w+)\s*:\s*([\w<>,\s]+)\s*='
        for match in re.finditer(const_pattern, content):
            const_name = match.group(1)
            const_type = match.group(2)
            line = content[:match.start()].count('\n') + 1

            symbol = ParsedSymbol(
                name=const_name,
                kind=SymbolKind.CONSTANT,
                file_path=file_path,
                line=line,
                column=match.start() - content.rfind('\n', 0, match.start()),
                end_line=line,
                end_column=0,
                return_type=const_type,
                modifiers=self._extract_modifiers(match.group(0)),
            )
            symbols.append(symbol)

        return symbols

    def extract_relationships(
        self, content: str, file_path: str, symbols: List[ParsedSymbol]
    ) -> List[ParsedRelationship]:
        """Extract relationships between Rust symbols.

        Args:
            content: Rust source code
            file_path: Path to the file
            symbols: Previously extracted symbols

        Returns:
            List of relationships
        """
        relationships = []

        # Extract function calls
        call_pattern = r'(\w+)(?:::\w+)*\s*\('
        for match in re.finditer(call_pattern, content):
            called_fn = match.group(1)
            line = content[:match.start()].count('\n') + 1

            # Find the calling function
            calling_fn = self._find_containing_function(symbols, line)
            if calling_fn:
                relationships.append(
                    ParsedRelationship(
                        source_symbol=calling_fn.name,
                        target_symbol=called_fn,
                        relationship_type="calls",
                        source_file=file_path,
                        target_file="",
                        line=line,
                        column=match.start() - content.rfind('\n', 0, match.start()),
                    )
                )

        # Extract trait implementations
        impl_trait_pattern = r'impl(?:<[^>]+>)?\s+(\w+)(?:<[^>]+>)?\s+for\s+(\w+)'
        for match in re.finditer(impl_trait_pattern, content):
            trait_name = match.group(1)
            struct_name = match.group(2)
            line = content[:match.start()].count('\n') + 1

            relationships.append(
                ParsedRelationship(
                    source_symbol=struct_name,
                    target_symbol=trait_name,
                    relationship_type="implements",
                    source_file=file_path,
                    target_file="",
                    line=line,
                    column=match.start() - content.rfind('\n', 0, match.start()),
                )
            )

        return relationships

    def extract_imports(self, content: str) -> List[str]:
        """Extract use statements from Rust code.

        Args:
            content: Rust source code

        Returns:
            List of imported modules/crates
        """
        imports = []

        # use std::collections::HashMap;
        use_pattern = r'use\s+([\w:]+(?:\s*{[^}]+})?)\s*;'
        for match in re.finditer(use_pattern, content):
            import_path = match.group(1)
            # Simplify grouped imports like "std::collections::{HashMap, HashSet}"
            if '{' in import_path:
                base = import_path.split('{')[0]
                items = import_path.split('{')[1].split('}')[0].split(',')
                for item in items:
                    imports.append(f"{base}{item.strip()}")
            else:
                imports.append(import_path)

        return imports

    def _extract_modifiers(self, declaration: str) -> List[str]:
        """Extract modifiers from a declaration."""
        modifiers = []
        modifier_keywords = ['pub', 'async', 'unsafe', 'const', 'mut']

        for keyword in modifier_keywords:
            if re.search(r'\b' + keyword + r'\b', declaration):
                modifiers.append(keyword)

        return modifiers

    def _parse_parameters(self, params_str: str) -> List[dict]:
        """Parse function parameters."""
        if not params_str.strip():
            return []

        parameters = []

        # Handle self parameter
        if params_str.strip().startswith('&self') or params_str.strip().startswith('self'):
            self_param = params_str.split(',')[0].strip()
            parameters.append({
                "name": "self",
                "type": self_param,
            })
            # Remove self from params_str
            if ',' in params_str:
                params_str = ','.join(params_str.split(',')[1:])
            else:
                params_str = ""

        if not params_str.strip():
            return parameters

        # Parse regular parameters: "name: Type"
        param_parts = self._smart_split(params_str, ',')

        for part in param_parts:
            part = part.strip()
            if not part:
                continue

            # Match "mut? name: Type"
            match = re.match(r'(?:mut\s+)?(\w+)\s*:\s*(.*)', part)
            if match:
                param_name = match.group(1)
                param_type = match.group(2).strip()
                parameters.append({
                    "name": param_name,
                    "type": param_type,
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

    def _find_parent_impl(self, content: str, position: int) -> Optional[str]:
        """Find the impl block that contains a given position."""
        before_position = content[:position]

        # Look for "impl StructName" or "impl Trait for StructName"
        impl_matches = list(re.finditer(r'impl(?:<[^>]+>)?\s+(?:\w+\s+for\s+)?(\w+)', before_position))

        if not impl_matches:
            return None

        # Get the last impl before this position
        last_impl = impl_matches[-1]
        struct_name = last_impl.group(1)

        return struct_name

    def _find_end_line(self, content: str, start_pos: int) -> int:
        """Find the end line of a symbol."""
        # Look for closing brace
        brace_count = 0
        found_opening = False
        for i in range(start_pos, len(content)):
            if content[i] == ';' and not found_opening:
                # Struct with no body (tuple struct or unit struct)
                return content[:i].count('\n') + 1
            elif content[i] == '{':
                found_opening = True
                brace_count += 1
            elif content[i] == '}':
                brace_count -= 1
                if found_opening and brace_count == 0:
                    return content[:i].count('\n') + 1

        # If no closing brace found, estimate
        return content[:start_pos].count('\n') + 10

    def _find_containing_function(self, symbols: List[ParsedSymbol], line: int) -> Optional[ParsedSymbol]:
        """Find the function that contains a given line."""
        for symbol in symbols:
            if symbol.kind in (SymbolKind.FUNCTION, SymbolKind.METHOD):
                if symbol.line <= line <= symbol.end_line:
                    return symbol
        return None
