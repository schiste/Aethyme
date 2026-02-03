"""Service for extracting code relationships from SCIP symbols.

This service analyzes code and extracts relationships like:
- Function calls
- Class inheritance
- Import dependencies
- References

These relationships are stored in the code_relationships table for graph analysis.
"""

import re
from typing import List, Dict, Any, Optional, Set
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, and_

from app.models.code_graph import CodeRelationship, SymbolMetadata, RelationshipType
from app.services.scip import SCIPSymbol, SCIPDocument
from app.services.tree_sitter import TreeSitterService, Symbol


class RelationshipExtractor:
    """Extract code relationships from parsed code."""

    def __init__(self, db: AsyncSession, repository_id: str):
        """Initialize relationship extractor.

        Args:
            db: Database session
            repository_id: Repository ID
        """
        self.db = db
        self.repository_id = repository_id
        self.tree_sitter = TreeSitterService()

    async def extract_from_scip_document(
        self, doc: SCIPDocument, file_content: str
    ) -> List[CodeRelationship]:
        """Extract relationships from a SCIP document.

        Args:
            doc: SCIP document
            file_content: Full file content for analysis

        Returns:
            List of code relationships
        """
        relationships = []

        # Extract relationships from each symbol
        for symbol in doc.symbols:
            # Extract function calls
            if symbol.kind in ("function", "method"):
                call_relationships = await self._extract_function_calls(
                    symbol, file_content, doc.language
                )
                relationships.extend(call_relationships)

            # Extract class inheritance
            if symbol.kind == "class":
                inheritance_relationships = await self._extract_inheritance(
                    symbol, file_content, doc.language
                )
                relationships.extend(inheritance_relationships)

            # Extract container relationships
            if symbol.parent_symbol_id:
                container_rel = CodeRelationship(
                    repository_id=self.repository_id,
                    source_symbol_id=symbol.symbol_id,
                    source_file_path=symbol.file_path,
                    source_name=symbol.name,
                    source_kind=symbol.kind,
                    target_symbol_id=symbol.parent_symbol_id,
                    target_file_path=symbol.file_path,
                    target_name=symbol.parent_symbol_id.split(":")[-1],
                    target_kind="class",
                    relationship_type=RelationshipType.CONTAINED_IN,
                    line_number=symbol.line_start,
                    column_number=symbol.col_start,
                    language=doc.language,
                )
                relationships.append(container_rel)

        # Extract file-level imports
        import_relationships = await self._extract_imports(doc, file_content)
        relationships.extend(import_relationships)

        return relationships

    async def _extract_function_calls(
        self, symbol: SCIPSymbol, file_content: str, language: str
    ) -> List[CodeRelationship]:
        """Extract function call relationships from a function/method.

        Args:
            symbol: SCIP symbol (function or method)
            file_content: Full file content
            language: Programming language

        Returns:
            List of call relationships
        """
        relationships = []

        # Extract the function body content
        lines = file_content.split("\n")
        if symbol.line_start < 1 or symbol.line_end > len(lines):
            return relationships

        function_body = "\n".join(lines[symbol.line_start - 1 : symbol.line_end])

        # Simple regex-based call extraction (language-specific patterns)
        if language == "go":
            # Match function calls: functionName(...)
            call_pattern = r"\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\("
        elif language == "python":
            # Match function calls and method calls
            call_pattern = r"\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\("
        elif language in ("javascript", "typescript"):
            # Match function calls
            call_pattern = r"\b([a-zA-Z_$][a-zA-Z0-9_$]*)\s*\("
        else:
            call_pattern = r"\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\("

        matches = re.finditer(call_pattern, function_body)
        called_functions: Set[str] = set()

        for match in matches:
            function_name = match.group(1)
            # Skip common keywords and the function itself
            if function_name in (
                "if",
                "for",
                "while",
                "switch",
                "return",
                "print",
                symbol.name,
            ):
                continue

            called_functions.add(function_name)

        # Create relationships for unique called functions
        for called_func in called_functions:
            # Generate target symbol ID (simplified - in real app, would need proper resolution)
            target_symbol_id = (
                f"{self.repository_id}:{symbol.file_path}:function:{called_func}"
            )

            rel = CodeRelationship(
                repository_id=self.repository_id,
                source_symbol_id=symbol.symbol_id,
                source_file_path=symbol.file_path,
                source_name=symbol.name,
                source_kind=symbol.kind,
                target_symbol_id=target_symbol_id,
                target_file_path=symbol.file_path,  # Assume same file for now
                target_name=called_func,
                target_kind="function",
                relationship_type=RelationshipType.CALLS,
                line_number=symbol.line_start,
                column_number=symbol.col_start,
                language=language,
            )
            relationships.append(rel)

        return relationships

    async def _extract_inheritance(
        self, symbol: SCIPSymbol, file_content: str, language: str
    ) -> List[CodeRelationship]:
        """Extract class inheritance relationships.

        Args:
            symbol: SCIP symbol (class)
            file_content: Full file content
            language: Programming language

        Returns:
            List of inheritance relationships
        """
        relationships = []

        # Extract the class definition line
        lines = file_content.split("\n")
        if symbol.line_start < 1 or symbol.line_start > len(lines):
            return relationships

        class_definition = lines[symbol.line_start - 1]

        # Language-specific inheritance patterns
        if language == "go":
            # Go doesn't have class inheritance (uses interfaces)
            return relationships
        elif language == "python":
            # Match: class MyClass(BaseClass):
            pattern = r"class\s+" + re.escape(symbol.name) + r"\s*\(([^)]+)\)"
        elif language in ("javascript", "typescript"):
            # Match: class MyClass extends BaseClass
            pattern = r"class\s+" + re.escape(symbol.name) + r"\s+extends\s+(\w+)"
        else:
            return relationships

        match = re.search(pattern, class_definition)
        if match:
            parent_classes = match.group(1).split(",")
            for parent_class in parent_classes:
                parent_class = parent_class.strip()
                if not parent_class:
                    continue

                # Generate parent symbol ID
                target_symbol_id = (
                    f"{self.repository_id}:{symbol.file_path}:class:{parent_class}"
                )

                rel = CodeRelationship(
                    repository_id=self.repository_id,
                    source_symbol_id=symbol.symbol_id,
                    source_file_path=symbol.file_path,
                    source_name=symbol.name,
                    source_kind=symbol.kind,
                    target_symbol_id=target_symbol_id,
                    target_file_path=symbol.file_path,  # Assume same file for now
                    target_name=parent_class,
                    target_kind="class",
                    relationship_type=RelationshipType.INHERITS,
                    line_number=symbol.line_start,
                    column_number=symbol.col_start,
                    language=language,
                )
                relationships.append(rel)

        return relationships

    async def _extract_imports(
        self, doc: SCIPDocument, file_content: str
    ) -> List[CodeRelationship]:
        """Extract import relationships from a file.

        Args:
            doc: SCIP document
            file_content: Full file content

        Returns:
            List of import relationships
        """
        relationships = []
        language = doc.language

        # Language-specific import patterns
        if language == "python":
            # Match: import module, from module import name
            import_pattern = r"^\s*(?:from\s+(\S+)\s+)?import\s+(.+?)(?:\s+as\s+\S+)?$"
        elif language == "go":
            # Match: import "package" or import ("package1" "package2")
            import_pattern = r'import\s+(?:"([^"]+)"|(\([^)]+\)))'
        elif language in ("javascript", "typescript"):
            # Match: import { name } from 'module'
            import_pattern = r'import\s+.*?\s+from\s+["\']([^"\']+)["\']'
        else:
            return relationships

        lines = file_content.split("\n")
        for line_num, line in enumerate(lines, start=1):
            match = re.search(import_pattern, line)
            if match:
                imported_module = match.group(1) if match.group(1) else match.group(2)
                if not imported_module:
                    continue

                # Create a simplified import relationship
                # Source is the current file, target is the imported module
                source_symbol_id = f"{self.repository_id}:{doc.file_path}:file"
                target_symbol_id = f"{self.repository_id}:module:{imported_module}"

                rel = CodeRelationship(
                    repository_id=self.repository_id,
                    source_symbol_id=source_symbol_id,
                    source_file_path=doc.file_path,
                    source_name=doc.file_path,
                    source_kind="file",
                    target_symbol_id=target_symbol_id,
                    target_file_path=imported_module,
                    target_name=imported_module,
                    target_kind="module",
                    relationship_type=RelationshipType.IMPORTS,
                    line_number=line_num,
                    column_number=0,
                    language=language,
                )
                relationships.append(rel)

        return relationships

    async def save_relationships(
        self, relationships: List[CodeRelationship]
    ) -> Dict[str, int]:
        """Save relationships to database.

        Args:
            relationships: List of code relationships

        Returns:
            Statistics about saved relationships
        """
        if not relationships:
            return {"saved": 0, "skipped": 0, "errors": 0}

        saved = 0
        skipped = 0
        errors = 0

        for rel in relationships:
            try:
                # Check if relationship already exists
                stmt = select(CodeRelationship).where(
                    and_(
                        CodeRelationship.source_symbol_id == rel.source_symbol_id,
                        CodeRelationship.target_symbol_id == rel.target_symbol_id,
                        CodeRelationship.relationship_type == rel.relationship_type,
                    )
                )
                result = await self.db.execute(stmt)
                existing = result.scalar_one_or_none()

                if existing:
                    skipped += 1
                    continue

                # Save new relationship
                self.db.add(rel)
                saved += 1

            except Exception as e:
                errors += 1
                print(f"Error saving relationship: {e}")

        # Commit all relationships
        try:
            await self.db.commit()
        except Exception as e:
            await self.db.rollback()
            print(f"Error committing relationships: {e}")
            return {"saved": 0, "skipped": skipped, "errors": errors + saved}

        return {"saved": saved, "skipped": skipped, "errors": errors}

    async def update_symbol_metadata(
        self, symbols: List[SCIPSymbol]
    ) -> Dict[str, int]:
        """Update or create symbol metadata with computed metrics.

        Args:
            symbols: List of SCIP symbols

        Returns:
            Statistics about updated metadata
        """
        updated = 0

        for symbol in symbols:
            # Calculate metrics
            inbound_calls = await self._count_relationships(
                symbol.symbol_id, RelationshipType.CALLED_BY
            )
            outbound_calls = await self._count_relationships(
                symbol.symbol_id, RelationshipType.CALLS
            )
            dependencies = await self._count_relationships(
                symbol.symbol_id, RelationshipType.IMPORTS
            )

            # Simple impact score: inbound_calls * 2 + dependencies
            impact_score = min(inbound_calls * 2 + dependencies, 100)

            # Check if metadata exists
            stmt = select(SymbolMetadata).where(
                SymbolMetadata.symbol_id == symbol.symbol_id
            )
            result = await self.db.execute(stmt)
            metadata = result.scalar_one_or_none()

            if metadata:
                # Update existing
                metadata.inbound_call_count = inbound_calls
                metadata.outbound_call_count = outbound_calls
                metadata.dependency_count = dependencies
                metadata.impact_score = impact_score
            else:
                # Create new
                metadata = SymbolMetadata(
                    symbol_id=symbol.symbol_id,
                    repository_id=self.repository_id,
                    inbound_call_count=inbound_calls,
                    outbound_call_count=outbound_calls,
                    dependency_count=dependencies,
                    impact_score=impact_score,
                )
                self.db.add(metadata)

            updated += 1

        await self.db.commit()
        return {"updated": updated}

    async def _count_relationships(
        self, symbol_id: str, relationship_type: RelationshipType
    ) -> int:
        """Count relationships of a specific type for a symbol.

        Args:
            symbol_id: Symbol ID
            relationship_type: Type of relationship

        Returns:
            Count of relationships
        """
        stmt = select(CodeRelationship).where(
            and_(
                CodeRelationship.target_symbol_id == symbol_id,
                CodeRelationship.relationship_type == relationship_type,
            )
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()
        return len(relationships)
