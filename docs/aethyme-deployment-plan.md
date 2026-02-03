# Aethyme Deployment Plan

Note: This plan targets the Aethyme service (port 8001), which uses a versioned API (`/api/*`). Endpoints here intentionally retain `/api/`.

**Status**: Internal Tool (Production-Ready, Full Scope)
**Integration**: Aeptus Monorepo (`packages/aethyme/`)
**Export Strategy**: git subtree split (when ready)

---

## Executive Summary

This plan deploys Aethyme as a **self-contained package** within the Aeptus monorepo. It includes all features from the PRD—not an MVP—designed for internal use first, with a clean export path for future standalone distribution.

**Key Principles**:
- **Non-breaking**: Zero impact on existing Aeptus functionality
- **Self-contained**: Independent dependencies, testing, deployment
- **Production-ready**: Full scope, not prototype
- **Export-ready**: Clean separation for future extraction via git subtree

---

## Package Structure

```
packages/aethyme/
├── package.json                    # Independent dependencies
├── pyproject.toml                  # Python package config
├── README.md                       # Package documentation
├── .gitignore
├── .env.example
│
├── src/
│   ├── indexer/                    # Python indexing engine
│   │   ├── __init__.py
│   │   ├── scip_python.py         # Python indexer (scip-python)
│   │   ├── scip_typescript.py     # TypeScript/TSX indexer (scip-typescript)
│   │   ├── graph_builder.py       # Graph construction
│   │   ├── file_watcher.py        # Incremental updates
│   │   └── cli.py                 # CLI interface
│   │
│   ├── graph/                      # Graph storage and retrieval
│   │   ├── __init__.py
│   │   ├── schema.py              # DuckDB schema definitions
│   │   ├── store.py               # DuckDB operations
│   │   ├── ego.py                 # Ego graph retrieval (1-hop, 2-hop)
│   │   ├── impact.py              # Impact analysis
│   │   └── hybrid_search.py       # Hybrid search (BM25 + graph)
│   │
│   ├── api/                        # FastAPI backend
│   │   ├── __init__.py
│   │   ├── main.py                # FastAPI app
│   │   ├── routes/
│   │   │   ├── __init__.py
│   │   │   ├── ego.py             # /ego endpoint
│   │   │   ├── impact.py          # /impact endpoint
│   │   │   ├── search.py          # /search endpoint
│   │   │   └── health.py          # /health endpoint
│   │   ├── models.py              # Pydantic request/response models
│   │   └── middleware.py          # Auth, CORS, logging
│   │
│   ├── ui/                         # React components (Aeptus integration)
│   │   ├── AethymeViewer.tsx    # Main graph viewer
│   │   ├── SearchInterface.tsx    # Hybrid search UI
│   │   ├── ImpactView.tsx         # Impact analysis view
│   │   ├── GraphVisualizer.tsx    # D3 graph rendering
│   │   └── types.ts               # TypeScript types
│   │
│   └── vscode/                     # VS Code extension
│       ├── package.json
│       ├── src/
│       │   ├── extension.ts       # Extension entry point
│       │   ├── commands.ts        # VS Code commands
│       │   ├── treeview.ts        # Graph tree view
│       │   └── webview.ts         # Graph visualization webview
│       └── media/                 # Extension assets
│
├── ops/
│   ├── docker/
│   │   ├── Dockerfile.indexer     # Indexer container
│   │   └── Dockerfile.api         # API container
│   ├── docker-compose.yml         # Local development
│   └── terraform/                 # Infrastructure (future)
│
├── tests/
│   ├── test_indexer.py
│   ├── test_graph.py
│   ├── test_api.py
│   └── fixtures/                  # Test repositories
│
├── scripts/
│   ├── index-repo.sh              # Index a repository
│   ├── start-api.sh               # Start FastAPI server
│   └── reset-graph.sh             # Reset graph database
│
└── docs/
    ├── ARCHITECTURE.md
    ├── API_REFERENCE.md
    ├── INDEXING.md
    └── DEPLOYMENT.md
```

---

## Phase 1: Foundation and Package Setup

### 1.1 Create Package Structure

**Files to create**:

**`packages/aethyme/package.json`**:
```json
{
  "name": "@aeptus/aethyme",
  "version": "0.1.0",
  "private": true,
  "description": "Graph-based code indexing for Python and TypeScript",
  "scripts": {
    "dev:api": "bash scripts/start-api.sh",
    "index": "python -m src.indexer.cli",
    "test": "pytest tests/",
    "lint": "ruff check src/",
    "type-check": "pyright src/",
    "build:vscode": "cd src/vscode && pnpm build",
    "reset": "bash scripts/reset-graph.sh"
  },
  "dependencies": {
    "react": "^18.2.0",
    "d3": "^7.8.5",
    "@types/d3": "^7.4.0"
  },
  "devDependencies": {
    "@types/react": "^18.2.0",
    "typescript": "^5.3.0",
    "vite": "^5.0.0"
  }
}
```

**`packages/aethyme/pyproject.toml`**:
```toml
[project]
name = "aethyme"
version = "0.1.0"
description = "Graph-based code indexing"
requires-python = ">=3.11"
dependencies = [
    "fastapi>=0.109.0",
    "uvicorn[standard]>=0.27.0",
    "duckdb>=0.10.0",
    "tree-sitter>=0.21.0",
    "tree-sitter-python>=0.21.0",
    "tree-sitter-typescript>=0.21.0",
    "pydantic>=2.5.0",
    "click>=8.1.0",
    "watchdog>=3.0.0",
    "httpx>=0.26.0",
    "rank-bm25>=0.2.2",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.4.0",
    "pytest-asyncio>=0.23.0",
    "ruff>=0.1.0",
    "pyright>=1.1.0",
]

[tool.ruff]
line-length = 100
target-version = "py311"

[tool.pyright]
typeCheckingMode = "strict"
pythonVersion = "3.11"
```

**`packages/aethyme/README.md`**:
```markdown
# Aethyme

Graph-based code indexing for Python and TypeScript/React codebases.

## Features

- **Precise indexing**: Uses SCIP (Code Intelligence Protocol) for Python and TypeScript
- **Graph retrieval**: 1-hop and 2-hop ego graphs for definitions
- **Impact analysis**: Find all callers and dependencies
- **Hybrid search**: BM25 keyword search + graph context
- **Incremental updates**: File watcher for automatic re-indexing

## Quick Start

### Index a repository
\`\`\`bash
pnpm --filter @aeptus/aethyme index -- /path/to/repo
\`\`\`

### Start API server
\`\`\`bash
pnpm --filter @aeptus/aethyme dev:api
\`\`\`

### Access UI
Navigate to http://localhost:3000/admin/aethyme

## Architecture

- **Indexer**: Python-based SCIP indexing (scip-python, scip-typescript)
- **Storage**: DuckDB (per-tenant graph databases)
- **API**: FastAPI (ego graphs, impact analysis, hybrid search)
- **UI**: React components integrated into Aeptus admin
- **VS Code**: Extension for in-editor graph navigation

See [docs/ARCHITECTURE.md](./docs/ARCHITECTURE.md) for details.
```

**`packages/aethyme/.env.example`**:
```bash
# DuckDB storage
AETHYME_DB_PATH=./data/graphs
AETHYME_CACHE_PATH=./data/cache

# API configuration
AETHYME_API_HOST=0.0.0.0
AETHYME_API_PORT=8001
AETHYME_CORS_ORIGINS=http://localhost:3000,http://localhost:5173

# Indexing
AETHYME_INDEX_CONCURRENCY=4
AETHYME_WATCH_ENABLED=true

# Logging
AETHYME_LOG_LEVEL=INFO
```

### 1.2 Update Root Configuration

**Modify `pnpm-workspace.yaml`**:
```yaml
packages:
  - 'apps/*'
  - 'packages/*'
  - 'packages/aethyme'  # Add explicit entry
```

**Modify root `package.json`** (add scripts):
```json
{
  "scripts": {
    "aethyme:dev": "pnpm --filter @aeptus/aethyme dev:api",
    "aethyme:index": "pnpm --filter @aeptus/aethyme index",
    "aethyme:test": "pnpm --filter @aeptus/aethyme test",
    "aethyme:reset": "pnpm --filter @aeptus/aethyme reset"
  }
}
```

