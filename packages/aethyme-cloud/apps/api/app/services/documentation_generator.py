"""Documentation Generation Service.

Automatically generate comprehensive documentation from code analysis:
- API documentation from function signatures and docstrings
- Architecture diagrams from code structure
- Dependency graphs from import relationships
- README files from repository analysis
"""

import re
from typing import List, Dict, Any, Optional
from datetime import datetime
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.models.code_graph import CodeRelationship, SymbolMetadata, RelationshipType
from app.services.scip import SCIPSymbol, SCIPDocument


class APIDocumentationGenerator:
    """Generate API documentation from code symbols."""

    def __init__(self, repository_id: str):
        """Initialize API documentation generator.

        Args:
            repository_id: Repository ID
        """
        self.repository_id = repository_id

    async def generate_from_symbols(
        self, symbols: List[SCIPSymbol], language: str
    ) -> Dict[str, Any]:
        """Generate API documentation from symbols.

        Args:
            symbols: List of SCIP symbols
            language: Programming language

        Returns:
            Documentation structure
        """
        # Group symbols by file and kind
        files = {}
        for symbol in symbols:
            if symbol.file_path not in files:
                files[symbol.file_path] = {
                    "path": symbol.file_path,
                    "language": language,
                    "functions": [],
                    "classes": [],
                    "constants": [],
                    "types": [],
                }

            if symbol.kind in ("function", "method"):
                files[symbol.file_path]["functions"].append(
                    self._format_function_doc(symbol, language)
                )
            elif symbol.kind == "class":
                files[symbol.file_path]["classes"].append(
                    self._format_class_doc(symbol, language)
                )
            elif symbol.kind == "constant":
                files[symbol.file_path]["constants"].append(
                    self._format_constant_doc(symbol)
                )
            elif symbol.kind in ("type", "interface"):
                files[symbol.file_path]["types"].append(
                    self._format_type_doc(symbol, language)
                )

        return {
            "repository_id": self.repository_id,
            "language": language,
            "generated_at": datetime.utcnow().isoformat(),
            "files": list(files.values()),
            "statistics": self._calculate_statistics(files),
        }

    def _format_function_doc(
        self, symbol: SCIPSymbol, language: str
    ) -> Dict[str, Any]:
        """Format function documentation.

        Args:
            symbol: Function symbol
            language: Programming language

        Returns:
            Formatted function documentation
        """
        # Extract parameters from signature
        params = self._extract_parameters(symbol.signature, language)

        # Extract return type
        return_type = self._extract_return_type(symbol.signature, language)

        # Parse docstring for description and param docs
        parsed_docstring = self._parse_docstring(
            symbol.documentation or "", language
        )

        return {
            "name": symbol.name,
            "signature": symbol.signature,
            "description": parsed_docstring.get("description", ""),
            "parameters": params,
            "returns": {
                "type": return_type,
                "description": parsed_docstring.get("returns", ""),
            },
            "examples": parsed_docstring.get("examples", []),
            "location": {
                "file": symbol.file_path,
                "line": symbol.line_start,
            },
            "visibility": self._determine_visibility(symbol.name, language),
        }

    def _format_class_doc(self, symbol: SCIPSymbol, language: str) -> Dict[str, Any]:
        """Format class documentation.

        Args:
            symbol: Class symbol
            language: Programming language

        Returns:
            Formatted class documentation
        """
        parsed_docstring = self._parse_docstring(
            symbol.documentation or "", language
        )

        return {
            "name": symbol.name,
            "description": parsed_docstring.get("description", ""),
            "attributes": parsed_docstring.get("attributes", []),
            "methods": [],  # Will be populated separately
            "location": {
                "file": symbol.file_path,
                "line": symbol.line_start,
            },
            "visibility": self._determine_visibility(symbol.name, language),
        }

    def _format_constant_doc(self, symbol: SCIPSymbol) -> Dict[str, Any]:
        """Format constant documentation."""
        return {
            "name": symbol.name,
            "description": symbol.documentation or "",
            "location": {
                "file": symbol.file_path,
                "line": symbol.line_start,
            },
        }

    def _format_type_doc(self, symbol: SCIPSymbol, language: str) -> Dict[str, Any]:
        """Format type/interface documentation."""
        parsed_docstring = self._parse_docstring(
            symbol.documentation or "", language
        )

        return {
            "name": symbol.name,
            "kind": symbol.kind,
            "description": parsed_docstring.get("description", ""),
            "fields": parsed_docstring.get("fields", []),
            "location": {
                "file": symbol.file_path,
                "line": symbol.line_start,
            },
        }

    def _extract_parameters(
        self, signature: Optional[str], language: str
    ) -> List[Dict[str, str]]:
        """Extract parameters from function signature.

        Args:
            signature: Function signature
            language: Programming language

        Returns:
            List of parameters with names and types
        """
        if not signature:
            return []

        params = []

        if language == "go":
            # Go: func Name(param1 type1, param2 type2) returnType
            match = re.search(r"\((.*?)\)", signature)
            if match:
                params_str = match.group(1)
                for param in params_str.split(","):
                    param = param.strip()
                    if param:
                        parts = param.rsplit(" ", 1)
                        if len(parts) == 2:
                            params.append({"name": parts[0], "type": parts[1]})
                        else:
                            params.append({"name": param, "type": "unknown"})

        elif language == "python":
            # Python: def name(param1: type1, param2: type2) -> returnType
            match = re.search(r"\((.*?)\)", signature)
            if match:
                params_str = match.group(1)
                for param in params_str.split(","):
                    param = param.strip()
                    if param and param != "self":
                        if ":" in param:
                            name, type_str = param.split(":", 1)
                            # Remove default values
                            type_str = type_str.split("=")[0].strip()
                            params.append({"name": name.strip(), "type": type_str})
                        else:
                            name = param.split("=")[0].strip()
                            params.append({"name": name, "type": "Any"})

        elif language in ("javascript", "typescript"):
            # TypeScript: function name(param1: type1, param2: type2): returnType
            match = re.search(r"\((.*?)\)", signature)
            if match:
                params_str = match.group(1)
                for param in params_str.split(","):
                    param = param.strip()
                    if param:
                        if ":" in param:
                            name, type_str = param.split(":", 1)
                            params.append({"name": name.strip(), "type": type_str.strip()})
                        else:
                            params.append({"name": param, "type": "any"})

        return params

    def _extract_return_type(
        self, signature: Optional[str], language: str
    ) -> Optional[str]:
        """Extract return type from function signature."""
        if not signature:
            return None

        if language == "go":
            # Go: func Name(...) returnType
            match = re.search(r"\)\s*(.+?)(?:\s*\{|$)", signature)
            if match:
                return match.group(1).strip()

        elif language == "python":
            # Python: def name(...) -> returnType
            match = re.search(r"->\s*(.+?)(?:\s*:|$)", signature)
            if match:
                return match.group(1).strip()

        elif language in ("javascript", "typescript"):
            # TypeScript: function name(...): returnType
            match = re.search(r"\):\s*(.+?)(?:\s*\{|$)", signature)
            if match:
                return match.group(1).strip()

        return None

    def _parse_docstring(
        self, docstring: str, language: str
    ) -> Dict[str, Any]:
        """Parse docstring into structured format.

        Supports:
        - Google style (Python)
        - JSDoc style (JavaScript/TypeScript)
        - GoDoc style (Go)
        """
        result = {
            "description": "",
            "parameters": [],
            "returns": "",
            "examples": [],
            "attributes": [],
            "fields": [],
        }

        if not docstring:
            return result

        lines = docstring.strip().split("\n")
        current_section = "description"
        description_lines = []
        param_lines = []
        return_lines = []
        example_lines = []

        for line in lines:
            line = line.strip()

            # Detect sections
            if re.match(r"^(Args?|Parameters?|Params?):", line, re.IGNORECASE):
                current_section = "params"
                continue
            elif re.match(r"^Returns?:", line, re.IGNORECASE):
                current_section = "returns"
                continue
            elif re.match(r"^Examples?:", line, re.IGNORECASE):
                current_section = "examples"
                continue
            elif re.match(r"^@param", line):
                current_section = "params"
            elif re.match(r"^@returns?", line):
                current_section = "returns"

            # Add to appropriate section
            if current_section == "description":
                description_lines.append(line)
            elif current_section == "params":
                param_lines.append(line)
            elif current_section == "returns":
                return_lines.append(line)
            elif current_section == "examples":
                example_lines.append(line)

        result["description"] = " ".join(description_lines).strip()
        result["returns"] = " ".join(return_lines).strip()

        if example_lines:
            result["examples"] = ["\n".join(example_lines)]

        return result

    def _determine_visibility(self, name: str, language: str) -> str:
        """Determine if symbol is public or private."""
        if language == "go":
            # Go: uppercase first letter = public
            return "public" if name[0].isupper() else "private"
        elif language == "python":
            # Python: underscore prefix = private
            return "private" if name.startswith("_") else "public"
        elif language in ("javascript", "typescript"):
            # JS/TS: # prefix = private (modern)
            return "private" if name.startswith("#") else "public"
        return "public"

    def _calculate_statistics(self, files: Dict[str, Any]) -> Dict[str, int]:
        """Calculate documentation statistics."""
        stats = {
            "total_files": len(files),
            "total_functions": 0,
            "total_classes": 0,
            "total_constants": 0,
            "total_types": 0,
            "documented_functions": 0,
            "documented_classes": 0,
        }

        for file_data in files.values():
            stats["total_functions"] += len(file_data["functions"])
            stats["total_classes"] += len(file_data["classes"])
            stats["total_constants"] += len(file_data["constants"])
            stats["total_types"] += len(file_data["types"])

            for func in file_data["functions"]:
                if func.get("description"):
                    stats["documented_functions"] += 1

            for cls in file_data["classes"]:
                if cls.get("description"):
                    stats["documented_classes"] += 1

        # Calculate coverage
        if stats["total_functions"] > 0:
            stats["function_coverage"] = int(
                (stats["documented_functions"] / stats["total_functions"]) * 100
            )
        else:
            stats["function_coverage"] = 0

        if stats["total_classes"] > 0:
            stats["class_coverage"] = int(
                (stats["documented_classes"] / stats["total_classes"]) * 100
            )
        else:
            stats["class_coverage"] = 0

        return stats

    def generate_markdown(self, docs: Dict[str, Any]) -> str:
        """Generate Markdown documentation.

        Args:
            docs: Documentation structure

        Returns:
            Markdown formatted documentation
        """
        md_lines = []

        # Header
        md_lines.append(f"# API Documentation")
        md_lines.append(f"\n**Repository**: {docs['repository_id']}")
        md_lines.append(f"**Language**: {docs['language']}")
        md_lines.append(f"**Generated**: {docs['generated_at']}\n")

        # Statistics
        stats = docs["statistics"]
        md_lines.append("## Statistics\n")
        md_lines.append(f"- **Files**: {stats['total_files']}")
        md_lines.append(f"- **Functions**: {stats['total_functions']} ({stats['function_coverage']}% documented)")
        md_lines.append(f"- **Classes**: {stats['total_classes']} ({stats['class_coverage']}% documented)")
        md_lines.append(f"- **Types**: {stats['total_types']}\n")

        # Table of Contents
        md_lines.append("## Table of Contents\n")
        for file_data in docs["files"]:
            md_lines.append(f"- [{file_data['path']}](#{self._slugify(file_data['path'])})")
        md_lines.append("")

        # File sections
        for file_data in docs["files"]:
            md_lines.append(f"## {file_data['path']}\n")

            # Functions
            if file_data["functions"]:
                md_lines.append("### Functions\n")
                for func in file_data["functions"]:
                    md_lines.append(f"#### `{func['name']}`\n")
                    if func["signature"]:
                        md_lines.append(f"```{file_data['language']}")
                        md_lines.append(func["signature"])
                        md_lines.append("```\n")
                    if func["description"]:
                        md_lines.append(func["description"] + "\n")

                    if func["parameters"]:
                        md_lines.append("**Parameters:**\n")
                        for param in func["parameters"]:
                            md_lines.append(f"- `{param['name']}` ({param['type']})")
                        md_lines.append("")

                    if func["returns"]["type"]:
                        md_lines.append(f"**Returns:** `{func['returns']['type']}`\n")
                        if func["returns"]["description"]:
                            md_lines.append(func["returns"]["description"] + "\n")

            # Classes
            if file_data["classes"]:
                md_lines.append("### Classes\n")
                for cls in file_data["classes"]:
                    md_lines.append(f"#### `{cls['name']}`\n")
                    if cls["description"]:
                        md_lines.append(cls["description"] + "\n")

        return "\n".join(md_lines)

    def _slugify(self, text: str) -> str:
        """Convert text to slug for markdown links."""
        return text.lower().replace("/", "-").replace(".", "-").replace(" ", "-")
