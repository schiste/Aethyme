"""
Tests for Invariant Rules Engine

Tests all 9 invariant detectors and context exporters.
"""

import pytest
from pathlib import Path
from src.agent_enablement.invariants import InvariantRulesEngine, InvariantSeverity


class TestInvariantRulesEngine:
    """Test invariant rules engine"""

    @pytest.fixture
    def engine(self):
        return InvariantRulesEngine()

    @pytest.fixture
    def exemplar_repo(self, tmp_path):
        """Create exemplar repository with perfect parity"""
        repo = tmp_path / "exemplar"
        repo.mkdir()

        # Create perfect React component with data-ui
        (repo / "Component.tsx").write_text('''
export const Button = () => (
  <button data-ui="submit-button">Submit</button>
);
        ''')

        # Create generated file with marker
        gen_file = repo / "generated" / "types.ts"
        gen_file.parent.mkdir()
        gen_file.write_text('// @generated - DO NOT EDIT\nexport type Foo = string;')

        # Create FOLDER.md
        (repo / "FOLDER.md").write_text("# Exemplar Repo\n\nPerfect parity.")

        # Create Agent.md
        (repo / "agent.md").write_text("# Agent Manifest\n\nExemplar repo.")

        return repo

    def test_get_all_rules(self, engine):
        """Test getting all invariant rules"""
        rules = engine.get_all_rules()
        assert len(rules) == 9
        assert all(rule.id for rule in rules)

    def test_data_ui_detector_success(self, engine, tmp_path):
        """Test data-ui detector with compliant code"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        # Component with data-ui
        (repo / "Button.tsx").write_text('''
<button data-ui="submit">Submit</button>
        ''')

        violations = engine._detect_missing_data_ui(repo)
        assert len(violations) == 0

    def test_data_ui_detector_violation(self, engine, tmp_path):
        """Test data-ui detector finds violations"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        # Component without data-ui
        (repo / "Button.tsx").write_text('''
<button>Submit</button>
        ''')

        violations = engine._detect_missing_data_ui(repo)
        assert len(violations) > 0
        assert violations[0].invariant_id == "data-ui-selectors"
        assert violations[0].severity == InvariantSeverity.BLOCKER

    def test_generated_file_detector(self, engine, tmp_path):
        """Test generated file protection detector"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        gen_dir = repo / "generated"
        gen_dir.mkdir()

        # Generated file without marker
        (gen_dir / "types.ts").write_text("export type Foo = string;")

        violations = engine._detect_unprotected_generated_files(repo)
        assert len(violations) > 0
        assert violations[0].invariant_id == "generated-file-protection"

    def test_absolute_links_detector(self, engine, tmp_path):
        """Test relative links detector"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        # Markdown with absolute link
        (repo / "README.md").write_text('''
[Docs](https://github.com/user/repo/blob/main/docs/README.md)
        ''')

        violations = engine._detect_absolute_links(repo)
        assert len(violations) > 0
        assert violations[0].invariant_id == "relative-links"

    def test_folder_docs_detector(self, engine, tmp_path):
        """Test FOLDER.md detector"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        src_dir = repo / "src"
        src_dir.mkdir()

        # Add code files but no FOLDER.md
        (src_dir / "index.ts").write_text("export const foo = 'bar';")

        violations = engine._detect_missing_folder_docs(repo)
        assert len(violations) > 0

    def test_agent_manifest_detector(self, engine, tmp_path):
        """Test Agent.md detector"""
        repo = tmp_path / "test_repo"
        repo.mkdir()

        violations = engine._detect_missing_agent_manifest(repo)
        assert len(violations) == 1
        assert violations[0].invariant_id == "onboarding-prompts"

        # Add Agent.md
        (repo / "agent.md").write_text("# Manifest")
        violations = engine._detect_missing_agent_manifest(repo)
        assert len(violations) == 0

    def test_run_all_detectors(self, engine, exemplar_repo):
        """Test running all detectors on exemplar repo"""
        results = engine.run_all_detectors(exemplar_repo)

        assert len(results) == 9
        # Exemplar should have minimal violations
        total_violations = sum(len(v) for v in results.values())
        assert total_violations < 5  # Some invariants may not be fully satisfied

    def test_export_context(self, engine, exemplar_repo):
        """Test context export from all invariants"""
        context = engine.export_all_context(exemplar_repo)

        assert len(context) == 9
        assert "data-ui-selectors" in context
        assert "ui_elements" in context["data-ui-selectors"]


class TestInvariantViolation:
    """Test InvariantViolation dataclass"""

    def test_violation_creation(self):
        """Test creating violation"""
        from src.agent_enablement.invariants import InvariantViolation

        violation = InvariantViolation(
            invariant_id="test-invariant",
            file_path="src/test.ts",
            line_number=10,
            message="Test violation",
            severity=InvariantSeverity.WARNING
        )

        assert violation.invariant_id == "test-invariant"
        assert violation.line_number == 10
        assert violation.severity == InvariantSeverity.WARNING


@pytest.mark.parametrize("invariant_id,expected_category", [
    ("data-ui-selectors", "testing"),
    ("generated-file-protection", "safety"),
    ("relative-links", "documentation"),
    ("folder-docs", "documentation"),
    ("config-driven-routes", "architecture"),
])
def test_invariant_categories(invariant_id, expected_category):
    """Test invariants are in correct categories"""
    engine = InvariantRulesEngine()
    rule = engine.get_rule(invariant_id)
    assert rule is not None
    assert rule.category == expected_category