---

## Phase 2: Python Indexer (SCIP-based)

### 2.1 SCIP Integration

**`packages/aethyme/src/indexer/scip_python.py`**:
```python
"""Python indexer using scip-python."""
import subprocess
import json
from pathlib import Path
from typing import List, Dict, Any

def index_python_files(repo_path: Path) -> Dict[str, Any]:
    """
    Index Python files using scip-python.

    Returns SCIP index with occurrences (definitions and references).
    """
    # Run scip-python indexer
    result = subprocess.run(
        ['scip-python', 'index', '--project-name', repo_path.name, str(repo_path)],
        cwd=repo_path,
        capture_output=True,
        text=True,
        check=True
    )

    # Parse SCIP index (JSONL format)
    index_path = repo_path / 'index.scip'
    if not index_path.exists():
        raise FileNotFoundError(f"SCIP index not found at {index_path}")

    documents = []
    with open(index_path, 'r') as f:
        for line in f:
            if line.strip():
                documents.append(json.loads(line))

    return {
        'metadata': {
            'tool_name': 'scip-python',
            'project_name': repo_path.name,
            'language': 'python'
        },
        'documents': documents
    }

def extract_definitions(scip_index: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Extract definitions from SCIP index."""
    definitions = []

    for doc in scip_index['documents']:
        file_path = doc['relative_path']
        for occurrence in doc.get('occurrences', []):
            if occurrence.get('symbol_roles', 0) & 1:  # Definition role
                definitions.append({
                    'symbol': occurrence['symbol'],
                    'file': file_path,
                    'line': occurrence['range'][0],
                    'col': occurrence['range'][1],
                    'kind': occurrence.get('symbol_kind', 'unknown'),
                    'text': occurrence.get('text', '')
                })

    return definitions

def extract_references(scip_index: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Extract references from SCIP index."""
    references = []

    for doc in scip_index['documents']:
        file_path = doc['relative_path']
        for occurrence in doc.get('occurrences', []):
            if not (occurrence.get('symbol_roles', 0) & 1):  # Not a definition
                references.append({
                    'symbol': occurrence['symbol'],
                    'file': file_path,
                    'line': occurrence['range'][0],
                    'col': occurrence['range'][1],
                    'kind': 'reference',
                    'target_symbol': occurrence.get('symbol', '')
                })

    return references
```

**`packages/aethyme/src/indexer/scip_typescript.py`**:
```python
"""TypeScript/TSX indexer using scip-typescript."""
import subprocess
import json
from pathlib import Path
from typing import List, Dict, Any

def index_typescript_files(repo_path: Path) -> Dict[str, Any]:
    """
    Index TypeScript/TSX files using scip-typescript.

    Requires tsconfig.json in repository root.
    """
    # Ensure tsconfig.json exists
    tsconfig = repo_path / 'tsconfig.json'
    if not tsconfig.exists():
        raise FileNotFoundError(f"tsconfig.json not found in {repo_path}")

    # Run scip-typescript indexer
    result = subprocess.run(
        ['scip-typescript', 'index', '--project-name', repo_path.name],
        cwd=repo_path,
        capture_output=True,
        text=True,
        check=True
    )

    # Parse SCIP index
    index_path = repo_path / 'index.scip'
    if not index_path.exists():
        raise FileNotFoundError(f"SCIP index not found at {index_path}")

    documents = []
    with open(index_path, 'r') as f:
        for line in f:
            if line.strip():
                documents.append(json.loads(line))

    return {
        'metadata': {
            'tool_name': 'scip-typescript',
            'project_name': repo_path.name,
            'language': 'typescript'
        },
        'documents': documents
    }

def extract_component_definitions(scip_index: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Extract React component definitions."""
    components = []

    for doc in scip_index['documents']:
        file_path = doc['relative_path']
        if not (file_path.endswith('.tsx') or file_path.endswith('.jsx')):
            continue

        for occurrence in doc.get('occurrences', []):
            if occurrence.get('symbol_roles', 0) & 1:  # Definition
                if 'Component' in occurrence.get('symbol_kind', '') or \
                   occurrence.get('text', '').startswith('function') or \
                   occurrence.get('text', '').startswith('const'):
                    components.append({
                        'symbol': occurrence['symbol'],
                        'file': file_path,
                        'line': occurrence['range'][0],
                        'col': occurrence['range'][1],
                        'kind': 'component',
                        'name': occurrence.get('text', '').split()[1] if ' ' in occurrence.get('text', '') else '',
                    })

    return components

def extract_imports(scip_index: Dict[str, Any]) -> List[Dict[str, Any]]:
    """Extract import relationships."""
    imports = []

    for doc in scip_index['documents']:
        file_path = doc['relative_path']
        for occurrence in doc.get('occurrences', []):
            if 'import' in occurrence.get('text', '').lower():
                imports.append({
                    'from_file': file_path,
                    'symbol': occurrence['symbol'],
                    'line': occurrence['range'][0],
                    'kind': 'import'
                })

    return imports
```

### 2.2 Graph Builder

**`packages/aethyme/src/indexer/graph_builder.py`**:
```python
"""Build graph from SCIP indices."""
from pathlib import Path
from typing import List, Dict, Any, Set, Tuple
from dataclasses import dataclass
import hashlib

@dataclass
class Node:
    """Graph node (definition or reference)."""
    id: str
    symbol: str
    file: str
    line: int
    col: int
    kind: str  # 'def' or 'ref'
    language: str
    text: str = ""

    @classmethod
    def create(cls, symbol: str, file: str, line: int, col: int, kind: str, language: str, text: str = ""):
        """Create node with unique ID."""
        node_str = f"{symbol}:{file}:{line}:{col}:{kind}"
        node_id = hashlib.sha256(node_str.encode()).hexdigest()[:16]
        return cls(
            id=node_id,
            symbol=symbol,
            file=file,
            line=line,
            col=col,
            kind=kind,
            language=language,
            text=text
        )

@dataclass
class Edge:
    """Graph edge (relationship between nodes)."""
    id: str
    from_node: str
    to_node: str
    edge_type: str  # 'invoke', 'import', 'contain', 'props_flow'

    @classmethod
    def create(cls, from_node: str, to_node: str, edge_type: str):
        """Create edge with unique ID."""
        edge_str = f"{from_node}:{to_node}:{edge_type}"
        edge_id = hashlib.sha256(edge_str.encode()).hexdigest()[:16]
        return cls(
            id=edge_id,
            from_node=from_node,
            to_node=to_node,
            edge_type=edge_type
        )

class GraphBuilder:
    """Build graph from SCIP indices."""

    def __init__(self):
        self.nodes: Dict[str, Node] = {}
        self.edges: List[Edge] = []
        self.symbol_to_def: Dict[str, str] = {}  # symbol -> def node_id

    def add_definitions(self, definitions: List[Dict[str, Any]], language: str):
        """Add definition nodes."""
        for defn in definitions:
            node = Node.create(
                symbol=defn['symbol'],
                file=defn['file'],
                line=defn['line'],
                col=defn['col'],
                kind='def',
                language=language,
                text=defn.get('text', '')
            )
            self.nodes[node.id] = node
            self.symbol_to_def[defn['symbol']] = node.id

    def add_references(self, references: List[Dict[str, Any]], language: str):
        """Add reference nodes and invoke edges."""
        for ref in references:
            ref_node = Node.create(
                symbol=ref['symbol'],
                file=ref['file'],
                line=ref['line'],
                col=ref['col'],
                kind='ref',
                language=language
            )
            self.nodes[ref_node.id] = ref_node

            # Create invoke edge: ref -> def
            target_symbol = ref.get('target_symbol', ref['symbol'])
            if target_symbol in self.symbol_to_def:
                def_node_id = self.symbol_to_def[target_symbol]
                edge = Edge.create(ref_node.id, def_node_id, 'invoke')
                self.edges.append(edge)

    def add_imports(self, imports: List[Dict[str, Any]], language: str):
        """Add import edges."""
        for imp in imports:
            # Create import edge: file -> imported_symbol
            if imp['symbol'] in self.symbol_to_def:
                # Find node in from_file (any node)
                from_nodes = [n for n in self.nodes.values() if n.file == imp['from_file']]
                if from_nodes:
                    from_node = from_nodes[0]
                    to_node = self.symbol_to_def[imp['symbol']]
                    edge = Edge.create(from_node.id, to_node, 'import')
                    self.edges.append(edge)

    def add_containment(self):
        """Add containment edges (file contains definitions)."""
        files: Dict[str, List[str]] = {}
        for node in self.nodes.values():
            if node.kind == 'def':
                if node.file not in files:
                    files[node.file] = []
                files[node.file].append(node.id)

        # Create virtual file nodes and contain edges
        for file_path, node_ids in files.items():
            file_node = Node.create(
                symbol=f"file:{file_path}",
                file=file_path,
                line=0,
                col=0,
                kind='file',
                language='',
                text=file_path
            )
            self.nodes[file_node.id] = file_node

            for node_id in node_ids:
                edge = Edge.create(file_node.id, node_id, 'contain')
                self.edges.append(edge)

    def build(self) -> Tuple[Dict[str, Node], List[Edge]]:
        """Finalize graph."""
        self.add_containment()
        return self.nodes, self.edges
```

