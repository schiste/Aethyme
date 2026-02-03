"""Tests for Onboarding Manifest Generator and Integration Tests"""

import pytest
from pathlib import Path
from src.agent_enablement.onboarding import OnboardingManifestGenerator


class TestOnboardingManifestGenerator:
    """Test Agent.md generation"""

    @pytest.fixture
    def generator(self):
        return OnboardingManifestGenerator()

    @pytest.fixture
    def sample_repo(self, tmp_path):
        repo = tmp_path / "sample"
        repo.mkdir()
        (repo / "package.json").write_text('{"name": "test", "dependencies": {"react": "18.0.0"}}')
        return repo

    def test_generate_minimal_manifest(self, generator, sample_repo):
        """Test minimal manifest generation"""
        manifest = generator.generate_manifest(sample_repo, template="minimal")
        assert "# AI Agent Onboarding Manifest" in manifest
        assert "Project Identity" in manifest

    def test_generate_standard_manifest(self, generator, sample_repo):
        """Test standard manifest generation"""
        manifest = generator.generate_manifest(sample_repo, template="standard")
        assert "Agent Readiness Score" in manifest
        assert "Build/Run/Test" in manifest

    def test_generate_comprehensive_manifest(self, generator, sample_repo):
        """Test comprehensive manifest generation"""
        manifest = generator.generate_manifest(sample_repo, template="comprehensive")
        assert "Detailed Parity Report" in manifest
        assert "Context Pack Details" in manifest

    def test_save_manifest(self, generator, sample_repo):
        """Test manifest saving"""
        manifest = generator.generate_manifest(sample_repo)
        path = generator.save_manifest(manifest, sample_repo)

        assert path.exists()
        assert path.name == "agent.md"

# Integration tests
class TestIntegrationWorkflows:
    """End-to-end integration tests"""

    def test_complete_workflow(self, tmp_path):
        """Test complete agent enablement workflow"""
        from src.agent_enablement import (
            ParityScorer, GapDetector, ContextPackGenerator,
            AutofixPatchGenerator, OnboardingManifestGenerator
        )

        # Create test repo
        repo = tmp_path / "integration"
        repo.mkdir()
        (repo / "Component.tsx").write_text('<button>Click</button>')

        # Step 1: Scan parity
        scorer = ParityScorer()
        report = scorer.scan_repository(repo)
        assert report.overall_score < 100  # Has violations

        # Step 2: Detect gaps
        detector = GapDetector()
        gaps = detector.detect_gaps(repo, parity_report=report)
        assert len(gaps) > 0

        # Step 3: Generate context pack
        pack_gen = ContextPackGenerator()
        pack = pack_gen.generate_pack(repo)
        assert pack.repo == "integration"

        # Step 4: Generate manifest
        manifest_gen = OnboardingManifestGenerator()
        manifest = manifest_gen.generate_manifest(repo, parity_report=report)
        assert "Agent Readiness Score" in manifest

        # Step 5: Generate patches
        patch_gen = AutofixPatchGenerator()
        autofixable = detector.get_autofixable_gaps(gaps)
        patches = patch_gen.generate_batch_patches(autofixable, repo)
        assert len(patches) > 0

    def test_exemplar_repo_parity(self, tmp_path):
        """Test exemplar repo achieves high parity"""
        from src.agent_enablement import ParityScorer

        # Create exemplar repo
        repo = tmp_path / "exemplar"
        repo.mkdir()

        # Perfect code
        (repo / "Good.tsx").write_text('<button data-ui="btn">Click</button>')
        (repo / "agent.md").write_text("# Manifest")
        (repo / "FOLDER.md").write_text("# Folder")

        gen = repo / "generated"
        gen.mkdir()
        (gen / "types.ts").write_text("// @generated\nexport type Foo = string;")

        scorer = ParityScorer()
        report = scorer.scan_repository(repo)

        # Should have high score
        assert report.overall_score >= 70
