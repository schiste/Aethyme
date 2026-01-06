"""Schema-first planning and validation for safe code generation.

This module ensures that schemas, routes, and API contracts are defined
before code generation, preventing drift and ensuring consistency.
"""

import re
import json
from typing import Dict, List, Optional, Set, Any
from dataclasses import dataclass, field
from pathlib import Path
from enum import Enum
import structlog

logger = structlog.get_logger(__name__)


class SchemaType(str, Enum):
    """Types of schemas that can be extracted."""
    OPENAPI = "openapi"
    GRAPHQL = "graphql"
    TYPESCRIPT = "typescript"
    PYTHON_PYDANTIC = "python_pydantic"
    JSON_SCHEMA = "json_schema"
    PROTOBUF = "protobuf"


@dataclass
class ExtractedSchema:
    """Extracted schema information from codebase."""

    schema_type: SchemaType
    file_path: str
    content: str
    entities: List[str] = field(default_factory=list)
    endpoints: List[str] = field(default_factory=list)
    operations: List[str] = field(default_factory=list)
    dependencies: Set[str] = field(default_factory=set)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            'schema_type': self.schema_type.value,
            'file_path': self.file_path,
            'entities': self.entities,
            'endpoints': self.endpoints,
            'operations': self.operations,
            'dependencies': list(self.dependencies),
            'metadata': self.metadata,
        }