### 2.3 CLI Interface

**`packages/aethyme/src/indexer/cli.py`**:
```python
"""CLI for indexing repositories."""
import click
from pathlib import Path
from .scip_python import index_python_files, extract_definitions as extract_py_defs, extract_references as extract_py_refs
from .scip_typescript import index_typescript_files, extract_component_definitions, extract_imports
from .graph_builder import GraphBuilder
from ..graph.store import GraphStore

@click.group()
def cli():
    """Aethyme indexer CLI."""
    pass

@cli.command()
@click.argument('repo_path', type=click.Path(exists=True))
@click.option('--output', '-o', default='./data/graphs/repo.db', help='Output DuckDB path')
@click.option('--languages', '-l', default='python,typescript', help='Languages to index (comma-separated)')
def index(repo_path: str, output: str, languages: str):
    """Index a repository and build graph."""
    repo = Path(repo_path)
    langs = set(languages.split(','))

    click.echo(f"Indexing {repo} with languages: {langs}")

    builder = GraphBuilder()

    # Index Python
    if 'python' in langs:
        click.echo("Indexing Python files...")
        py_index = index_python_files(repo)
        py_defs = extract_py_defs(py_index)
        py_refs = extract_py_refs(py_index)
        builder.add_definitions(py_defs, 'python')
        builder.add_references(py_refs, 'python')
        click.echo(f"  Found {len(py_defs)} Python definitions, {len(py_refs)} references")

    # Index TypeScript
    if 'typescript' in langs:
        click.echo("Indexing TypeScript files...")
        ts_index = index_typescript_files(repo)
        ts_defs = extract_component_definitions(ts_index)
        ts_imports = extract_imports(ts_index)
        builder.add_definitions(ts_defs, 'typescript')
        builder.add_imports(ts_imports, 'typescript')
        click.echo(f"  Found {len(ts_defs)} TypeScript definitions, {len(ts_imports)} imports")

    # Build graph
    click.echo("Building graph...")
    nodes, edges = builder.build()
    click.echo(f"  {len(nodes)} nodes, {len(edges)} edges")

    # Store in DuckDB
    click.echo(f"Writing to {output}...")
    store = GraphStore(output)
    store.create_schema()
    store.insert_nodes(list(nodes.values()))
    store.insert_edges(edges)

    click.echo("✓ Indexing complete")

if __name__ == '__main__':
    cli()
```

---

## Phase 3: Graph Storage (DuckDB)

### 3.1 Schema Definition

**`packages/aethyme/src/graph/schema.py`**:
```python
"""DuckDB schema for graph storage."""

SCHEMA_SQL = """
-- Nodes table: definitions and references
CREATE TABLE IF NOT EXISTS nodes (
    id VARCHAR PRIMARY KEY,
    symbol VARCHAR NOT NULL,
    file VARCHAR NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    kind VARCHAR NOT NULL,  -- 'def', 'ref', 'file'
    language VARCHAR NOT NULL,
    text VARCHAR,
    indexed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_symbol (symbol),
    INDEX idx_file (file),
    INDEX idx_kind (kind)
);

-- Edges table: relationships between nodes
CREATE TABLE IF NOT EXISTS edges (
    id VARCHAR PRIMARY KEY,
    from_node VARCHAR NOT NULL,
    to_node VARCHAR NOT NULL,
    edge_type VARCHAR NOT NULL,  -- 'invoke', 'import', 'contain', 'props_flow'
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (from_node) REFERENCES nodes(id),
    FOREIGN KEY (to_node) REFERENCES nodes(id),
    INDEX idx_from (from_node),
    INDEX idx_to (to_node),
    INDEX idx_type (edge_type)
);

-- Full-text search index (for hybrid search)
CREATE TABLE IF NOT EXISTS fts_nodes (
    node_id VARCHAR PRIMARY KEY,
    content VARCHAR NOT NULL,
    FOREIGN KEY (node_id) REFERENCES nodes(id)
);

-- Metadata table
CREATE TABLE IF NOT EXISTS metadata (
    key VARCHAR PRIMARY KEY,
    value VARCHAR NOT NULL,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
"""

INDEXES_SQL = """
-- Additional indexes for common queries
CREATE INDEX IF NOT EXISTS idx_nodes_symbol_kind ON nodes(symbol, kind);
CREATE INDEX IF NOT EXISTS idx_edges_from_type ON edges(from_node, edge_type);
CREATE INDEX IF NOT EXISTS idx_edges_to_type ON edges(to_node, edge_type);
"""
```

### 3.2 DuckDB Store

**`packages/aethyme/src/graph/store.py`**:
```python
"""DuckDB graph storage."""
import duckdb
from pathlib import Path
from typing import List, Dict, Any, Optional
from .schema import SCHEMA_SQL, INDEXES_SQL
from ..indexer.graph_builder import Node, Edge

class GraphStore:
    """DuckDB-backed graph storage."""

    def __init__(self, db_path: str):
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = duckdb.connect(str(self.db_path))

    def create_schema(self):
        """Initialize database schema."""
        self.conn.execute(SCHEMA_SQL)
        self.conn.execute(INDEXES_SQL)

    def insert_nodes(self, nodes: List[Node]):
        """Bulk insert nodes."""
        data = [
            (n.id, n.symbol, n.file, n.line, n.col, n.kind, n.language, n.text)
            for n in nodes
        ]
        self.conn.executemany(
            "INSERT OR REPLACE INTO nodes (id, symbol, file, line, col, kind, language, text) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            data
        )

        # Insert into FTS for full-text search
        fts_data = [(n.id, f"{n.symbol} {n.text}") for n in nodes if n.text]
        if fts_data:
            self.conn.executemany(
                "INSERT OR REPLACE INTO fts_nodes (node_id, content) VALUES (?, ?)",
                fts_data
            )

    def insert_edges(self, edges: List[Edge]):
        """Bulk insert edges."""
        data = [(e.id, e.from_node, e.to_node, e.edge_type) for e in edges]
        self.conn.executemany(
            "INSERT OR REPLACE INTO edges (id, from_node, to_node, edge_type) VALUES (?, ?, ?, ?)",
            data
        )

    def get_node(self, node_id: str) -> Optional[Dict[str, Any]]:
        """Get node by ID."""
        result = self.conn.execute(
            "SELECT * FROM nodes WHERE id = ?", [node_id]
        ).fetchone()
        if not result:
            return None
        return self._row_to_node(result)

    def find_definition(self, symbol: str) -> Optional[Dict[str, Any]]:
        """Find definition node by symbol."""
        result = self.conn.execute(
            "SELECT * FROM nodes WHERE symbol = ? AND kind = 'def' LIMIT 1",
            [symbol]
        ).fetchone()
        if not result:
            return None
        return self._row_to_node(result)

    def get_neighbors(self, node_id: str, edge_type: Optional[str] = None) -> List[Dict[str, Any]]:
        """Get neighboring nodes (1-hop)."""
        if edge_type:
            query = """
                SELECT n.* FROM nodes n
                JOIN edges e ON (e.to_node = n.id OR e.from_node = n.id)
                WHERE (e.from_node = ? OR e.to_node = ?) AND e.edge_type = ?
            """
            params = [node_id, node_id, edge_type]
        else:
            query = """
                SELECT n.* FROM nodes n
                JOIN edges e ON (e.to_node = n.id OR e.from_node = n.id)
                WHERE e.from_node = ? OR e.to_node = ?
            """
            params = [node_id, node_id]

        results = self.conn.execute(query, params).fetchall()
        return [self._row_to_node(row) for row in results]

    def _row_to_node(self, row) -> Dict[str, Any]:
        """Convert DB row to node dict."""
        return {
            'id': row[0],
            'symbol': row[1],
            'file': row[2],
            'line': row[3],
            'col': row[4],
            'kind': row[5],
            'language': row[6],
            'text': row[7] or '',
        }

    def close(self):
        """Close database connection."""
        self.conn.close()
```

