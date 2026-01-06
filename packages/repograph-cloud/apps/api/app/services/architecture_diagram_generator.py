"""Architecture Diagram Generator.

Generate visual architecture diagrams from code structure:
- Component diagrams from modules and packages
- Dependency diagrams from import relationships
- Call flow diagrams from function relationships
- Output formats: Mermaid, PlantUML, DOT (Graphviz)
"""

from typing import List, Dict, Any, Set
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select

from app.models.code_graph import CodeRelationship, RelationshipType


class ArchitectureDiagramGenerator:
    """Generate architecture diagrams from code relationships."""

    def __init__(self, db: AsyncSession, repository_id: str):
        """Initialize diagram generator.

        Args:
            db: Database session
            repository_id: Repository ID
        """
        self.db = db
        self.repository_id = repository_id

    async def generate_component_diagram(
        self, format: str = "mermaid"
    ) -> Dict[str, Any]:
        """Generate component diagram showing modules/packages.

        Args:
            format: Output format (mermaid, plantuml, dot)

        Returns:
            Diagram data with source code
        """
        # Get all import relationships to understand dependencies
        stmt = select(CodeRelationship).where(
            CodeRelationship.repository_id == self.repository_id,
            CodeRelationship.relationship_type == RelationshipType.IMPORTS,
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        # Extract unique modules
        modules = set()
        dependencies = []

        for rel in relationships:
            source_module = self._extract_module(rel.source_file_path)
            target_module = self._extract_module(rel.target_file_path)

            if source_module:
                modules.add(source_module)
            if target_module:
                modules.add(target_module)

            if source_module and target_module and source_module != target_module:
                dependencies.append((source_module, target_module))

        # Generate diagram based on format
        if format == "mermaid":
            diagram = self._generate_mermaid_component_diagram(
                list(modules), dependencies
            )
        elif format == "plantuml":
            diagram = self._generate_plantuml_component_diagram(
                list(modules), dependencies
            )
        elif format == "dot":
            diagram = self._generate_dot_component_diagram(list(modules), dependencies)
        else:
            diagram = "Unsupported format"

        return {
            "type": "component_diagram",
            "format": format,
            "diagram": diagram,
            "modules": list(modules),
            "dependency_count": len(dependencies),
        }

    async def generate_dependency_graph(
        self, format: str = "mermaid", max_depth: int = 3
    ) -> Dict[str, Any]:
        """Generate dependency graph showing file dependencies.

        Args:
            format: Output format
            max_depth: Maximum depth to traverse

        Returns:
            Dependency graph diagram
        """
        # Get all relationships
        stmt = select(CodeRelationship).where(
            CodeRelationship.repository_id == self.repository_id
        )
        result = await self.db.execute(stmt)
        relationships = result.scalars().all()

        # Build graph of file dependencies
        file_deps = {}
        for rel in relationships:
            if rel.source_file_path not in file_deps:
                file_deps[rel.source_file_path] = set()
            file_deps[rel.source_file_path].add(rel.target_file_path)

        # Generate diagram
        if format == "mermaid":
            diagram = self._generate_mermaid_dependency_graph(file_deps)
        elif format == "plantuml":
            diagram = self._generate_plantuml_dependency_graph(file_deps)
        elif format == "dot":
            diagram = self._generate_dot_dependency_graph(file_deps)
        else:
            diagram = "Unsupported format"

        return {
            "type": "dependency_graph",
            "format": format,
            "diagram": diagram,
            "file_count": len(file_deps),
        }

    async def generate_call_flow_diagram(
        self, entry_point_symbol_id: str, format: str = "mermaid"
    ) -> Dict[str, Any]:
        """Generate call flow diagram from an entry point.

        Args:
            entry_point_symbol_id: Symbol ID to start from
            format: Output format

        Returns:
            Call flow diagram
        """
        # Get all call relationships starting from entry point
        visited = set()
        call_chain = []

        await self._traverse_calls(entry_point_symbol_id, visited, call_chain, depth=0)

        # Generate sequence diagram
        if format == "mermaid":
            diagram = self._generate_mermaid_sequence_diagram(call_chain)
        elif format == "plantuml":
            diagram = self._generate_plantuml_sequence_diagram(call_chain)
        else:
            diagram = "Unsupported format"

        return {
            "type": "call_flow_diagram",
            "format": format,
            "diagram": diagram,
            "call_count": len(call_chain),
        }

    async def _traverse_calls(
        self,
        symbol_id: str,
        visited: Set[str],
        call_chain: List[Dict],
        depth: int,
        max_depth: int = 5,
    ):
        """Recursively traverse function calls."""
        if depth >= max_depth or symbol_id in visited:
            return

        visited.add(symbol_id)

        # Find all functions called by this symbol
        stmt = select(CodeRelationship).where(
            CodeRelationship.source_symbol_id == symbol_id,
            CodeRelationship.relationship_type == RelationshipType.CALLS,
        )
        result = await self.db.execute(stmt)
        calls = result.scalars().all()

        for call in calls:
            call_chain.append(
                {
                    "caller": call.source_name,
                    "callee": call.target_name,
                    "depth": depth,
                }
            )
            await self._traverse_calls(
                call.target_symbol_id, visited, call_chain, depth + 1, max_depth
            )

    def _extract_module(self, file_path: str) -> str:
        """Extract module name from file path."""
        # Remove file extension and get directory
        parts = file_path.split("/")
        if len(parts) > 1:
            return parts[0]  # First directory is the module
        return ""

    def _generate_mermaid_component_diagram(
        self, modules: List[str], dependencies: List[tuple]
    ) -> str:
        """Generate Mermaid component diagram."""
        lines = ["graph TD"]

        # Add module nodes
        for i, module in enumerate(modules):
            lines.append(f"    {self._sanitize_id(module)}[{module}]")

        # Add dependencies
        for source, target in dependencies:
            lines.append(
                f"    {self._sanitize_id(source)} --> {self._sanitize_id(target)}"
            )

        return "\n".join(lines)

    def _generate_mermaid_dependency_graph(
        self, file_deps: Dict[str, Set[str]]
    ) -> str:
        """Generate Mermaid dependency graph."""
        lines = ["graph LR"]

        # Limit to top-level files to avoid clutter
        for source, targets in list(file_deps.items())[:20]:
            source_id = self._sanitize_id(source)
            lines.append(f"    {source_id}[\"{source}\"]")

            for target in list(targets)[:5]:  # Limit edges per node
                target_id = self._sanitize_id(target)
                lines.append(f"    {source_id} --> {target_id}")

        return "\n".join(lines)

    def _generate_mermaid_sequence_diagram(self, call_chain: List[Dict]) -> str:
        """Generate Mermaid sequence diagram."""
        lines = ["sequenceDiagram"]

        # Add participants
        participants = set()
        for call in call_chain:
            participants.add(call["caller"])
            participants.add(call["callee"])

        for participant in participants:
            lines.append(f"    participant {self._sanitize_id(participant)}")

        # Add calls
        for call in call_chain:
            caller_id = self._sanitize_id(call["caller"])
            callee_id = self._sanitize_id(call["callee"])
            lines.append(f"    {caller_id}->>+{callee_id}: call")

        return "\n".join(lines)

    def _generate_plantuml_component_diagram(
        self, modules: List[str], dependencies: List[tuple]
    ) -> str:
        """Generate PlantUML component diagram."""
        lines = ["@startuml", ""]

        # Add components
        for module in modules:
            lines.append(f'component "{module}" as {self._sanitize_id(module)}')

        lines.append("")

        # Add dependencies
        for source, target in dependencies:
            lines.append(
                f"{self._sanitize_id(source)} --> {self._sanitize_id(target)}"
            )

        lines.append("@enduml")
        return "\n".join(lines)

    def _generate_plantuml_dependency_graph(
        self, file_deps: Dict[str, Set[str]]
    ) -> str:
        """Generate PlantUML dependency diagram."""
        lines = ["@startuml", ""]

        for source, targets in list(file_deps.items())[:20]:
            for target in list(targets)[:5]:
                lines.append(f'"{source}" --> "{target}"')

        lines.append("@enduml")
        return "\n".join(lines)

    def _generate_plantuml_sequence_diagram(self, call_chain: List[Dict]) -> str:
        """Generate PlantUML sequence diagram."""
        lines = ["@startuml", ""]

        for call in call_chain:
            lines.append(f'{call["caller"]} -> {call["callee"]}: call()')

        lines.append("@enduml")
        return "\n".join(lines)

    def _generate_dot_component_diagram(
        self, modules: List[str], dependencies: List[tuple]
    ) -> str:
        """Generate DOT (Graphviz) component diagram."""
        lines = ["digraph G {", '  node [shape=box];', ""]

        # Add nodes
        for module in modules:
            lines.append(f'  "{module}";')

        lines.append("")

        # Add edges
        for source, target in dependencies:
            lines.append(f'  "{source}" -> "{target}";')

        lines.append("}")
        return "\n".join(lines)

    def _generate_dot_dependency_graph(self, file_deps: Dict[str, Set[str]]) -> str:
        """Generate DOT dependency graph."""
        lines = ["digraph G {", ""]

        for source, targets in list(file_deps.items())[:20]:
            for target in list(targets)[:5]:
                lines.append(f'  "{source}" -> "{target}";')

        lines.append("}")
        return "\n".join(lines)

    def _sanitize_id(self, name: str) -> str:
        """Sanitize name for use as diagram ID."""
        return name.replace("/", "_").replace(".", "_").replace("-", "_")