class SchemaExtractor:
    """Extract schemas from codebase for analysis."""

    def __init__(self, repo_root: str):
        """Initialize schema extractor.

        Args:
            repo_root: Root directory of the repository
        """
        self.repo_root = Path(repo_root)
        self.schemas: List[ExtractedSchema] = []

    def extract_all(self) -> List[ExtractedSchema]:
        """Extract all schemas from the repository.

        Returns:
            List of extracted schemas
        """
        self.schemas = []

        # Extract OpenAPI schemas
        self.schemas.extend(self._extract_openapi())

        # Extract GraphQL schemas
        self.schemas.extend(self._extract_graphql())

        # Extract TypeScript interfaces
        self.schemas.extend(self._extract_typescript())

        # Extract Pydantic models
        self.schemas.extend(self._extract_pydantic())

        # Extract JSON schemas
        self.schemas.extend(self._extract_json_schema())

        logger.info("Schema extraction complete", count=len(self.schemas))
        return self.schemas

    def _extract_openapi(self) -> List[ExtractedSchema]:
        """Extract OpenAPI/Swagger schemas."""
        schemas = []

        # Common OpenAPI file patterns
        patterns = [
            'openapi.yaml', 'openapi.yml', 'openapi.json',
            'swagger.yaml', 'swagger.yml', 'swagger.json',
            'api-spec.yaml', 'api-spec.yml',
        ]

        for pattern in patterns:
            for file_path in self.repo_root.rglob(pattern):
                try:
                    content = file_path.read_text()

                    # Parse endpoints
                    endpoints = self._parse_openapi_endpoints(content)

                    # Parse entities/models
                    entities = self._parse_openapi_entities(content)

                    schema = ExtractedSchema(
                        schema_type=SchemaType.OPENAPI,
                        file_path=str(file_path.relative_to(self.repo_root)),
                        content=content,
                        endpoints=endpoints,
                        entities=entities,
                    )
                    schemas.append(schema)
                    logger.debug("Extracted OpenAPI schema", file=str(file_path))

                except Exception as e:
                    logger.warning("Failed to extract OpenAPI", file=str(file_path), error=str(e))

        return schemas

    def _extract_graphql(self) -> List[ExtractedSchema]:
        """Extract GraphQL schemas."""
        schemas = []

        for file_path in self.repo_root.rglob('*.graphql'):
            try:
                content = file_path.read_text()

                # Parse types
                type_pattern = r'type\s+(\w+)\s*{'
                types = re.findall(type_pattern, content)

                # Parse queries/mutations
                query_pattern = r'(?:query|mutation)\s+(\w+)'
                queries = re.findall(query_pattern, content)

                schema = ExtractedSchema(
                    schema_type=SchemaType.GRAPHQL,
                    file_path=str(file_path.relative_to(self.repo_root)),
                    content=content,
                    entities=types,
                    operations=queries,
                )
                schemas.append(schema)
                logger.debug("Extracted GraphQL schema", file=str(file_path))

            except Exception as e:
                logger.warning("Failed to extract GraphQL", file=str(file_path), error=str(e))

        return schemas

    def _extract_typescript(self) -> List[ExtractedSchema]:
        """Extract TypeScript interfaces and types."""
        schemas = []

        for file_path in self.repo_root.rglob('*.ts'):
            # Skip test files and node_modules
            if 'node_modules' in str(file_path) or '.test.ts' in str(file_path):
                continue

            try:
                content = file_path.read_text()

                # Parse interfaces
                interface_pattern = r'(?:export\s+)?interface\s+(\w+)'
                interfaces = re.findall(interface_pattern, content)

                # Parse types
                type_pattern = r'(?:export\s+)?type\s+(\w+)\s*='
                types = re.findall(type_pattern, content)

                entities = list(set(interfaces + types))

                if entities:
                    schema = ExtractedSchema(
                        schema_type=SchemaType.TYPESCRIPT,
                        file_path=str(file_path.relative_to(self.repo_root)),
                        content=content,
                        entities=entities,
                    )
                    schemas.append(schema)

            except Exception as e:
                logger.warning("Failed to extract TypeScript", file=str(file_path), error=str(e))

        return schemas

    def _extract_pydantic(self) -> List[ExtractedSchema]:
        """Extract Pydantic models from Python files."""
        schemas = []

        for file_path in self.repo_root.rglob('*.py'):
            # Skip test files
            if 'test_' in file_path.name or '__pycache__' in str(file_path):
                continue

            try:
                content = file_path.read_text()

                # Skip if no pydantic import
                if 'pydantic' not in content and 'BaseModel' not in content:
                    continue

                # Parse BaseModel classes
                model_pattern = r'class\s+(\w+)\s*\([^)]*BaseModel[^)]*\)'
                models = re.findall(model_pattern, content)

                if models:
                    schema = ExtractedSchema(
                        schema_type=SchemaType.PYTHON_PYDANTIC,
                        file_path=str(file_path.relative_to(self.repo_root)),
                        content=content,
                        entities=models,
                    )
                    schemas.append(schema)

            except Exception as e:
                logger.warning("Failed to extract Pydantic", file=str(file_path), error=str(e))

        return schemas

    def _extract_json_schema(self) -> List[ExtractedSchema]:
        """Extract JSON Schema files."""
        schemas = []

        for file_path in self.repo_root.rglob('*.schema.json'):
            try:
                content = file_path.read_text()
                data = json.loads(content)

                # Extract entities from definitions or properties
                entities = []
                if 'definitions' in data:
                    entities.extend(data['definitions'].keys())
                if '$defs' in data:
                    entities.extend(data['$defs'].keys())

                schema = ExtractedSchema(
                    schema_type=SchemaType.JSON_SCHEMA,
                    file_path=str(file_path.relative_to(self.repo_root)),
                    content=content,
                    entities=entities,
                )
                schemas.append(schema)

            except Exception as e:
                logger.warning("Failed to extract JSON Schema", file=str(file_path), error=str(e))

        return schemas

    def _parse_openapi_endpoints(self, content: str) -> List[str]:
        """Parse endpoints from OpenAPI content."""
        endpoints = []

        # Simple pattern matching for paths
        path_pattern = r'^\s*(/[/\w\-{}]+):\s*$'

        for line in content.split('\n'):
            match = re.match(path_pattern, line)
            if match:
                endpoints.append(match.group(1))

        return endpoints

    def _parse_openapi_entities(self, content: str) -> List[str]:
        """Parse entity/model names from OpenAPI content."""
        entities = []

        # Look for schema definitions
        schema_pattern = r'^\s*(\w+):\s*$'
        in_schemas_section = False

        for line in content.split('\n'):
            if 'schemas:' in line or 'definitions:' in line:
                in_schemas_section = True
                continue

            if in_schemas_section:
                # Stop at other top-level keys
                if line and not line.startswith(' ') and not line.startswith('\t'):
                    in_schemas_section = False
                    continue

                match = re.match(schema_pattern, line)
                if match:
                    entities.append(match.group(1))

        return entities

    def get_routes(self) -> List[str]:
        """Get all API routes/endpoints from extracted schemas.

        Returns:
            List of route paths
        """
        routes = []
        for schema in self.schemas:
            routes.extend(schema.endpoints)
        return list(set(routes))

    def get_entities(self) -> List[str]:
        """Get all entities/models from extracted schemas.

        Returns:
            List of entity names
        """
        entities = []
        for schema in self.schemas:
            entities.extend(schema.entities)
        return list(set(entities))


@dataclass
class ValidationResult:
    """Result of schema validation."""

    is_valid: bool
    errors: List[str] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    suggestions: List[str] = field(default_factory=list)

    def add_error(self, message: str):
        """Add an error."""
        self.is_valid = False
        self.errors.append(message)

    def add_warning(self, message: str):
        """Add a warning."""
        self.warnings.append(message)

    def add_suggestion(self, message: str):
        """Add a suggestion."""
        self.suggestions.append(message)