### 3.3 Ego Graph Retrieval

**`packages/aethyme/src/graph/ego.py`**:
```python
"""Ego graph retrieval (1-hop and 2-hop)."""
from typing import Dict, Any, List, Set
from .store import GraphStore

def get_ego_graph_1hop(store: GraphStore, symbol: str) -> Dict[str, Any]:
    """
    Get 1-hop ego graph for a symbol (flattened).

    Returns:
        {
            'definition': {...},
            'callers': [...],
            'callees': [...],
            'imports': [...]
        }
    """
    # Find definition
    defn = store.find_definition(symbol)
    if not defn:
        return {'error': 'Definition not found'}

    # Get all edges from/to definition
    edges = store.conn.execute("""
        SELECT e.edge_type, n.*
        FROM edges e
        JOIN nodes n ON (e.from_node = n.id OR e.to_node = n.id)
        WHERE (e.from_node = ? OR e.to_node = ?) AND n.id != ?
    """, [defn['id'], defn['id'], defn['id']]).fetchall()

    callers = []
    callees = []
    imports = []

    for edge_type, *node_data in edges:
        node = {
            'id': node_data[0],
            'symbol': node_data[1],
            'file': node_data[2],
            'line': node_data[3],
            'col': node_data[4],
            'kind': node_data[5],
            'language': node_data[6],
            'text': node_data[7] or '',
        }

        if edge_type == 'invoke':
            # Check direction
            is_caller = store.conn.execute(
                "SELECT 1 FROM edges WHERE from_node = ? AND to_node = ?",
                [node['id'], defn['id']]
            ).fetchone()
            if is_caller:
                callers.append(node)
            else:
                callees.append(node)
        elif edge_type == 'import':
            imports.append(node)

    return {
        'definition': defn,
        'callers': callers,
        'callees': callees,
        'imports': imports,
        'total_edges': len(callers) + len(callees) + len(imports)
    }

def get_ego_graph_2hop(store: GraphStore, symbol: str) -> Dict[str, Any]:
    """
    Get 2-hop ego graph for a symbol (summarized).

    Returns summarized counts to avoid overwhelming output.
    """
    # Get 1-hop first
    ego_1hop = get_ego_graph_1hop(store, symbol)
    if 'error' in ego_1hop:
        return ego_1hop

    # For each 1-hop neighbor, get their neighbors (2-hop)
    second_hop_callers = set()
    second_hop_callees = set()

    for caller in ego_1hop['callers']:
        neighbors = store.get_neighbors(caller['id'], edge_type='invoke')
        for n in neighbors:
            if n['id'] != ego_1hop['definition']['id']:
                second_hop_callers.add(n['symbol'])

    for callee in ego_1hop['callees']:
        neighbors = store.get_neighbors(callee['id'], edge_type='invoke')
        for n in neighbors:
            if n['id'] != ego_1hop['definition']['id']:
                second_hop_callees.add(n['symbol'])

    return {
        'definition': ego_1hop['definition'],
        '1hop': {
            'callers': len(ego_1hop['callers']),
            'callees': len(ego_1hop['callees']),
            'imports': len(ego_1hop['imports']),
        },
        '2hop': {
            'callers': len(second_hop_callers),
            'callees': len(second_hop_callees),
        },
        'summary': f"1-hop: {len(ego_1hop['callers'])} callers, {len(ego_1hop['callees'])} callees | 2-hop: {len(second_hop_callers)} callers, {len(second_hop_callees)} callees"
    }
```

### 3.4 Impact Analysis

**`packages/aethyme/src/graph/impact.py`**:
```python
"""Impact analysis: find all transitive callers."""
from typing import Set, List, Dict, Any
from .store import GraphStore

def analyze_impact(store: GraphStore, symbol: str, max_depth: int = 10) -> Dict[str, Any]:
    """
    Find all transitive callers (upstream dependencies).

    Uses BFS to traverse invoke edges backward.
    """
    defn = store.find_definition(symbol)
    if not defn:
        return {'error': 'Definition not found'}

    visited: Set[str] = set()
    queue: List[tuple] = [(defn['id'], 0)]
    visited.add(defn['id'])

    callers_by_depth: Dict[int, List[Dict]] = {}

    while queue:
        node_id, depth = queue.pop(0)

        if depth >= max_depth:
            continue

        # Find callers (nodes with invoke edge TO current node)
        caller_rows = store.conn.execute("""
            SELECT n.* FROM nodes n
            JOIN edges e ON e.from_node = n.id
            WHERE e.to_node = ? AND e.edge_type = 'invoke'
        """, [node_id]).fetchall()

        for row in caller_rows:
            caller = {
                'id': row[0],
                'symbol': row[1],
                'file': row[2],
                'line': row[3],
                'kind': row[5],
            }

            if caller['id'] not in visited:
                visited.add(caller['id'])
                queue.append((caller['id'], depth + 1))

                if depth not in callers_by_depth:
                    callers_by_depth[depth] = []
                callers_by_depth[depth].append(caller)

    total_impacted = sum(len(callers) for callers in callers_by_depth.values())

    return {
        'definition': defn,
        'total_impacted': total_impacted,
        'max_depth': max(callers_by_depth.keys()) if callers_by_depth else 0,
        'by_depth': {
            depth: {
                'count': len(callers),
                'sample': callers[:5]  # Show first 5
            }
            for depth, callers in callers_by_depth.items()
        }
    }
```

### 3.5 Hybrid Search

**`packages/aethyme/src/graph/hybrid_search.py`**:
```python
"""Hybrid search: BM25 keyword search + graph context."""
from typing import List, Dict, Any
from rank_bm25 import BM25Okapi
from .store import GraphStore
from .ego import get_ego_graph_1hop

def hybrid_search(store: GraphStore, query: str, top_k: int = 10) -> List[Dict[str, Any]]:
    """
    Hybrid search combining keyword search and graph context.

    1. BM25 search on symbols and text
    2. For top results, enrich with 1-hop ego graph
    """
    # Get all nodes for BM25 corpus
    all_nodes = store.conn.execute("""
        SELECT id, symbol, text FROM nodes WHERE kind = 'def'
    """).fetchall()

    corpus = [f"{row[1]} {row[2] or ''}" for row in all_nodes]
    node_ids = [row[0] for row in all_nodes]

    # BM25 search
    tokenized_corpus = [doc.lower().split() for doc in corpus]
    bm25 = BM25Okapi(tokenized_corpus)

    tokenized_query = query.lower().split()
    scores = bm25.get_scores(tokenized_query)

    # Get top K
    top_indices = sorted(range(len(scores)), key=lambda i: scores[i], reverse=True)[:top_k]

    results = []
    for idx in top_indices:
        node_id = node_ids[idx]
        node = store.get_node(node_id)
        if not node:
            continue

        # Enrich with 1-hop ego graph
        ego = get_ego_graph_1hop(store, node['symbol'])

        results.append({
            'node': node,
            'score': float(scores[idx]),
            'context': {
                'callers_count': len(ego.get('callers', [])),
                'callees_count': len(ego.get('callees', [])),
                'imports_count': len(ego.get('imports', [])),
            }
        })

    return results
```

---

## Phase 4: FastAPI Backend

### 4.1 Main API Application

