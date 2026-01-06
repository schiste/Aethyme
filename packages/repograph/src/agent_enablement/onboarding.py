"""
Onboarding Manifest Generator

Generates Agent.md manifests with repository-specific context.
Integrates with AI onboarding system and context packs.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import json
import logging
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any

from .context_packs import ContextPackGenerator
from .parity_scorer import ParityScorer, ParityReport

logger = logging.getLogger(__name__)


class OnboardingManifestGenerator:
    """
    Generates Agent.md onboarding manifests.

    Integrates:
    - Repository analysis
    - Context packs
    - Parity scores
    - Best practices from AI_ONBOARDING_CUTTING_EDGE_IDEAS.md
    """

    def __init__(
        self,
        context_generator: Optional[ContextPackGenerator] = None,
        parity_scorer: Optional[ParityScorer] = None
    ):
        self.context_generator = context_generator or ContextPackGenerator()
        self.parity_scorer = parity_scorer or ParityScorer()

    def generate_manifest(
        self,
        repo_path: Path,
        parity_report: Optional[ParityReport] = None,
        template: str = "standard"
    ) -> str:
        """
        Generate Agent.md manifest for repository.

        Args:
            repo_path: Repository path
            parity_report: Optional pre-computed parity report
            template: Template type (standard, minimal, comprehensive)

        Returns:
            Generated Agent.md content
        """
        logger.info(f"Generating Agent.md manifest for: {repo_path}")

        # Analyze repository
        analysis = self._analyze_repository(repo_path)

        # Get parity score if not provided
        if not parity_report:
            parity_report = self.parity_scorer.scan_repository(repo_path)

        # Generate context pack summary
        context_pack = self.context_generator.generate_pack(
            repo_path,
            pack_types=["routes", "env", "tests"],
            compress=False
        )

        # Generate manifest based on template
        if template == "minimal":
            manifest = self._generate_minimal_manifest(repo_path, analysis)
        elif template == "comprehensive":
            manifest = self._generate_comprehensive_manifest(
                repo_path, analysis, parity_report, context_pack
            )
        else:  # standard
            manifest = self._generate_standard_manifest(
                repo_path, analysis, parity_report, context_pack
            )

        logger.info("Agent.md manifest generated")
        return manifest

    def _analyze_repository(self, repo_path: Path) -> Dict[str, Any]:
        """Analyze repository structure and tech stack"""
        analysis = {
            "name": repo_path.name,
            "languages": [],
            "frameworks": [],
            "test_frameworks": [],
            "structure": "monorepo" if (repo_path / "packages").exists() else "single",
            "has_docker": (repo_path / "Dockerfile").exists(),
            "has_ci": (repo_path / ".github" / "workflows").exists(),
        }

        # Detect languages
        if list(repo_path.glob("**/*.py")):
            analysis["languages"].append("Python")
        if list(repo_path.glob("**/*.ts")):
            analysis["languages"].append("TypeScript")
        if list(repo_path.glob("**/*.js")):
            analysis["languages"].append("JavaScript")

        # Detect frameworks
        if (repo_path / "package.json").exists():
            try:
                pkg = json.loads((repo_path / "package.json").read_text())
                deps = {**pkg.get("dependencies", {}), **pkg.get("devDependencies", {})}

                if "react" in deps:
                    analysis["frameworks"].append("React")
                if "next" in deps:
                    analysis["frameworks"].append("Next.js")
                if "jest" in deps:
                    analysis["test_frameworks"].append("Jest")
            except:
                pass

        if (repo_path / "requirements.txt").exists() or (repo_path / "pyproject.toml").exists():
            # Check for Django, FastAPI, etc.
            try:
                if (repo_path / "manage.py").exists():
                    analysis["frameworks"].append("Django")
                # Could parse requirements for more
            except:
                pass

        return analysis

    def _generate_minimal_manifest(
        self,
        repo_path: Path,
        analysis: Dict[str, Any]
    ) -> str:
        """Generate minimal Agent.md manifest"""

        manifest = f"""# AI Agent Onboarding Manifest

## Project Identity
- **Name:** {analysis['name']}
- **Languages:** {', '.join(analysis['languages']) if analysis['languages'] else 'Unknown'}
- **Type:** {analysis['structure']}

## Quick Start
```bash
# Install dependencies
# [Add your commands]

# Run development server
# [Add your commands]

# Run tests
# [Add your commands]
```

## Key Conventions
- [Add your coding conventions]

## Red Flags
- ❌ [Things to never do]

