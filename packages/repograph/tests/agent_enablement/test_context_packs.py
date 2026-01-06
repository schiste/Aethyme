"""Tests for Context Pack Generator"""

import pytest
import json
from pathlib import Path
from src.agent_enablement.context_packs import ContextPackGenerator, ContextPack


class TestContextPackGenerator:
    """Test context pack generation"""

    @pytest.fixture
    def generator(self):
        return ContextPackGenerator()

    @pytest.fixture
    def full_repo(self, tmp_path):
        """Create repository with all features"""
        repo = tmp_path / "full"
        repo.mkdir()

        # Routes config
        (repo / "routes.config.ts").write_text('''
export const routes = [
  { path: "/home", component: "Home" },
  { path: "/about", component: "About" }
];
        ''')

        # Environment file
        (repo / ".env.example").write_text('''
DATABASE_URL=postgresql://localhost/db
API_KEY=
        ''')

        # Test files
        (repo / "test.spec.ts").write_text('test("example", () => {});')

        # Models
        models_dir = repo / "models"
        models_dir.mkdir()
        (models_dir / "user.py").write_text('''
class User(Model):
    name: str
    email: str
        ''')

        return repo

    def test_generate_pack_all_types(self, generator, full_repo):
        """Test generating all pack types"""
        pack = generator.generate_pack(full_repo)

        assert isinstance(pack, ContextPack)
        assert pack.repo == "full"
        assert pack.version == generator.PACK_VERSION
        assert len(pack.context_packs) == 6

    def test_generate_specific_types(self, generator, full_repo):
        """Test generating specific pack types"""
        pack = generator.generate_pack(
            full_repo,
            pack_types=["routes", "env"]
        )

        assert len(pack.context_packs) == 2
        assert "routes" in pack.context_packs
        assert "env" in pack.context_packs

    def test_routes_pack(self, generator, full_repo):
        """Test routes pack extraction"""
        pack = generator.generate_pack(full_repo, pack_types=["routes"])

        routes_pack = pack.context_packs["routes"]
        assert "routes" in routes_pack
        assert "total_count" in routes_pack

    def test_env_pack(self, generator, full_repo):
        """Test environment pack extraction"""
        pack = generator.generate_pack(full_repo, pack_types=["env"])

        env_pack = pack.context_packs["env"]
        assert "variables" in env_pack
        assert len(env_pack["variables"]) > 0

    def test_tests_pack(self, generator, full_repo):
        """Test tests pack extraction"""
        pack = generator.generate_pack(full_repo, pack_types=["tests"])

        tests_pack = pack.context_packs["tests"]
        assert "test_files" in tests_pack
        assert "frameworks" in tests_pack

    def test_checksum_determinism(self, generator, full_repo):
        """Test checksums are deterministic"""
        pack1 = generator.generate_pack(full_repo)
        pack2 = generator.generate_pack(full_repo)

        assert pack1.checksum == pack2.checksum

    def test_save_and_load_pack(self, generator, full_repo, tmp_path):
        """Test save/load roundtrip"""
        pack = generator.generate_pack(full_repo)
        output = tmp_path / "pack.json"

        generator.save_pack(pack, output, compress=False)
        loaded = generator.load_pack(output)

        assert loaded.checksum == pack.checksum
        assert loaded.repo == pack.repo

    def test_compression(self, generator, full_repo, tmp_path):
        """Test pack compression"""
        pack = generator.generate_pack(full_repo, compress=True)
        output = tmp_path / "pack.json"

        generator.save_pack(pack, output, compress=True)

        # Check file was compressed
        if pack.compression == "gzip":
            assert output.with_suffix(".json.gz").exists()