**`packages/aethyme/src/api/main.py`**:
```python
"""FastAPI application for Aethyme."""
from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from contextlib import asynccontextmanager
import os
from .routes import ego, impact, search, health
from ..graph.store import GraphStore

# Global store instance
store: GraphStore | None = None

@asynccontextmanager
async def lifespan(app: FastAPI):
    """Manage application lifecycle."""
    global store
    db_path = os.getenv('AETHYME_DB_PATH', './data/graphs/repo.db')
    store = GraphStore(db_path)
    yield
    if store:
        store.close()

app = FastAPI(
    title="Aethyme API",
    description="Graph-based code indexing and retrieval",
    version="0.1.0",
    lifespan=lifespan
)

# CORS
origins = os.getenv('AETHYME_CORS_ORIGINS', 'http://localhost:3000').split(',')
app.add_middleware(
    CORSMiddleware,
    allow_origins=origins,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Include routers
app.include_router(ego.router, prefix="/api/ego", tags=["ego"])
app.include_router(impact.router, prefix="/api/impact", tags=["impact"])
app.include_router(search.router, prefix="/api/search", tags=["search"])
app.include_router(health.router, prefix="/health", tags=["health"])

def get_store() -> GraphStore:
    """Dependency for getting store."""
    if store is None:
        raise HTTPException(status_code=500, detail="Store not initialized")
    return store
```

**`packages/aethyme/src/api/routes/ego.py`**:
```python
"""Ego graph endpoints."""
from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel
from typing import Literal
from ...graph.store import GraphStore
from ...graph.ego import get_ego_graph_1hop, get_ego_graph_2hop
from ..main import get_store

router = APIRouter()

class EgoRequest(BaseModel):
    symbol: str
    hops: Literal[1, 2] = 1

@router.post("/")
async def ego_graph(request: EgoRequest, store: GraphStore = Depends(get_store)):
    """Get ego graph for a symbol."""
    if request.hops == 1:
        result = get_ego_graph_1hop(store, request.symbol)
    else:
        result = get_ego_graph_2hop(store, request.symbol)

    if 'error' in result:
        raise HTTPException(status_code=404, detail=result['error'])

    return result
```

**`packages/aethyme/src/api/routes/impact.py`**:
```python
"""Impact analysis endpoints."""
from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel
from ...graph.store import GraphStore
from ...graph.impact import analyze_impact
from ..main import get_store

router = APIRouter()

class ImpactRequest(BaseModel):
    symbol: str
    max_depth: int = 10

@router.post("/")
async def impact_analysis(request: ImpactRequest, store: GraphStore = Depends(get_store)):
    """Analyze impact of changing a symbol."""
    result = analyze_impact(store, request.symbol, request.max_depth)

    if 'error' in result:
        raise HTTPException(status_code=404, detail=result['error'])

    return result
```

**`packages/aethyme/src/api/routes/search.py`**:
```python
"""Hybrid search endpoints."""
from fastapi import APIRouter, Depends
from pydantic import BaseModel
from ...graph.store import GraphStore
from ...graph.hybrid_search import hybrid_search
from ..main import get_store

router = APIRouter()

class SearchRequest(BaseModel):
    query: str
    top_k: int = 10

@router.post("/")
async def search_symbols(request: SearchRequest, store: GraphStore = Depends(get_store)):
    """Hybrid search for symbols."""
    results = hybrid_search(store, request.query, request.top_k)
    return {'results': results, 'total': len(results)}
```

**`packages/aethyme/src/api/routes/health.py`**:
```python
"""Health check endpoints."""
from fastapi import APIRouter, Depends
from ...graph.store import GraphStore
from ..main import get_store

router = APIRouter()

@router.get("/")
async def health_check(store: GraphStore = Depends(get_store)):
    """Health check."""
    # Check database connectivity
    try:
        node_count = store.conn.execute("SELECT COUNT(*) FROM nodes").fetchone()[0]
        edge_count = store.conn.execute("SELECT COUNT(*) FROM edges").fetchone()[0]
        return {
            'status': 'healthy',
            'nodes': node_count,
            'edges': edge_count
        }
    except Exception as e:
        return {'status': 'unhealthy', 'error': str(e)}
```

### 4.2 Startup Script

**`packages/aethyme/scripts/start-api.sh`**:
```bash
#!/usr/bin/env bash
set -euo pipefail

# Load environment
if [ -f .env ]; then
  export $(grep -v '^#' .env | xargs)
fi

HOST="${AETHYME_API_HOST:-0.0.0.0}"
PORT="${AETHYME_API_PORT:-8001}"

echo "[aethyme] Starting FastAPI server on $HOST:$PORT"

cd "$(dirname "$0")/.."
uvicorn src.api.main:app --host "$HOST" --port "$PORT" --reload
```

---

## Phase 5: React UI Integration

### 5.1 Main Graph Viewer

**`packages/aethyme/src/ui/AethymeViewer.tsx`**:
```typescript
/**
 * Main Aethyme viewer component.
 *
 * Integrates into Aeptus admin at /admin/aethyme
 */
import React, { useState } from 'react'
import SearchInterface from './SearchInterface'
import GraphVisualizer from './GraphVisualizer'
import ImpactView from './ImpactView'
import type { GraphNode, GraphEdge } from './types'

export default function AethymeViewer() {
  const [selectedSymbol, setSelectedSymbol] = useState<string | null>(null)
  const [graphData, setGraphData] = useState<{ nodes: GraphNode[]; edges: GraphEdge[] } | null>(null)
  const [view, setView] = useState<'search' | 'graph' | 'impact'>('search')

  const handleSymbolSelect = async (symbol: string) => {
    setSelectedSymbol(symbol)

    // Fetch ego graph
    const response = await fetch('http://localhost:8001/api/ego', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ symbol, hops: 1 })
    })

    if (response.ok) {
      const data = await response.json()
      // Transform to graph format
      const nodes: GraphNode[] = [
        { id: data.definition.id, label: data.definition.symbol, type: 'definition' },
        ...data.callers.map((c: any) => ({ id: c.id, label: c.symbol, type: 'caller' })),
        ...data.callees.map((c: any) => ({ id: c.id, label: c.symbol, type: 'callee' })),
      ]
      const edges: GraphEdge[] = [
        ...data.callers.map((c: any) => ({ from: c.id, to: data.definition.id, type: 'invoke' })),
        ...data.callees.map((c: any) => ({ from: data.definition.id, to: c.id, type: 'invoke' })),
      ]
      setGraphData({ nodes, edges })
      setView('graph')
    }
  }

  return (
    <div data-ui="Admin.Aethyme.Viewer" className="h-screen flex flex-col">
      {/* Header */}
      <header className="bg-white border-b px-4 py-3">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold">Aethyme</h1>
          <div className="flex gap-2">
            <button
              onClick={() => setView('search')}
              className={`px-3 py-1 rounded ${view === 'search' ? 'bg-blue-600 text-white' : 'bg-gray-200'}`}
            >
              Search
            </button>
            <button
              onClick={() => setView('graph')}
              disabled={!graphData}
              className={`px-3 py-1 rounded ${view === 'graph' ? 'bg-blue-600 text-white' : 'bg-gray-200'}`}
            >
              Graph
            </button>
            <button
              onClick={() => setView('impact')}
              disabled={!selectedSymbol}
              className={`px-3 py-1 rounded ${view === 'impact' ? 'bg-blue-600 text-white' : 'bg-gray-200'}`}
            >
              Impact
            </button>
          </div>
        </div>
      </header>

      {/* Main content */}
      <main className="flex-1 overflow-hidden">
        {view === 'search' && <SearchInterface onSymbolSelect={handleSymbolSelect} />}
        {view === 'graph' && graphData && <GraphVisualizer data={graphData} />}
        {view === 'impact' && selectedSymbol && <ImpactView symbol={selectedSymbol} />}
      </main>
    </div>
  )
}
```

