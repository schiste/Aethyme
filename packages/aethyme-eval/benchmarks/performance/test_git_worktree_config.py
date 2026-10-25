"""Offline controls for the synthetic Git experiment; no Aethyme binary/build."""
from pathlib import Path
import shutil
import tempfile
import unittest

from git_worktree_config import Experiment, case


class MissingFormatWrite(Experiment):
    """A successful-looking backend write must not bypass the format check."""
    def run(self, repo, *args, **kwargs):
        if args[-2:] == ("core.repositoryformatversion", "1"):
            return {"returncode": 0, "stdout": "", "stderr": ""}
        return super().run(repo, *args, **kwargs)


class WorktreeConfigControls(unittest.TestCase):
    def setUp(self):
        self.git = shutil.which("git")
        self.assertIsNotNone(self.git, "Git is required for these offline controls")
        self.owner = tempfile.TemporaryDirectory(prefix="aethyme config's controls ")
        self.addCleanup(self.owner.cleanup)
        self.root = Path(self.owner.name).resolve()
        self.playground = self.root / "playground"
        self.output = self.root / "receipts"
        self.playground.mkdir()
        self.output.mkdir()

    def experiment(self, kind=Experiment):
        return kind(self.playground, self.git, self.output)

    def assert_layout(self, name, bare=False, explicit=False):
        ex = self.experiment()
        result = case(ex, name, bare, explicit, None)
        for key in ("session_isolation", "new_worktree_full", "rollback_original_bytes",
                    "format_one_refusal_fence", "unknown_extension_refusal_checked"):
            self.assertTrue(result[key], key)
        self.assertEqual(result["legacy_client"], "not_run")
        self.assertEqual(len(result["interruption_checkpoints"]), 4)
        # Inspect actual command receipts as well as the returned observations.
        refusals = [row for row in ex.commands
                    if row["argv"][-3:] == ["rev-parse", "--verify", "HEAD"]]
        self.assertEqual(len(refusals), 3)
        self.assertTrue(all(row["returncode"] != 0 for row in refusals))
        self.assertTrue(all("aethymeunknownrefusalprobe" in row["stderr"].lower()
                            for row in refusals))
        self.assertLessEqual(len(ex.commands), 750)
        self.assertTrue((self.output / "commands.jsonl").is_file())

    def test_normal_primary_refuses_unknown_extension_and_rolls_back(self):
        self.assert_layout("normal")

    def test_explicit_core_worktree_preserves_existing_checkouts(self):
        self.assert_layout("explicit", explicit=True)

    def test_bare_primary_preserves_existing_checkouts(self):
        self.assert_layout("bare", bare=True)

    def test_missing_format_write_is_not_accepted(self):
        ex = self.experiment(MissingFormatWrite)
        with self.assertRaisesRegex(RuntimeError, "lacks the format-one refusal fence"):
            case(ex, "missing-format", False, False, None)

    def test_supported_client_is_not_misreported_as_legacy(self):
        ex = self.experiment()
        with self.assertRaisesRegex(RuntimeError, "did not refuse.*worktreeConfig"):
            case(ex, "modern", False, False, None, legacy=self.git)

    def test_exhausted_command_budget_dispatches_nothing(self):
        ex = self.experiment()
        ex.commands = [{} for _ in range(750)]
        with self.assertRaisesRegex(RuntimeError, "750-command budget"):
            ex.run(self.playground, "--version")
        self.assertFalse((self.output / "commands.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
