#!/usr/bin/env python3
"""Opt-in, offline worktreeConfig controls; creates only synthetic Playgrounds.

This is a quiescent migration experiment, not a production migration utility.
No existing repository path is accepted. No monitor, model or package install.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile

from git_worktree_options import Experiment as GitExperiment

class Experiment(GitExperiment):
    def run(self, *args, **kwargs):
        if len(self.commands) >= 600:
            raise RuntimeError("synthetic experiment exhausted its 600-command budget")
        return super().run(*args, **kwargs)


KEYS = ("core.bare", "core.worktree", "core.sparseCheckout")


def value(ex, repo, key, git=None):
    row = ex.run(repo, "config", "--get", key, git=git, required=False)
    if row["returncode"] not in (0, 1):
        raise RuntimeError("configuration read failed")
    return row["stdout"].strip() if row["returncode"] == 0 else None


def config_path(ex, repo):
    path = Path(ex.run(repo, "rev-parse", "--absolute-git-dir")["stdout"].strip())
    if not path.resolve(strict=True).is_relative_to(ex.root):
        raise RuntimeError("Git directory escaped Playground")
    return path / "config.worktree"


def snapshot(ex, repos, git=None):
    return [{"bare": ex.run(p, "rev-parse", "--is-bare-repository", git=git)["stdout"].strip(),
             "head": ex.run(p, "rev-parse", "HEAD", git=git)["stdout"].strip(),
             "keys": {key: value(ex, p, key, git) for key in KEYS}}
            for p in repos]


def case(ex, name, bare, explicit_worktree, other):
    primary = ex.init(name, ("--bare",) if bare else ())
    # A non-bare seed is needed to create a commit for the bare primary.
    seed = ex.init(name + "-seed") if bare else primary
    (seed / "input.txt").write_text("synthetic input\n")
    ex.run(seed, "add", "input.txt")
    ex.run(seed, "commit", "-qm", "synthetic base")
    if bare:
        ex.run(primary, "fetch", str(seed), "main:main")
    if explicit_worktree:
        ex.run(primary, "config", "core.worktree", str(primary))
    # False is still an explicitly shared setting whose placement must survive.
    ex.run(primary, "config", "core.sparseCheckout", "false")
    # With one worktree, --worktree aliases --local before extension enable.
    ex.run(primary, "config", "--worktree", "aethymeFixture.beforeEnable", "shared")
    observed = ex.run(primary, "config", "--local", "--get", "aethymeFixture.beforeEnable")
    if observed["stdout"].strip() != "shared":
        raise RuntimeError("single-worktree pre-extension placement differed")
    ex.run(primary, "config", "--local", "--unset", "aethymeFixture.beforeEnable")
    linked = [ex.root / (name + "-linked-" + str(i)) for i in range(2)]
    for p in linked:
        ex.run(primary, "worktree", "add", "--detach", str(p), "HEAD")
    repos = [primary, *linked]
    # With existing linked checkouts, installed clients refuse this spelling.
    refused = ex.run(linked[0], "config", "--worktree", "aethymeFixture.beforeEnable",
                     "must-not-write", required=False)
    if refused["returncode"] == 0 or "worktreeConfig" not in refused["stderr"]:
        raise RuntimeError("expected explicit pre-extension multi-worktree refusal")
    if value(ex, primary, "aethymeFixture.beforeEnable") is not None:
        raise RuntimeError("refused config write changed shared settings")
    before = snapshot(ex, repos)
    common = Path(ex.run(primary, "rev-parse", "--git-common-dir")["stdout"].strip())
    if not common.is_absolute():
        common = primary / common
    common = common.resolve(strict=True)
    if not common.is_relative_to(ex.root):
        raise RuntimeError("shared config escaped Playground")
    shared = common / "config"
    original = shared.read_bytes()
    staged_shared = common / "config.experiment-staged"
    staged_shared.write_bytes(original)
    for key in KEYS:
        row = ex.run(primary, "config", "--file", str(staged_shared), "--unset-all", key,
                     required=False)
        if row["returncode"] not in (0, 5):
            raise RuntimeError("could not remove staged shared key")
    ex.run(primary, "config", "--file", str(staged_shared), "extensions.worktreeConfig", "true")
    stages = []
    for i, repo in enumerate(repos):
        target = config_path(ex, repo)
        if target.exists():
            raise RuntimeError("unexpected existing per-worktree config")
        # Linked checkouts must not inherit the primary's bare/worktree values.
        values = {"core.bare": "true" if bare and i == 0 else "false",
                  "core.sparseCheckout": "false"}
        if explicit_worktree and i == 0:
            values["core.worktree"] = str(primary)
        for key, setting in values.items():
            ex.run(primary, "config", "--file", str(target), key, setting)
        # Simulate interruption after each prepared file: extension is still off.
        if snapshot(ex, repos) != before:
            raise RuntimeError("staging changed pre-migration semantics")
        stages.append("staged_" + str(i) + "_old_semantics_preserved")
    os.replace(staged_shared, shared)
    after = snapshot(ex, repos)
    # Effective core.bare differs for linked worktrees (now explicit false), but
    # repository identity and heads must be unchanged, including a bare primary.
    for old, new in zip(before, after):
        if (old["bare"], old["head"]) != (new["bare"], new["head"]):
            raise RuntimeError("migration changed repository identity")
    for p in linked:
        if ex.run(p, "status", "--porcelain=v1")["stdout"]:
            raise RuntimeError("linked checkout became dirty")
    ex.run(linked[0], "config", "--worktree", "aethymeFixture.scope", "session-zero")
    if value(ex, linked[0], "aethymeFixture.scope") != "session-zero":
        raise RuntimeError("worktree-local value missing")
    for p in (primary, linked[1]):
        if value(ex, p, "aethymeFixture.scope") is not None:
            raise RuntimeError("session setting leaked to sibling")
    fresh = ex.root / (name + "-fresh")
    ex.run(primary, "worktree", "add", "--detach", str(fresh), "HEAD")
    if value(ex, fresh, "aethymeFixture.scope") is not None:
        raise RuntimeError("session setting leaked into new worktree")
    if ex.run(fresh, "status", "--porcelain=v1")["stdout"]:
        raise RuntimeError("new checkout became dirty")
    alternate = None
    if other:
        if snapshot(ex, repos, other) != after:
            raise RuntimeError("alternate client configuration disagreed")
        for p in (*linked, fresh):
            ex.run(p, "update-index", "--refresh", git=other)
            if ex.run(p, "status", "--porcelain=v1")["stdout"]:
                raise RuntimeError("alternate client index write changed status")
        alternate = "read_and_index_write_passed"
    # Quiescent rollback discards the synthetic session-only setting explicitly.
    # It is not a safe automatic rollback once real per-session settings exist.
    rollback = common / "config.experiment-rollback"
    rollback.write_bytes(original)
    os.replace(rollback, shared)
    if snapshot(ex, repos) != before:
        raise RuntimeError("rollback did not restore original semantics")
    for p in repos:
        config_path(ex, p).unlink()
    if shared.read_bytes() != original:
        raise RuntimeError("rollback changed original shared config bytes")
    return dict(before_extension_single_worktree_writes_shared=True, multi_worktree_write_refused=True, primary_bare=bare, explicit_primary_worktree=explicit_worktree,
                interruption_checkpoints=stages + ["after_atomic_enable"],
                before=before, after=after, session_isolation=True,
                new_worktree_full=True, alternate_client=alternate,
                rollback_original_bytes=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-parent", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--git", default="/opt/homebrew/bin/git")
    parser.add_argument("--other-git")
    args = parser.parse_args()
    parent = args.fixture_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("fixture parent must be an existing directory")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix="aethyme-config-playground-", dir=parent)).resolve()
    (output / "fixture.json").write_text(json.dumps({"owned_playground": str(root)}))
    ex = Experiment(root, args.git, output)
    try:
        report = dict(schema="aethyme.git-worktree-config/v1",
                      harness_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      git=ex.run(root, "--version")["stdout"].strip(),
                      other_git=ex.run(root, "--version", git=args.other_git)["stdout"].strip()
                      if args.other_git else None,
                      cases=[case(ex, "nonbare", False, False, args.other_git),
                             case(ex, "explicit-worktree", False, True, args.other_git),
                             case(ex, "bare", True, False, args.other_git)],
                      limitations=["quiescent synthetic experiment, not live migration",
                                   "interruption checkpoints, not power-loss durability",
                                   "no old-client, IDE, CI or GitHub qualification",
                                   "no production config change or performance claim"])
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    except BaseException:
        print("Failed; owned fixture retained:", root)
        raise
    shutil.rmtree(root)
    (output / "cleanup.json").write_text(json.dumps({"removed_owned_playground": str(root)}))
    print(json.dumps({"output": str(output), "cases": len(report["cases"]),
                      "commands": len(ex.commands), "fixture_removed": True}))


if __name__ == "__main__":
    main()