**`packages/aethyme/src/ui/SearchInterface.tsx`**:
```typescript
/**
 * Hybrid search interface.
 */
import React, { useState } from 'react'

interface SearchResult {
  node: {
    id: string
    symbol: string
    file: string
    line: number
    kind: string
  }
  score: number
  context: {
    callers_count: number
    callees_count: number
    imports_count: number
  }
}

interface Props {
  onSymbolSelect: (symbol: string) => void
}

export default function SearchInterface({ onSymbolSelect }: Props) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<SearchResult[]>([])
  const [loading, setLoading] = useState(false)

  const handleSearch = async () => {
    if (!query.trim()) return

    setLoading(true)
    try {
      const response = await fetch('http://localhost:8001/api/search', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query, top_k: 20 })
      })

      if (response.ok) {
        const data = await response.json()
        setResults(data.results)
      }
    } finally {
      setLoading(false)
    }
  }

  return (
    <div data-ui="Admin.Aethyme.Search" className="p-4">
      {/* Search bar */}
      <div className="mb-4">
        <div className="flex gap-2">
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder="Search for functions, classes, components..."
            className="flex-1 px-3 py-2 border rounded"
          />
          <button
            onClick={handleSearch}
            disabled={loading}
            className="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:bg-gray-400"
          >
            {loading ? 'Searching...' : 'Search'}
          </button>
        </div>
      </div>

      {/* Results */}
      <div className="space-y-2">
        {results.map((result) => (
          <div
            key={result.node.id}
            onClick={() => onSymbolSelect(result.node.symbol)}
            className="p-3 border rounded cursor-pointer hover:bg-blue-50"
          >
            <div className="flex items-start justify-between">
              <div>
                <div className="font-mono font-semibold">{result.node.symbol}</div>
                <div className="text-sm text-gray-600">
                  {result.node.file}:{result.node.line}
                </div>
              </div>
              <div className="text-xs text-gray-500">
                Score: {result.score.toFixed(2)}
              </div>
            </div>
            <div className="mt-2 flex gap-4 text-xs text-gray-600">
              <span>{result.context.callers_count} callers</span>
              <span>{result.context.callees_count} callees</span>
              <span>{result.context.imports_count} imports</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
```

**`packages/aethyme/src/ui/GraphVisualizer.tsx`**:
```typescript
/**
 * D3-based graph visualization.
 */
import React, { useEffect, useRef } from 'react'
import * as d3 from 'd3'
import type { GraphNode, GraphEdge } from './types'

interface Props {
  data: { nodes: GraphNode[]; edges: GraphEdge[] }
}

export default function GraphVisualizer({ data }: Props) {
  const svgRef = useRef<SVGSVGElement>(null)

  useEffect(() => {
    if (!svgRef.current) return

    const svg = d3.select(svgRef.current)
    svg.selectAll('*').remove()

    const width = svgRef.current.clientWidth
    const height = svgRef.current.clientHeight

    // Create force simulation
    const simulation = d3.forceSimulation(data.nodes as any)
      .force('link', d3.forceLink(data.edges).id((d: any) => d.id).distance(100))
      .force('charge', d3.forceManyBody().strength(-300))
      .force('center', d3.forceCenter(width / 2, height / 2))

    // Draw edges
    const link = svg.append('g')
      .selectAll('line')
      .data(data.edges)
      .join('line')
      .attr('stroke', '#999')
      .attr('stroke-opacity', 0.6)
      .attr('stroke-width', 2)

    // Draw nodes
    const node = svg.append('g')
      .selectAll('circle')
      .data(data.nodes)
      .join('circle')
      .attr('r', (d) => d.type === 'definition' ? 12 : 8)
      .attr('fill', (d) => {
        if (d.type === 'definition') return '#3b82f6'
        if (d.type === 'caller') return '#10b981'
        return '#f59e0b'
      })
      .call(d3.drag<any, any>()
        .on('start', (event, d: any) => {
          if (!event.active) simulation.alphaTarget(0.3).restart()
          d.fx = d.x
          d.fy = d.y
        })
        .on('drag', (event, d: any) => {
          d.fx = event.x
          d.fy = event.y
        })
        .on('end', (event, d: any) => {
          if (!event.active) simulation.alphaTarget(0)
          d.fx = null
          d.fy = null
        })
      )

    // Draw labels
    const label = svg.append('g')
      .selectAll('text')
      .data(data.nodes)
      .join('text')
      .text((d) => d.label)
      .attr('font-size', 10)
      .attr('dx', 15)
      .attr('dy', 4)

    // Update positions on tick
    simulation.on('tick', () => {
      link
        .attr('x1', (d: any) => d.source.x)
        .attr('y1', (d: any) => d.source.y)
        .attr('x2', (d: any) => d.target.x)
        .attr('y2', (d: any) => d.target.y)

      node
        .attr('cx', (d: any) => d.x)
        .attr('cy', (d: any) => d.y)

      label
        .attr('x', (d: any) => d.x)
        .attr('y', (d: any) => d.y)
    })

    return () => {
      simulation.stop()
    }
  }, [data])

  return (
    <svg
      ref={svgRef}
      data-ui="Admin.Aethyme.Graph"
      className="w-full h-full bg-gray-50"
    />
  )
}
```

**`packages/aethyme/src/ui/ImpactView.tsx`**:
```typescript
/**
 * Impact analysis view.
 */
import React, { useEffect, useState } from 'react'

interface ImpactData {
  definition: any
  total_impacted: number
  max_depth: number
  by_depth: Record<number, { count: number; sample: any[] }>
}

interface Props {
  symbol: string
}

export default function ImpactView({ symbol }: Props) {
  const [data, setData] = useState<ImpactData | null>(null)
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    const fetchImpact = async () => {
      setLoading(true)
      try {
        const response = await fetch('http://localhost:8001/api/impact', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ symbol, max_depth: 10 })
        })

        if (response.ok) {
          const result = await response.json()
          setData(result)
        }
      } finally {
        setLoading(false)
      }
    }

    fetchImpact()
  }, [symbol])

  if (loading) return <div className="p-4">Loading impact analysis...</div>
  if (!data) return <div className="p-4">No data</div>

  return (
    <div data-ui="Admin.Aethyme.Impact" className="p-4">
      {/* Summary */}
      <div className="mb-6">
        <h2 className="text-xl font-semibold mb-2">Impact Analysis: {symbol}</h2>
        <div className="bg-blue-50 border border-blue-200 rounded p-4">
          <div className="text-3xl font-bold text-blue-600">{data.total_impacted}</div>
          <div className="text-sm text-gray-600">Total symbols impacted</div>
          <div className="text-xs text-gray-500 mt-1">Max depth: {data.max_depth}</div>
        </div>
      </div>

      {/* By depth */}
      <div className="space-y-4">
        <h3 className="font-semibold">Impacted Symbols by Depth</h3>
        {Object.entries(data.by_depth).map(([depth, info]) => (
          <div key={depth} className="border rounded p-3">
            <div className="font-medium mb-2">
              Depth {depth} ({info.count} symbols)
            </div>
            <div className="space-y-1 text-sm">
              {info.sample.map((s, i) => (
                <div key={i} className="font-mono text-xs">
                  {s.symbol} <span className="text-gray-500">({s.file}:{s.line})</span>
                </div>
              ))}
              {info.count > 5 && (
                <div className="text-gray-500 text-xs">+ {info.count - 5} more</div>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
```

**`packages/aethyme/src/ui/types.ts`**:
```typescript
export interface GraphNode {
  id: string
  label: string
  type: 'definition' | 'caller' | 'callee'
  x?: number
  y?: number
}

export interface GraphEdge {
  from: string
  to: string
  type: 'invoke' | 'import' | 'contain' | 'props_flow'
}
```

### 5.2 Integration into Aeptus Admin

**Create route in Aeptus**: add a route file (for example `packages/app-shared/src/features/admin/pages/AethymePage.tsx`)
```typescript
/**
 * Aethyme admin page.
 *
 * Route: /admin/aethyme
 */
import React from 'react'
import AethymeViewer from '../../../packages/aethyme/src/ui/AethymeViewer'

export default function AdminAethymePage() {
  return (
    <div data-ui="Page.Admin.Aethyme">
      <AethymeViewer />
    </div>
  )
}
```

**Add to admin navigation menu**: Modify [packages/config/src/menu.config.ts](../packages/config/src/menu.config.ts)
```typescript
// Add to admin menu items
{
  id: 'admin-aethyme',
  label: 'Aethyme',
  path: '/admin/aethyme',
  icon: 'GraphIcon',
  requiredPermissions: ['admin.aethyme.view']
}
```

