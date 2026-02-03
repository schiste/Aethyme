"""Tests for schema-first planning and validation."""

import pytest
import tempfile
from pathlib import Path

from src.guardrails.schema_first import (
    SchemaExtractor,
    SchemaValidator,
    SchemaGate,
    SchemaType,
    ExtractedSchema,
    ValidationResult,
)


class TestSchemaExtractor:
    """Tests for schema extraction."""

    def test_extract_pydantic_models(self, tmp_path):
        """Test extracting Pydantic models from Python files."""
        # Create test file with Pydantic model
        test_file = tmp_path / "models.py"
        test_file.write_text("""
from pydantic import BaseModel

class User(BaseModel):
    id: int
    name: str

class Product(BaseModel):
    id: int
    price: float
""")

        extractor = SchemaExtractor(str(tmp_path))
        schemas = extractor._extract_pydantic()

        assert len(schemas) == 1
        assert schemas[0].schema_type == SchemaType.PYTHON_PYDANTIC
        assert 'User' in schemas[0].entities
        assert 'Product' in schemas[0].entities

    def test_extract_typescript_interfaces(self, tmp_path):
        """Test extracting TypeScript interfaces."""
        test_file = tmp_path / "types.ts"
        test_file.write_text("""
export interface User {
    id: number;
    name: string;
}

export type Product = {
    id: number;
    price: number;
}
""")

        extractor = SchemaExtractor(str(tmp_path))
        schemas = extractor._extract_typescript()

        assert len(schemas) == 1
        assert 'User' in schemas[0].entities
        assert 'Product' in schemas[0].entities

    def test_extract_graphql_schema(self, tmp_path):
        """Test extracting GraphQL schemas."""
        test_file = tmp_path / "schema.graphql"
        test_file.write_text("""
type User {
    id: ID!
    name: String!
}

type Product {
    id: ID!
    price: Float!
}

query getUser(id: ID!): User
""")

        extractor = SchemaExtractor(str(tmp_path))
        schemas = extractor._extract_graphql()

        assert len(schemas) == 1
        assert 'User' in schemas[0].entities
        assert 'Product' in schemas[0].entities
        assert 'getUser' in schemas[0].operations

    def test_get_routes(self, tmp_path):
        """Test getting all routes from schemas."""
        openapi_file = tmp_path / "openapi.yaml"
        openapi_file.write_text("""
paths:
  /users:
    get:
      summary: List users
  /products:
    post:
      summary: Create product
""")

        extractor = SchemaExtractor(str(tmp_path))
        extractor.extract_all()
        routes = extractor.get_routes()

        assert '/users' in routes
        assert '/products' in routes


class TestSchemaValidator:
    """Tests for schema validation."""

    def test_validate_existing_endpoint(self):
        """Test validation of existing endpoint."""
        schema = ExtractedSchema(
            schema_type=SchemaType.OPENAPI,
            file_path="openapi.yaml",
            content="",
            endpoints=['/users', '/products'],
        )

        validator = SchemaValidator([schema])
        result = validator.validate_endpoint('/users')

        assert result.is_valid
        assert len(result.errors) == 0

    def test_validate_missing_endpoint(self):
        """Test validation of missing endpoint."""
        schema = ExtractedSchema(
            schema_type=SchemaType.OPENAPI,
            file_path="openapi.yaml",
            content="",
            endpoints=['/users'],
        )

        validator = SchemaValidator([schema])
        result = validator.validate_endpoint('/products')

        assert not result.is_valid
        assert len(result.errors) > 0
        assert '/products' in result.errors[0]

    def test_validate_existing_entity(self):
        """Test validation of existing entity."""
        schema = ExtractedSchema(
            schema_type=SchemaType.PYTHON_PYDANTIC,
            file_path="models.py",
            content="",
            entities=['User', 'Product'],
        )

        validator = SchemaValidator([schema])
        result = validator.validate_entity('User')

        assert result.is_valid
        assert len(result.errors) == 0

    def test_validate_missing_entity(self):
        """Test validation of missing entity."""
        schema = ExtractedSchema(
            schema_type=SchemaType.PYTHON_PYDANTIC,
            file_path="models.py",
            content="",
            entities=['User'],
        )

        validator = SchemaValidator([schema])
        result = validator.validate_entity('Product')

        assert not result.is_valid
        assert len(result.errors) > 0
        assert 'Product' in result.errors[0]

    def test_validate_operation(self):
        """Test validating planned operations."""
        schema = ExtractedSchema(
            schema_type=SchemaType.OPENAPI,
            file_path="openapi.yaml",
            content="",
            endpoints=['/users', '/products'],
            entities=['User', 'Product'],
        )

        validator = SchemaValidator([schema])

        # Valid operation
        result = validator.validate_operation('endpoint', ['/users'])
        assert result.is_valid

        # Invalid operation
        result = validator.validate_operation('endpoint', ['/orders'])
        assert not result.is_valid


class TestSchemaGate:
    """Tests for schema gate."""

    def test_initialize_gate(self, tmp_path):
        """Test initializing schema gate."""
        gate = SchemaGate(str(tmp_path), strict_mode=True)
        gate.initialize()

        assert gate._initialized
        assert gate.validator is not None

    def test_check_operation_strict_mode(self, tmp_path):
        """Test operation check in strict mode."""
        # Create a schema file
        schema_file = tmp_path / "models.py"
        schema_file.write_text("""
from pydantic import BaseModel

class User(BaseModel):
    id: int
""")

        gate = SchemaGate(str(tmp_path), strict_mode=True)
        gate.initialize()

        # Should pass for existing entity
        result = gate.check_operation('entity', ['User'])
        assert result.is_valid

        # Should fail for missing entity in strict mode
        with pytest.raises(RuntimeError):
            gate.check_operation('entity', ['Product'])

    def test_check_operation_non_strict(self, tmp_path):
        """Test operation check in non-strict mode."""
        gate = SchemaGate(str(tmp_path), strict_mode=False)
        gate.initialize()

        # Should not raise, just return invalid result
        result = gate.check_operation('entity', ['Product'])
        assert not result.is_valid

    def test_require_skeleton(self, tmp_path):
        """Test requiring skeleton for operation."""
        schema_file = tmp_path / "models.py"
        schema_file.write_text("""
from pydantic import BaseModel

class User(BaseModel):
    id: int
""")

        gate = SchemaGate(str(tmp_path))
        gate.initialize()

        skeleton = gate.require_skeleton("Create new API endpoint")

        assert 'operation' in skeleton
        assert 'required_schemas' in skeleton
        assert 'instructions' in skeleton
        assert len(skeleton['instructions']) > 0

    def test_override_strict_mode(self, tmp_path):
        """Test overriding strict mode for specific check."""
        gate = SchemaGate(str(tmp_path), strict_mode=True)
        gate.initialize()

        # Should not raise with override
        result = gate.check_operation('entity', ['NonExistent'], override=True)
        assert not result.is_valid
