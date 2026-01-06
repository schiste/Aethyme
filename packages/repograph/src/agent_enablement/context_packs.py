"""
Context Pack Generator

Extracts minimal context packs for AI agents from repositories.
Generates deterministic, compressed, and versioned context data.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import hashlib
import gzip
import json
import logging
import re
from dataclasses import dataclass, field, asdict
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any
import ast

logger = logging.getLogger(__name__)


@dataclass
class ContextPack:
    """Complete context pack for a repository"""
    repo: str
    version: str
    context_packs: Dict[str, Any] = field(default_factory=dict)
    generated_at: str = ""
    checksum: str = ""
    compression: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary"""
        return asdict(self)

    def to_json(self, indent: int = 2) -> str:
        """Convert to JSON string"""
        return json.dumps(self.to_dict(), indent=indent)


class ContextPackGenerator:
    """
    Generates minimal context packs for AI agents.

    Six core pack types:
    1. menu: Navigation hierarchy
    2. routes: Application routes with parameters
    3. env: Environment variables and defaults
    4. tests: Test file locations and patterns
    5. models: Core entities and schemas
    6. api: API contracts (OpenAPI/GraphQL schemas)

    Features:
    - Deterministic output (same repo = same pack)
    - Compressed for large packs
    - Incremental delta support
    - Versioned format
    """

    PACK_VERSION = "1.0.0"

    def __init__(self):
        self.extractors = {
            "menu": self._extract_menu_pack,
            "routes": self._extract_routes_pack,
            "env": self._extract_env_pack,
            "tests": self._extract_tests_pack,
            "models": self._extract_models_pack,
            "api": self._extract_api_pack,
        }

    def generate_pack(
        self,
        repo_path: Path,
        pack_types: Optional[List[str]] = None,
        compress: bool = True
    ) -> ContextPack:
        """
        Generate context pack for repository.

        Args:
            repo_path: Repository root path
            pack_types: List of pack types to generate (None = all)
            compress: Whether to compress large packs

        Returns:
            Complete context pack
        """
        logger.info(f"Generating context pack for: {repo_path}")

        if pack_types is None:
            pack_types = list(self.extractors.keys())

        context_packs = {}

        for pack_type in pack_types:
            if pack_type in self.extractors:
                logger.info(f"  Extracting {pack_type} pack...")
                extractor = self.extractors[pack_type]
                try:
                    pack_data = extractor(repo_path)
                    context_packs[pack_type] = pack_data
                except Exception as e:
                    logger.error(f"Error extracting {pack_type}: {e}")
                    context_packs[pack_type] = {"error": str(e)}

        pack = ContextPack(
            repo=repo_path.name,
            version=self.PACK_VERSION,
            context_packs=context_packs,
            generated_at=datetime.utcnow().isoformat() + "Z",
        )

        # Calculate checksum for determinism validation
        pack.checksum = self._calculate_checksum(pack)

        # Compress if needed
        if compress:
            pack_json = pack.to_json()
            if len(pack_json) > 100000:  # > 100KB
                pack.compression = "gzip"
                logger.info("  Pack size > 100KB, compression applied")

        logger.info(f"Context pack generated ({len(pack_types)} types)")
        return pack

    # Pack extractors

    def _extract_menu_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract menu/navigation hierarchy"""
        menu = {
            "structure": [],
            "total_items": 0,
            "sources": []
        }

        # Look for menu config files
        menu_configs = list(repo_path.glob("**/menu.config.*")) + \
                      list(repo_path.glob("**/navigation.config.*"))

        for config_file in menu_configs:
            menu["sources"].append(str(config_file.relative_to(repo_path)))

            try:
                content = config_file.read_text()

                # Parse menu structure from config
                # Support TypeScript/JavaScript configs
                if config_file.suffix in [".ts", ".js"]:
                    structure = self._parse_menu_from_js(content)
                    menu["structure"].extend(structure)

            except Exception as e:
                logger.warning(f"Error parsing menu from {config_file}: {e}")

        # Fallback: Scan for route components
        if not menu["structure"]:
            menu["structure"] = self._infer_menu_from_routes(repo_path)

        menu["total_items"] = self._count_menu_items(menu["structure"])

        return menu

    def _extract_routes_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract application routes with parameters"""
        routes = {
            "routes": [],
            "total_count": 0,
            "sources": []
        }

        # Look for route config files
        route_configs = list(repo_path.glob("**/routes.config.*")) + \
                       list(repo_path.glob("**/router.config.*"))

        for config_file in route_configs:
            routes["sources"].append(str(config_file.relative_to(repo_path)))

            try:
                content = config_file.read_text()

                # Parse routes
                if config_file.suffix in [".ts", ".js"]:
                    parsed_routes = self._parse_routes_from_js(content)
                    routes["routes"].extend(parsed_routes)

            except Exception as e:
                logger.warning(f"Error parsing routes from {config_file}: {e}")

        # Fallback: Scan for route definitions in code
        if not routes["routes"]:
            routes["routes"] = self._infer_routes_from_code(repo_path)

        routes["total_count"] = len(routes["routes"])

        return routes

    def _extract_env_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract environment variables and defaults"""
        env = {
            "variables": [],
            "total_count": 0,
            "sources": []
        }

        # Look for env files
        env_files = [
            ".env.example",
            ".env.template",
            "env.example",
            "config/env.example"
        ]

        for env_file_name in env_files:
            env_file = repo_path / env_file_name
            if env_file.exists():
                env["sources"].append(env_file_name)

                try:
                    content = env_file.read_text()
                    variables = self._parse_env_file(content)
                    env["variables"].extend(variables)
                except Exception as e:
                    logger.warning(f"Error parsing {env_file_name}: {e}")

        # Also scan config files for env var references
        config_dirs = ["config", "src/config", "backend/config"]
        for config_dir in config_dirs:
            config_path = repo_path / config_dir
            if config_path.exists():
                env_vars = self._scan_for_env_vars(config_path)
                # Add any new vars not already in list
                existing_keys = {v["key"] for v in env["variables"]}
                for var in env_vars:
                    if var["key"] not in existing_keys:
                        env["variables"].append(var)

        env["total_count"] = len(env["variables"])

        return env

    def _extract_tests_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract test file locations and patterns"""
        tests = {
            "test_files": [],
            "total_count": 0,
            "patterns": {},
            "frameworks": []
        }

        # Common test patterns
        test_patterns = [
            "**/*.test.ts",
            "**/*.test.js",
            "**/*.spec.ts",
            "**/*.spec.js",
            "**/test_*.py",
            "**/*_test.py"
        ]

        for pattern in test_patterns:
            test_files = list(repo_path.glob(pattern))
            if test_files:
                framework = self._detect_test_framework(pattern)
                if framework and framework not in tests["frameworks"]:
                    tests["frameworks"].append(framework)

                for test_file in test_files:
                    rel_path = str(test_file.relative_to(repo_path))
                    tests["test_files"].append({
                        "path": rel_path,
                        "framework": framework,
                        "pattern": pattern
                    })

        tests["total_count"] = len(tests["test_files"])

        # Extract test patterns (data-ui selectors used, etc.)
        tests["patterns"] = self._extract_test_patterns(repo_path, tests["test_files"])

        return tests

    def _extract_models_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract core entities and schemas"""
        models = {
            "models": [],
            "total_count": 0,
            "sources": []
        }

        # Look for model definitions
        # Python: Django/SQLAlchemy models
        model_files = list(repo_path.glob("**/models.py")) + \
                     list(repo_path.glob("**/models/*.py"))

        for model_file in model_files:
            models["sources"].append(str(model_file.relative_to(repo_path)))

            try:
                parsed_models = self._parse_python_models(model_file)
                models["models"].extend(parsed_models)
            except Exception as e:
                logger.warning(f"Error parsing models from {model_file}: {e}")

        # TypeScript: Type definitions
        type_files = list(repo_path.glob("**/types.ts")) + \
                    list(repo_path.glob("**/types/*.ts")) + \
                    list(repo_path.glob("**/models/*.ts"))

        for type_file in type_files:
            models["sources"].append(str(type_file.relative_to(repo_path)))

            try:
                parsed_types = self._parse_typescript_types(type_file)
                models["models"].extend(parsed_types)
            except Exception as e:
                logger.warning(f"Error parsing types from {type_file}: {e}")

        models["total_count"] = len(models["models"])

        return models

    def _extract_api_pack(self, repo_path: Path) -> Dict[str, Any]:
        """Extract API contracts (OpenAPI/GraphQL schemas)"""
        api = {
            "endpoints": [],
            "schemas": [],
            "total_count": 0,
            "sources": []
        }

        # Look for OpenAPI specs
        openapi_files = list(repo_path.glob("**/openapi.yaml")) + \
                       list(repo_path.glob("**/openapi.json")) + \
                       list(repo_path.glob("**/swagger.yaml"))

        for spec_file in openapi_files:
            api["sources"].append(str(spec_file.relative_to(repo_path)))

            try:
                if spec_file.suffix == ".json":
                    spec = json.loads(spec_file.read_text())
                else:
                    # Would need yaml parser, simplified here
                    spec = {"info": {"title": "API Spec"}}

                # Extract endpoints
                if "paths" in spec:
                    for path, methods in spec["paths"].items():
                        for method, details in methods.items():
                            api["endpoints"].append({
                                "path": path,
                                "method": method.upper(),
                                "summary": details.get("summary", "")
                            })

            except Exception as e:
                logger.warning(f"Error parsing OpenAPI from {spec_file}: {e}")

        # Look for GraphQL schemas
        graphql_files = list(repo_path.glob("**/schema.graphql")) + \
                       list(repo_path.glob("**/*.graphql"))

        for schema_file in graphql_files:
            api["sources"].append(str(schema_file.relative_to(repo_path)))
            api["schemas"].append({
                "type": "graphql",
                "file": str(schema_file.relative_to(repo_path))
            })

        # Fallback: Infer from backend code
        if not api["endpoints"]:
            api["endpoints"] = self._infer_api_from_code(repo_path)

        api["total_count"] = len(api["endpoints"]) + len(api["schemas"])

        return api

    # Helper parsers

    def _parse_menu_from_js(self, content: str) -> List[Dict[str, Any]]:
        """Parse menu structure from JavaScript/TypeScript config"""
        menu_items = []

        # Simple regex-based parsing (would use AST parser in production)
        # Look for menu item patterns
        pattern = r'\{[^}]*label:\s*["\']([^"\']+)["\'][^}]*path:\s*["\']([^"\']+)["\'][^}]*\}'
        matches = re.finditer(pattern, content, re.DOTALL)

        for match in matches:
            menu_items.append({
                "label": match.group(1),
                "path": match.group(2)
            })

        return menu_items

    def _parse_routes_from_js(self, content: str) -> List[Dict[str, Any]]:
        """Parse routes from JavaScript/TypeScript config"""
        routes = []

        # Look for route definitions
        pattern = r'\{[^}]*path:\s*["\']([^"\']+)["\'][^}]*component:\s*["\']?([^"\',}]+)["\']?[^}]*\}'
        matches = re.finditer(pattern, content, re.DOTALL)

        for match in matches:
            path = match.group(1)
            component = match.group(2)

            # Extract path parameters
            params = re.findall(r':(\w+)', path)

            routes.append({
                "path": path,
                "component": component,
                "params": params
            })

        return routes

    def _parse_env_file(self, content: str) -> List[Dict[str, Any]]:
        """Parse environment file"""
        variables = []

        for line in content.split('\n'):
            line = line.strip()

            # Skip comments and empty lines
            if not line or line.startswith('#'):
                continue

            # Parse KEY=value or KEY=
            if '=' in line:
                key, _, value = line.partition('=')
                variables.append({
                    "key": key.strip(),
                    "default": value.strip() if value else None,
                    "required": value == ""
                })

        return variables

    def _scan_for_env_vars(self, config_path: Path) -> List[Dict[str, Any]]:
        """Scan config files for environment variable references"""
        env_vars = []
        seen = set()

        for file_path in config_path.rglob("*.py"):
            try:
                content = file_path.read_text()

                # Look for os.environ, os.getenv patterns
                patterns = [
                    r'os\.environ\[["\']([^"\']+)["\']\]',
                    r'os\.getenv\(["\']([^"\']+)["\']',
                    r'env\(["\']([^"\']+)["\']\)'
                ]

                for pattern in patterns:
                    matches = re.finditer(pattern, content)
                    for match in matches:
                        key = match.group(1)
                        if key not in seen:
                            env_vars.append({
                                "key": key,
                                "default": None,
                                "required": True
                            })
                            seen.add(key)

            except Exception:
                pass

        return env_vars

    def _detect_test_framework(self, pattern: str) -> Optional[str]:
        """Detect test framework from file pattern"""
        if ".test.ts" in pattern or ".spec.ts" in pattern:
            return "jest"
        elif ".test.js" in pattern:
            return "jest/mocha"
        elif "test_" in pattern or "_test.py" in pattern:
            return "pytest"
        return None

    def _extract_test_patterns(self, repo_path: Path, test_files: List[Dict]) -> Dict[str, Any]:
        """Extract common test patterns"""
        patterns = {
            "data_ui_selectors": [],
            "api_endpoints_tested": [],
            "test_count": len(test_files)
        }

        # Sample a few test files to extract patterns
        for test_file_info in test_files[:10]:  # Sample first 10
            file_path = repo_path / test_file_info["path"]
            try:
                content = file_path.read_text()

                # Extract data-ui selectors used in tests
                selectors = re.findall(r'data-ui=["\']([^"\']+)["\']', content)
                patterns["data_ui_selectors"].extend(selectors)

                # Extract API endpoints tested
                endpoints = re.findall(r'["\']/(api/[^"\']+)["\']', content)
                patterns["api_endpoints_tested"].extend(endpoints)

            except Exception:
                pass

        # Deduplicate
        patterns["data_ui_selectors"] = list(set(patterns["data_ui_selectors"]))
        patterns["api_endpoints_tested"] = list(set(patterns["api_endpoints_tested"]))

        return patterns

    def _parse_python_models(self, model_file: Path) -> List[Dict[str, Any]]:
        """Parse Python model definitions"""
        models = []

        try:
            content = model_file.read_text()
            tree = ast.parse(content)

            for node in ast.walk(tree):
                if isinstance(node, ast.ClassDef):
                    # Check if it's a model class
                    base_names = [base.id for base in node.bases if isinstance(base, ast.Name)]
                    if any(base in ["Model", "models.Model", "Base"] for base in base_names):
                        # Extract fields
                        fields = []
                        for item in node.body:
                            if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                                field_name = item.target.id
                                field_type = ast.unparse(item.annotation) if hasattr(ast, 'unparse') else "unknown"
                                fields.append({
                                    "name": field_name,
                                    "type": field_type
                                })

                        models.append({
                            "name": node.name,
                            "fields": fields,
                            "source": str(model_file.name)
                        })

        except Exception as e:
            logger.warning(f"Error parsing Python models: {e}")

        return models

    def _parse_typescript_types(self, type_file: Path) -> List[Dict[str, Any]]:
        """Parse TypeScript type definitions"""
        types = []

        try:
            content = type_file.read_text()

            # Simple regex-based parsing for interfaces/types
            interface_pattern = r'interface\s+(\w+)\s*\{([^}]+)\}'
            matches = re.finditer(interface_pattern, content, re.DOTALL)

            for match in matches:
                name = match.group(1)
                body = match.group(2)

                # Extract fields
                fields = []
                for line in body.split('\n'):
                    field_match = re.match(r'\s*(\w+)[\?:]?\s*:\s*([^;]+)', line)
                    if field_match:
                        fields.append({
                            "name": field_match.group(1),
                            "type": field_match.group(2).strip()
                        })

                types.append({
                    "name": name,
                    "fields": fields,
                    "source": str(type_file.name)
                })

        except Exception as e:
            logger.warning(f"Error parsing TypeScript types: {e}")

        return types

    def _infer_menu_from_routes(self, repo_path: Path) -> List[Dict[str, Any]]:
        """Infer menu structure from route components"""
        # Simplified fallback
        return []

    def _infer_routes_from_code(self, repo_path: Path) -> List[Dict[str, Any]]:
        """Infer routes from code files"""
        # Simplified fallback
        return []

    def _infer_api_from_code(self, repo_path: Path) -> List[Dict[str, Any]]:
        """Infer API endpoints from backend code"""
        endpoints = []

        # Look for Django/FastAPI route definitions
        for pattern in ["**/views.py", "**/api.py", "**/routes.py"]:
            for file_path in repo_path.glob(pattern):
                try:
                    content = file_path.read_text()

                    # Look for route decorators
                    route_pattern = r'@\w+\.route\(["\']([^"\']+)["\']'
                    matches = re.finditer(route_pattern, content)

                    for match in matches:
                        endpoints.append({
                            "path": match.group(1),
                            "method": "GET",  # Default
                            "source": str(file_path.relative_to(repo_path))
                        })

                except Exception:
                    pass

        return endpoints

    def _count_menu_items(self, structure: List[Dict]) -> int:
        """Count total menu items recursively"""
        count = len(structure)
        for item in structure:
            if "children" in item:
                count += self._count_menu_items(item["children"])
        return count

    def _calculate_checksum(self, pack: ContextPack) -> str:
        """Calculate deterministic checksum for pack"""
        # Use sorted JSON for determinism
        pack_dict = pack.to_dict()
        pack_dict.pop("generated_at", None)  # Exclude timestamp
        pack_dict.pop("checksum", None)  # Exclude checksum itself

        json_str = json.dumps(pack_dict, sort_keys=True)
        checksum = hashlib.sha256(json_str.encode()).hexdigest()[:16]

        return checksum

    def save_pack(self, pack: ContextPack, output_path: Path, compress: bool = True) -> None:
        """Save context pack to file"""
        output_path.parent.mkdir(parents=True, exist_ok=True)

        pack_json = pack.to_json()

        if compress and pack.compression == "gzip":
            # Compress and save
            compressed = gzip.compress(pack_json.encode())
            output_path = output_path.with_suffix(output_path.suffix + ".gz")
            output_path.write_bytes(compressed)
            logger.info(f"Compressed pack saved to: {output_path}")
        else:
            output_path.write_text(pack_json)
            logger.info(f"Pack saved to: {output_path}")

    def load_pack(self, pack_path: Path) -> ContextPack:
        """Load context pack from file"""
        if pack_path.suffix == ".gz":
            # Decompress
            compressed = pack_path.read_bytes()
            pack_json = gzip.decompress(compressed).decode()
        else:
            pack_json = pack_path.read_text()

        pack_dict = json.loads(pack_json)
        pack = ContextPack(**pack_dict)

        return pack

    def generate_delta(self, old_pack: ContextPack, new_pack: ContextPack) -> Dict[str, Any]:
        """Generate incremental delta between two packs"""
        delta = {
            "from_version": old_pack.checksum,
            "to_version": new_pack.checksum,
            "changes": {}
        }

        # Compare each pack type
        for pack_type in new_pack.context_packs.keys():
            if pack_type not in old_pack.context_packs:
                delta["changes"][pack_type] = {
                    "type": "added",
                    "data": new_pack.context_packs[pack_type]
                }
            elif old_pack.context_packs[pack_type] != new_pack.context_packs[pack_type]:
                delta["changes"][pack_type] = {
                    "type": "modified",
                    "data": new_pack.context_packs[pack_type]
                }

        # Check for removed packs
        for pack_type in old_pack.context_packs.keys():
            if pack_type not in new_pack.context_packs:
                delta["changes"][pack_type] = {
                    "type": "removed"
                }

        return delta