---
*Generated by RepoGraph Agent-Enablement System*
*Last updated: {datetime.utcnow().isoformat()}Z*
"""

        return manifest

    def _generate_standard_manifest(
        self,
        repo_path: Path,
        analysis: Dict[str, Any],
        parity_report: ParityReport,
        context_pack
    ) -> str:
        """Generate standard Agent.md manifest"""

        # Extract key info from context pack
        env_vars = context_pack.context_packs.get("env", {}).get("variables", [])
        routes_count = context_pack.context_packs.get("routes", {}).get("total_count", 0)
        test_count = context_pack.context_packs.get("tests", {}).get("total_count", 0)

        manifest = f"""# AI Agent Onboarding Manifest

## Project Identity
- **Name:** {analysis['name']}
- **Languages:** {', '.join(analysis['languages']) if analysis['languages'] else 'Unknown'}
- **Frameworks:** {', '.join(analysis['frameworks']) if analysis['frameworks'] else 'None detected'}
- **Type:** {analysis['structure']}
- **Test Frameworks:** {', '.join(analysis['test_frameworks']) if analysis['test_frameworks'] else 'None detected'}

## Agent Readiness Score
**Overall Score:** {parity_report.overall_score}/100

{chr(10).join(parity_report.recommendations[:3])}

## Project Overview
[TODO: Add high-level description of what this project does]

## Build/Run/Test
```bash
# Install dependencies
# TODO: Add installation commands

# Run development server
# TODO: Add dev server commands

# Run tests
# TODO: Add test commands

# Quality checks
# TODO: Add quality check commands
```

## Environment Variables
Required environment variables: {len([v for v in env_vars if v.get('required')])}
Total environment variables: {len(env_vars)}

Key variables:
"""

        # Add top environment variables
        for var in env_vars[:5]:
            manifest += f"- `{var['key']}`: "
            if var.get('default'):
                manifest += f"(default: {var['default']})"
            else:
                manifest += "(required)"
            manifest += "\n"

        manifest += f"""
## Architecture
- **Routes:** {routes_count} defined routes
- **Tests:** {test_count} test files
- **Structure:** {analysis['structure']}

[TODO: Add architecture details, key patterns, data flow]

## Coding Conventions
[TODO: Add coding conventions specific to this project]

## Common Tasks
[TODO: Add common development tasks and how to do them]

## Red Flags (NEVER do this)
- ❌ [Add project-specific anti-patterns]
- ❌ Skip quality checks before committing
- ❌ Modify files without understanding architecture

## Resources
- Context Pack: Available via `repograph agent context-pack`
- Parity Report: Run `repograph agent parity`
- Discovery: Use `repograph query search <term>`

---
*Generated by RepoGraph Agent-Enablement System*
*Last updated: {datetime.utcnow().isoformat()}Z*
*Agent Readiness Score: {parity_report.overall_score}/100*
"""

        return manifest

    def _generate_comprehensive_manifest(
        self,
        repo_path: Path,
        analysis: Dict[str, Any],
        parity_report: ParityReport,
        context_pack
    ) -> str:
        """Generate comprehensive Agent.md manifest with all details"""

        standard = self._generate_standard_manifest(
            repo_path, analysis, parity_report, context_pack
        )

        # Add detailed sections
        comprehensive = standard + f"""

## Detailed Parity Report

### Invariant Scores
"""

        for invariant_id, score in parity_report.invariants.items():
            comprehensive += f"""
#### {score.invariant_name}
- Score: {score.score}/100
- Violations: {score.violations}
- Can Autofix: {'✅ Yes' if score.can_autofix else '❌ No'}
"""

        comprehensive += """
## Context Pack Details

Detailed context is available in JSON format. Run:
```bash
repograph agent context-pack --output context.json
```

## Integration with RepoGraph

This repository is configured for RepoGraph integration. Available commands:

```bash
# Index repository
repograph index --org <org> --repo <repo>

# Search code
repograph query search "your search term"

# Get ego graph
repograph query ego --symbol <symbol>

# Check agent readiness
repograph agent parity

# Generate fixes
repograph autofix --safe
```

## Onboarding Checklist

For new AI agents:

1. ✅ Read this Agent.md file
2. ✅ Review parity score and gaps
3. ✅ Load context pack
4. ✅ Understand architecture
5. ✅ Review coding conventions
6. ✅ Know red flags
7. ✅ Explore with RepoGraph search

## Next Steps

[TODO: Add recommended next steps for agents starting work]

---
*For the complete onboarding guide, see: /docs/AI_ONBOARDING_CUTTING_EDGE_IDEAS.md*
"""

        return comprehensive

    def save_manifest(
        self,
        manifest: str,
        repo_path: Path,
        filename: str = "agent.md"
    ) -> Path:
        """Save manifest to repository"""
        manifest_path = repo_path / filename
        manifest_path.write_text(manifest)
        logger.info(f"Manifest saved to: {manifest_path}")
        return manifest_path