class SchemaValidator:
    """Validate operations against schemas."""

    def __init__(self, schemas: List[ExtractedSchema]):
        """Initialize validator.

        Args:
            schemas: List of extracted schemas
        """
        self.schemas = schemas
        self._build_indexes()

    def _build_indexes(self):
        """Build indexes for fast lookup."""
        self.endpoints = set()
        self.entities = set()

        for schema in self.schemas:
            self.endpoints.update(schema.endpoints)
            self.entities.update(schema.entities)

    def validate_endpoint(self, endpoint: str) -> ValidationResult:
        """Validate that an endpoint exists in schemas.

        Args:
            endpoint: Endpoint path to validate

        Returns:
            ValidationResult
        """
        result = ValidationResult(is_valid=True)

        if endpoint not in self.endpoints:
            result.add_error(f"Endpoint '{endpoint}' not found in any schema")
            result.add_suggestion(f"Add '{endpoint}' to API schema before implementing")

        return result

    def validate_entity(self, entity: str) -> ValidationResult:
        """Validate that an entity exists in schemas.

        Args:
            entity: Entity name to validate

        Returns:
            ValidationResult
        """
        result = ValidationResult(is_valid=True)

        if entity not in self.entities:
            result.add_error(f"Entity '{entity}' not found in any schema")
            result.add_suggestion(f"Define '{entity}' in schema before using")

        return result

    def validate_operation(
        self,
        operation_type: str,
        targets: List[str],
    ) -> ValidationResult:
        """Validate a planned operation.

        Args:
            operation_type: Type of operation (endpoint, entity, etc.)
            targets: List of target names

        Returns:
            ValidationResult
        """
        result = ValidationResult(is_valid=True)

        for target in targets:
            if operation_type == 'endpoint':
                sub_result = self.validate_endpoint(target)
            elif operation_type == 'entity':
                sub_result = self.validate_entity(target)
            else:
                result.add_warning(f"Unknown operation type: {operation_type}")
                continue

            result.errors.extend(sub_result.errors)
            result.warnings.extend(sub_result.warnings)
            result.suggestions.extend(sub_result.suggestions)

            if not sub_result.is_valid:
                result.is_valid = False

        return result


class SchemaGate:
    """Gate that prevents operations without schema approval."""

    def __init__(self, repo_root: str, strict_mode: bool = True):
        """Initialize schema gate.

        Args:
            repo_root: Root directory of repository
            strict_mode: If True, block operations on validation failure
        """
        self.repo_root = repo_root
        self.strict_mode = strict_mode
        self.extractor = SchemaExtractor(repo_root)
        self.validator = None
        self._initialized = False

    def initialize(self):
        """Initialize gate by extracting schemas."""
        if self._initialized:
            return

        logger.info("Initializing schema gate", repo_root=self.repo_root)
        schemas = self.extractor.extract_all()
        self.validator = SchemaValidator(schemas)
        self._initialized = True

        logger.info(
            "Schema gate initialized",
            schemas=len(schemas),
            endpoints=len(self.validator.endpoints),
            entities=len(self.validator.entities),
        )

    def check_operation(
        self,
        operation_type: str,
        targets: List[str],
        override: bool = False,
    ) -> ValidationResult:
        """Check if operation is allowed.

        Args:
            operation_type: Type of operation
            targets: Target names
            override: Override strict mode for this check

        Returns:
            ValidationResult

        Raises:
            RuntimeError: If operation blocked in strict mode
        """
        if not self._initialized:
            self.initialize()

        result = self.validator.validate_operation(operation_type, targets)

        if not result.is_valid and self.strict_mode and not override:
            error_msg = f"Operation blocked by schema gate: {'; '.join(result.errors)}"
            logger.error("Schema gate blocked operation", errors=result.errors)
            raise RuntimeError(error_msg)

        if result.warnings:
            logger.warning("Schema gate warnings", warnings=result.warnings)

        return result

    def require_skeleton(self, operation_description: str) -> Dict[str, Any]:
        """Require skeleton/plan before code generation.

        Args:
            operation_description: Description of planned operation

        Returns:
            Dictionary with skeleton requirements
        """
        return {
            'operation': operation_description,
            'required_schemas': list(self.validator.entities),
            'required_endpoints': list(self.validator.endpoints),
            'instructions': [
                '1. Define all new entities in schema files first',
                '2. Add new endpoints to API specification',
                '3. Get schema approval before implementation',
                '4. Generate skeleton/interfaces from schemas',
                '5. Implement business logic',
            ],
        }
