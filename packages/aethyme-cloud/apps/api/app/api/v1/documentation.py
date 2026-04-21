"""Documentation Generation API Endpoints."""

from typing import Optional, Literal
from fastapi import APIRouter, Depends, Query
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.database import get_db
from app.core.security import get_current_user
from app.models.user import User
from app.services.documentation_generator import APIDocumentationGenerator
from app.services.architecture_diagram_generator import ArchitectureDiagramGenerator
from app.services.code_metrics import CodeMetricsCalculator
from app.services.readme_generator import READMEGenerator

router = APIRouter()


@router.post("/generate/api-docs/{repository_id}")
async def generate_api_documentation(
    repository_id: str,
    language: str = Query(..., description="Programming language"),
    format: Literal["json", "markdown"] = Query(default="markdown"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Generate API documentation for a repository.

    Args:
        repository_id: Repository ID
        language: Programming language
        format: Output format (json or markdown)
        current_user: Authenticated user
        db: Database session

    Returns:
        Generated API documentation
    """
    # TODO: Verify user has access to repository
    # TODO: Fetch symbols from SCIP index

    generator = APIDocumentationGenerator(repository_id)

    # For now, return placeholder
    # In full implementation, would fetch symbols and generate docs
    docs = {
        "repository_id": repository_id,
        "language": language,
        "files": [],
        "statistics": {
            "total_files": 0,
            "total_functions": 0,
            "total_classes": 0,
        },
    }

    if format == "markdown":
        markdown = generator.generate_markdown(docs)
        return {"format": "markdown", "content": markdown}
    else:
        return docs


@router.get("/diagrams/component/{repository_id}")
async def generate_component_diagram(
    repository_id: str,
    format: Literal["mermaid", "plantuml", "dot"] = Query(default="mermaid"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Generate component architecture diagram.

    Args:
        repository_id: Repository ID
        format: Diagram format
        current_user: Authenticated user
        db: Database session

    Returns:
        Component diagram
    """
    generator = ArchitectureDiagramGenerator(db, repository_id)
    diagram = await generator.generate_component_diagram(format=format)
    return diagram


@router.get("/diagrams/dependencies/{repository_id}")
async def generate_dependency_diagram(
    repository_id: str,
    format: Literal["mermaid", "plantuml", "dot"] = Query(default="mermaid"),
    max_depth: int = Query(default=3, ge=1, le=10),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Generate dependency graph diagram.

    Args:
        repository_id: Repository ID
        format: Diagram format
        max_depth: Maximum traversal depth
        current_user: Authenticated user
        db: Database session

    Returns:
        Dependency diagram
    """
    generator = ArchitectureDiagramGenerator(db, repository_id)
    diagram = await generator.generate_dependency_graph(format=format, max_depth=max_depth)
    return diagram


@router.get("/diagrams/call-flow/{symbol_id}")
async def generate_call_flow_diagram(
    symbol_id: str,
    format: Literal["mermaid", "plantuml"] = Query(default="mermaid"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Generate call flow sequence diagram from a symbol.

    Args:
        symbol_id: Symbol ID to start from
        format: Diagram format
        current_user: Authenticated user
        db: Database session

    Returns:
        Call flow diagram
    """
    # Extract repository_id from symbol_id (format: repo:file:kind:name)
    repository_id = symbol_id.split(":")[0]

    generator = ArchitectureDiagramGenerator(db, repository_id)
    diagram = await generator.generate_call_flow_diagram(symbol_id, format=format)
    return diagram


@router.get("/metrics/{repository_id}")
async def get_code_metrics(
    repository_id: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Get comprehensive code metrics for a repository.

    Args:
        repository_id: Repository ID
        current_user: Authenticated user
        db: Database session

    Returns:
        Complete metrics report
    """
    calculator = CodeMetricsCalculator(db, repository_id)
    metrics = await calculator.calculate_all_metrics()
    return metrics


@router.get("/metrics/hotspots/{repository_id}")
async def get_code_hotspots(
    repository_id: str,
    limit: int = Query(default=10, ge=1, le=50),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Get code hotspots (high complexity + high change frequency).

    Args:
        repository_id: Repository ID
        limit: Maximum number of hotspots
        current_user: Authenticated user
        db: Database session

    Returns:
        List of code hotspots
    """
    calculator = CodeMetricsCalculator(db, repository_id)
    hotspots = await calculator.get_hotspots(limit=limit)
    return {"repository_id": repository_id, "hotspots": hotspots}


@router.post("/generate/readme/{repository_id}")
async def generate_readme(
    repository_id: str,
    repository_name: str = Query(..., description="Repository name (owner/repo)"),
    description: Optional[str] = Query(None, description="Project description"),
    language: str = Query(default="unknown"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Generate README.md for a repository.

    Args:
        repository_id: Repository ID
        repository_name: Repository name
        description: Project description
        language: Primary language
        current_user: Authenticated user
        db: AsyncSession: Database session

    Returns:
        Generated README content
    """
    # Get metrics for README
    metrics_calculator = CodeMetricsCalculator(db, repository_id)
    metrics = await metrics_calculator.calculate_all_metrics()

    # Generate README
    generator = READMEGenerator(repository_id, repository_name)
    readme_content = generator.generate(
        description=description, language=language, metrics=metrics
    )

    return {"repository_id": repository_id, "content": readme_content, "format": "markdown"}
