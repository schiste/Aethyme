"""Populate sample graph data for testing.

This script creates sample code relationships to test the graph visualization features.
"""

import asyncio
import sys
from pathlib import Path

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))

from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession
from sqlalchemy.orm import sessionmaker
from app.models.code_graph import CodeRelationship, SymbolMetadata, RelationshipType
from app.core.config import settings


async def create_sample_data():
    """Create sample graph data for a fictional Go repository."""

    # Create async engine (convert to async URL)
    db_url = str(settings.DATABASE_URL).replace("postgresql://", "postgresql+asyncpg://")
    engine = create_async_engine(db_url, echo=True)
    async_session = sessionmaker(engine, class_=AsyncSession, expire_on_commit=False)

    async with async_session() as session:
        repository_id = "sample-repo"
        file_path = "main.go"

        # Sample symbols and relationships for a simple web server
        relationships = [
            # main() calls HandleRequest, StartServer, InitDB
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:main",
                source_file_path=file_path,
                source_name="main",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:InitDB",
                target_file_path=file_path,
                target_name="InitDB",
                target_kind="function",
                relationship_type="calls",
                line_number=10,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:main",
                source_file_path=file_path,
                source_name="main",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:StartServer",
                target_file_path=file_path,
                target_name="StartServer",
                target_kind="function",
                relationship_type="calls",
                line_number=11,
                column_number=1,
                language="go",
            ),
            # StartServer calls HandleRequest, LogRequest
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:StartServer",
                source_file_path=file_path,
                source_name="StartServer",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                target_file_path=file_path,
                target_name="HandleRequest",
                target_kind="function",
                relationship_type="calls",
                line_number=20,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:StartServer",
                source_file_path=file_path,
                source_name="StartServer",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:LogRequest",
                target_file_path=file_path,
                target_name="LogRequest",
                target_kind="function",
                relationship_type="calls",
                line_number=21,
                column_number=1,
                language="go",
            ),
            # HandleRequest calls ValidateInput, ProcessData, SendResponse
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                source_file_path=file_path,
                source_name="HandleRequest",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:ValidateInput",
                target_file_path=file_path,
                target_name="ValidateInput",
                target_kind="function",
                relationship_type="calls",
                line_number=30,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                source_file_path=file_path,
                source_name="HandleRequest",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:ProcessData",
                target_file_path=file_path,
                target_name="ProcessData",
                target_kind="function",
                relationship_type="calls",
                line_number=31,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                source_file_path=file_path,
                source_name="HandleRequest",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:SendResponse",
                target_file_path=file_path,
                target_name="SendResponse",
                target_kind="function",
                relationship_type="calls",
                line_number=32,
                column_number=1,
                language="go",
            ),
            # ProcessData calls QueryDB, TransformData
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:ProcessData",
                source_file_path=file_path,
                source_name="ProcessData",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:QueryDB",
                target_file_path=file_path,
                target_name="QueryDB",
                target_kind="function",
                relationship_type="calls",
                line_number=40,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:ProcessData",
                source_file_path=file_path,
                source_name="ProcessData",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:TransformData",
                target_file_path=file_path,
                target_name="TransformData",
                target_kind="function",
                relationship_type="calls",
                line_number=41,
                column_number=1,
                language="go",
            ),
            # Add some reverse relationships (called_by)
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:InitDB",
                source_file_path=file_path,
                source_name="InitDB",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:main",
                target_file_path=file_path,
                target_name="main",
                target_kind="function",
                relationship_type="called_by",
                line_number=10,
                column_number=1,
                language="go",
            ),
            CodeRelationship(
                repository_id=repository_id,
                source_symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                source_file_path=file_path,
                source_name="HandleRequest",
                source_kind="function",
                target_symbol_id=f"{repository_id}:{file_path}:function:StartServer",
                target_file_path=file_path,
                target_name="StartServer",
                target_kind="function",
                relationship_type="called_by",
                line_number=20,
                column_number=1,
                language="go",
            ),
        ]

        # Create symbol metadata
        metadata_entries = [
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:main",
                repository_id=repository_id,
                inbound_call_count=0,
                outbound_call_count=2,
                dependency_count=0,
                impact_score=5,
                cyclomatic_complexity=2,
            ),
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:StartServer",
                repository_id=repository_id,
                inbound_call_count=1,
                outbound_call_count=2,
                dependency_count=0,
                impact_score=15,
                cyclomatic_complexity=3,
            ),
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:HandleRequest",
                repository_id=repository_id,
                inbound_call_count=1,
                outbound_call_count=3,
                dependency_count=0,
                impact_score=25,
                cyclomatic_complexity=5,
            ),
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:ProcessData",
                repository_id=repository_id,
                inbound_call_count=1,
                outbound_call_count=2,
                dependency_count=1,
                impact_score=20,
                cyclomatic_complexity=4,
            ),
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:ValidateInput",
                repository_id=repository_id,
                inbound_call_count=1,
                outbound_call_count=0,
                dependency_count=0,
                impact_score=10,
                cyclomatic_complexity=2,
            ),
            SymbolMetadata(
                symbol_id=f"{repository_id}:{file_path}:function:QueryDB",
                repository_id=repository_id,
                inbound_call_count=1,
                outbound_call_count=0,
                dependency_count=1,
                impact_score=15,
                cyclomatic_complexity=3,
            ),
        ]

        # Add all relationships
        for rel in relationships:
            session.add(rel)

        # Add all metadata
        for meta in metadata_entries:
            session.add(meta)

        await session.commit()

        print(f"✅ Created {len(relationships)} sample relationships")
        print(f"✅ Created {len(metadata_entries)} symbol metadata entries")
        print("\nSample function symbol IDs for testing:")
        print(f"  - main: {repository_id}:{file_path}:function:main")
        print(f"  - HandleRequest: {repository_id}:{file_path}:function:HandleRequest")
        print(f"  - ProcessData: {repository_id}:{file_path}:function:ProcessData")

    await engine.dispose()


if __name__ == "__main__":
    print("Populating sample graph data...")
    asyncio.run(create_sample_data())
    print("\n✅ Done! You can now test the graph visualization.")
    print("\nExample URL:")
    print(f"  http://localhost:3000/graph/sample-repo:main.go:function:HandleRequest")