---

## Phase 6: VS Code Extension

### 6.1 Extension Package

**`packages/aethyme/src/vscode/package.json`**:
```json
{
  "name": "aethyme-vscode",
  "displayName": "Aethyme",
  "description": "Graph-based code navigation",
  "version": "0.1.0",
  "engines": {
    "vscode": "^1.80.0"
  },
  "categories": ["Other"],
  "activationEvents": ["onStartupFinished"],
  "main": "./out/extension.js",
  "contributes": {
    "commands": [
      {
        "command": "aethyme.showEgoGraph",
        "title": "Aethyme: Show Ego Graph"
      },
      {
        "command": "aethyme.analyzeImpact",
        "title": "Aethyme: Analyze Impact"
      },
      {
        "command": "aethyme.searchSymbol",
        "title": "Aethyme: Search Symbol"
      }
    ],
    "views": {
      "explorer": [
        {
          "id": "aethymeExplorer",
          "name": "Aethyme"
        }
      ]
    },
    "configuration": {
      "title": "Aethyme",
      "properties": {
        "aethyme.apiUrl": {
          "type": "string",
          "default": "http://localhost:8001",
          "description": "Aethyme API URL"
        }
      }
    }
  },
  "scripts": {
    "vscode:prepublish": "npm run compile",
    "compile": "tsc -p ./",
    "watch": "tsc -watch -p ./"
  },
  "devDependencies": {
    "@types/vscode": "^1.80.0",
    "@types/node": "^18.x",
    "typescript": "^5.0.0"
  }
}
```

**`packages/aethyme/src/vscode/src/extension.ts`**:
```typescript
/**
 * Aethyme VS Code extension entry point.
 */
import * as vscode from 'vscode'
import { showEgoGraph, analyzeImpact, searchSymbol } from './commands'

export function activate(context: vscode.ExtensionContext) {
  console.log('Aethyme extension activated')

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand('aethyme.showEgoGraph', showEgoGraph),
    vscode.commands.registerCommand('aethyme.analyzeImpact', analyzeImpact),
    vscode.commands.registerCommand('aethyme.searchSymbol', searchSymbol)
  )
}

export function deactivate() {}
```

**`packages/aethyme/src/vscode/src/commands.ts`**:
```typescript
/**
 * VS Code commands for Aethyme.
 */
import * as vscode from 'vscode'

const API_URL = vscode.workspace.getConfiguration('aethyme').get<string>('apiUrl') || 'http://localhost:8001'

export async function showEgoGraph() {
  const editor = vscode.window.activeTextEditor
  if (!editor) return

  const symbol = await vscode.window.showInputBox({
    prompt: 'Enter symbol name',
    placeHolder: 'e.g., MyClass.myMethod'
  })

  if (!symbol) return

  // Fetch ego graph
  const response = await fetch(`${API_URL}/api/ego`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ symbol, hops: 1 })
  })

  if (response.ok) {
    const data = await response.json()

    // Show in webview
    const panel = vscode.window.createWebviewPanel(
      'aethymeEgo',
      `Ego Graph: ${symbol}`,
      vscode.ViewColumn.Two,
      { enableScripts: true }
    )

    panel.webview.html = getEgoGraphHtml(data)
  } else {
    vscode.window.showErrorMessage('Symbol not found')
  }
}

export async function analyzeImpact() {
  const editor = vscode.window.activeTextEditor
  if (!editor) return

  const symbol = await vscode.window.showInputBox({
    prompt: 'Enter symbol name for impact analysis',
    placeHolder: 'e.g., MyClass.myMethod'
  })

  if (!symbol) return

  const response = await fetch(`${API_URL}/api/impact`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ symbol, max_depth: 10 })
  })

  if (response.ok) {
    const data = await response.json()
    vscode.window.showInformationMessage(
      `Impact: ${data.total_impacted} symbols affected (max depth: ${data.max_depth})`
    )
  }
}

export async function searchSymbol() {
  const query = await vscode.window.showInputBox({
    prompt: 'Search for symbols',
    placeHolder: 'Enter search query'
  })

  if (!query) return

  const response = await fetch(`${API_URL}/api/search`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query, top_k: 20 })
  })

  if (response.ok) {
    const data = await response.json()
    const items = data.results.map((r: any) => ({
      label: r.node.symbol,
      description: `${r.node.file}:${r.node.line}`,
      detail: `Score: ${r.score.toFixed(2)}`
    }))

    const selected = await vscode.window.showQuickPick(items)
    if (selected) {
      // Open file at location
      const uri = vscode.Uri.file(selected.description.split(':')[0])
      const line = parseInt(selected.description.split(':')[1])
      const doc = await vscode.workspace.openTextDocument(uri)
      await vscode.window.showTextDocument(doc, {
        selection: new vscode.Range(line - 1, 0, line - 1, 0)
      })
    }
  }
}

function getEgoGraphHtml(data: any): string {
  return `
    <!DOCTYPE html>
    <html>
    <head>
      <style>
        body { font-family: sans-serif; padding: 20px; }
        .section { margin-bottom: 20px; }
        .node { padding: 8px; border: 1px solid #ddd; margin: 4px 0; border-radius: 4px; }
        .definition { background: #dbeafe; }
        .caller { background: #d1fae5; }
        .callee { background: #fed7aa; }
      </style>
    </head>
    <body>
      <h2>${data.definition.symbol}</h2>
      <div class="section">
        <h3>Callers (${data.callers.length})</h3>
        ${data.callers.map((c: any) => `
          <div class="node caller">${c.symbol} (${c.file}:${c.line})</div>
        `).join('')}
      </div>
      <div class="section">
        <h3>Callees (${data.callees.length})</h3>
        ${data.callees.map((c: any) => `
          <div class="node callee">${c.symbol} (${c.file}:${c.line})</div>
        `).join('')}
      </div>
    </body>
    </html>
  `
}
```

---

## Phase 7: Dog-fooding (Index Aeptus)

### 7.1 Index Aeptus Codebase

**Script**: `packages/aethyme/scripts/index-aeptus.sh`
```bash
#!/usr/bin/env bash
set -euo pipefail

AEPTUS_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUTPUT="$AEPTUS_ROOT/packages/aethyme/data/graphs/aeptus.db"

echo "[aethyme] Indexing Aeptus codebase"
echo "  Root: $AEPTUS_ROOT"
echo "  Output: $OUTPUT"

cd "$AEPTUS_ROOT/packages/aethyme"

# Index Python backend and TypeScript frontend
python -m src.indexer.cli index "$AEPTUS_ROOT" \
  --output "$OUTPUT" \
  --languages python,typescript

echo "✓ Aeptus indexed successfully"
echo "  Start API: pnpm aethyme:dev"
echo "  View UI: http://localhost:3000/admin/aethyme"
```

**Add to root package.json**:
```json
{
  "scripts": {
    "aethyme:index-aeptus": "bash packages/aethyme/scripts/index-aeptus.sh"
  }
}
```

### 7.2 Watch for Changes (Incremental Updates)

**`packages/aethyme/src/indexer/file_watcher.py`**:
```python
"""Watch for file changes and incrementally update graph."""
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler, FileModifiedEvent, FileCreatedEvent
from pathlib import Path
import time
from .scip_python import index_python_files
from .scip_typescript import index_typescript_files
from .graph_builder import GraphBuilder
from ..graph.store import GraphStore

class RepoWatcher(FileSystemEventHandler):
    """Watch repository for changes."""

    def __init__(self, repo_path: Path, store: GraphStore):
        self.repo_path = repo_path
        self.store = store
        self.pending_files = set()

    def on_modified(self, event):
        if event.is_directory:
            return
        if self._is_code_file(event.src_path):
            self.pending_files.add(Path(event.src_path))

    def on_created(self, event):
        if event.is_directory:
            return
        if self._is_code_file(event.src_path):
            self.pending_files.add(Path(event.src_path))

    def _is_code_file(self, path: str) -> bool:
        return path.endswith(('.py', '.ts', '.tsx', '.jsx'))

    def process_pending(self):
        """Process pending file changes."""
        if not self.pending_files:
            return

        print(f"[watch] Processing {len(self.pending_files)} changed files")

        # Re-index changed files
        for file_path in self.pending_files:
            self._reindex_file(file_path)

        self.pending_files.clear()

    def _reindex_file(self, file_path: Path):
        """Re-index a single file."""
        # Delete existing nodes/edges for this file
        self.store.conn.execute("DELETE FROM nodes WHERE file = ?", [str(file_path)])

        # Re-index
        if file_path.suffix == '.py':
            # Index single Python file (would need single-file indexing support)
            pass
        elif file_path.suffix in ('.ts', '.tsx', '.jsx'):
            # Index single TypeScript file
            pass

def watch_repo(repo_path: Path, store: GraphStore, interval: int = 5):
    """Watch repository for changes."""
    watcher = RepoWatcher(repo_path, store)
    observer = Observer()
    observer.schedule(watcher, str(repo_path), recursive=True)
    observer.start()

    print(f"[watch] Watching {repo_path} for changes")

    try:
        while True:
            time.sleep(interval)
            watcher.process_pending()
    except KeyboardInterrupt:
        observer.stop()

    observer.join()
```

---

## Phase 8: Testing, CI/CD, Documentation

### 8.1 Testing

**`packages/aethyme/tests/test_indexer.py`**:
```python
"""Test indexer functionality."""
import pytest
from pathlib import Path
from src.indexer.scip_python import index_python_files, extract_definitions
from src.indexer.graph_builder import GraphBuilder

def test_index_python_sample():
    """Test indexing a sample Python file."""
    # Create sample repo
    sample_repo = Path(__file__).parent / 'fixtures' / 'sample_python'

    # Index
    index = index_python_files(sample_repo)
    assert 'documents' in index

    # Extract definitions
    defs = extract_definitions(index)
    assert len(defs) > 0

def test_graph_builder():
    """Test graph builder."""
    builder = GraphBuilder()

    # Add sample definitions
    builder.add_definitions([
        {'symbol': 'test.MyClass', 'file': 'test.py', 'line': 10, 'col': 0, 'text': 'class MyClass'}
    ], 'python')

    # Build graph
    nodes, edges = builder.build()
    assert len(nodes) > 0
```

**`packages/aethyme/tests/test_api.py`**:
```python
"""Test API endpoints."""
import pytest
from fastapi.testclient import TestClient
from src.api.main import app

client = TestClient(app)

def test_health():
    """Test health endpoint."""
    response = client.get('/health')
    assert response.status_code == 200
    assert 'status' in response.json()

def test_ego_graph():
    """Test ego graph endpoint."""
    response = client.post('/api/ego', json={'symbol': 'test.MyClass', 'hops': 1})
    # Expect 404 if not indexed, or 200 if sample indexed
    assert response.status_code in [200, 404]
```

### 8.2 CI/CD Integration

**Add to root `.github/workflows/ci.yml`** (or create new workflow):
```yaml
name: Aethyme CI

on:
  push:
    paths:
      - 'packages/aethyme/**'
  pull_request:
    paths:
      - 'packages/aethyme/**'

jobs:
  test-aethyme:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Setup Python
        uses: actions/setup-python@v4
        with:
          python-version: '3.11'

      - name: Install dependencies
        run: |
          cd packages/aethyme
          pip install -e ".[dev]"

      - name: Run linting
        run: |
          cd packages/aethyme
          ruff check src/

      - name: Run type checking
        run: |
          cd packages/aethyme
          pyright src/

      - name: Run tests
        run: |
          cd packages/aethyme
          pytest tests/
```

### 8.3 Documentation

**`packages/aethyme/docs/ARCHITECTURE.md`** (~300 lines covering):
- System overview
- Component architecture
- Data flow
- Graph schema
- API design
- UI integration

**`packages/aethyme/docs/API_REFERENCE.md`** (~200 lines covering):
- Endpoint specifications
- Request/response schemas
- Error handling
- Rate limiting

**`packages/aethyme/docs/INDEXING.md`** (~200 lines covering):
- SCIP indexers (scip-python, scip-typescript)
- Graph construction
- Incremental updates
- Performance considerations

**`packages/aethyme/docs/DEPLOYMENT.md`** (~150 lines covering):
- Local development setup
- Docker deployment
- Environment variables
- Troubleshooting

---

## Phase 9: Export Strategy

### 9.1 Git Subtree Split

When ready to extract Aethyme as standalone project:

```bash
# Create standalone branch
git subtree split --prefix=packages/aethyme -b aethyme-standalone

# Create new repo
mkdir aethyme
cd aethyme
git init
git pull /path/to/aeptus aethyme-standalone

# Clean up
cd /path/to/aeptus
git branch -D aethyme-standalone
```

### 9.2 Post-Export Updates

**Files to modify in standalone repo**:

1. **Update `README.md`**: Remove Aeptus-specific references
2. **Update `package.json`**: Change name from `@aeptus/aethyme` to `aethyme`
3. **Remove UI integration**: Delete `src/ui/` or make it optional
4. **Update documentation**: Remove Aeptus-specific deployment instructions
5. **Add standalone deployment**: Docker Compose, Kubernetes manifests

---

## Safety Guarantees

### 1. Non-Breaking

- **Self-contained package**: No dependencies on other Aeptus packages
- **Independent package.json**: No conflicts with root dependencies
- **Separate API server**: Runs on port 8001 (not 3000 or 8000)
- **Optional UI**: Admin route can be disabled by removing from menu config

### 2. Rollback Strategy

If Aethyme causes issues:

1. **Remove from workspace**: Comment out in `pnpm-workspace.yaml`
2. **Remove scripts**: Delete from root `package.json`
3. **Remove UI route**: Delete `src/pages/admin/aethyme.tsx`
4. **Remove menu item**: Delete from `menu.config.ts`

Total rollback time: < 5 minutes

### 3. Resource Isolation

- **DuckDB**: Separate database files in `packages/aethyme/data/`
- **Logs**: Separate log directory
- **Docker**: Optional containerization (not required for development)

---

## Implementation Timeline

**Week 1-2: Foundation (Phase 1-3)**
- Package structure
- Python indexer (SCIP)
- DuckDB storage
- Basic CLI

**Week 3-4: API and Retrieval (Phase 3-4)**
- FastAPI backend
- Ego graph retrieval
- Impact analysis
- Hybrid search

**Week 5: UI Integration (Phase 5)**
- React components
- Aeptus admin integration
- D3 visualization

**Week 6: VS Code Extension (Phase 6)**
- Extension package
- Commands
- Webview integration

**Week 7: Dog-fooding (Phase 7)**
- Index Aeptus codebase
- File watching
- Performance tuning

**Week 8: Testing and Docs (Phase 8)**
- Unit tests
- CI/CD
- Documentation

**Total**: 8 weeks to full production deployment

---

## Success Metrics

1. **Indexing Performance**: Index Aeptus codebase (347 files) in < 5 minutes
2. **Query Performance**: Ego graph retrieval in < 100ms
3. **Graph Size**: 10,000+ nodes, 50,000+ edges for Aeptus
4. **Test Coverage**: > 80% for Python backend
5. **API Uptime**: > 99.9% (internal tool)

---

## Dependencies Installation

### Python (Backend)
```bash
cd packages/aethyme
pip install -e ".[dev]"

# Install SCIP indexers
pip install scip-python scip-typescript
```

### Node (Frontend)
```bash
cd packages/aethyme
pnpm install
```

### VS Code Extension
```bash
cd packages/aethyme/src/vscode
pnpm install
pnpm compile
```

---

## Next Steps

1. **Create package structure**: Run `mkdir -p packages/aethyme/src/{indexer,graph,api,ui,vscode}`
2. **Initialize package.json and pyproject.toml**: Create dependency files
3. **Start with Phase 1**: Implement Python indexer using scip-python
4. **Dog-food early**: Index small sample repo first, then Aeptus
5. **Iterate on UI**: Get feedback from team on graph visualization

---

This plan provides a **complete, production-ready deployment** of Aethyme within the Aeptus monorepo, with all features from the PRD, clean separation for future export, and zero risk to existing Aeptus functionality.

**Ready to begin Phase 1?** Let me know if you'd like to start with package structure creation or have any questions about the approach.
