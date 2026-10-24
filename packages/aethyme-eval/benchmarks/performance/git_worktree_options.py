#!/usr/bin/env python3
"""Opt-in bounded Git experiments in owned synthetic Playgrounds only.

No Aethyme source, package installation, network, provider, or production config
is read or modified. First observations are not cold-cache measurements.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import statistics
import subprocess
import tempfile
import time

FILES = 1800
SAMPLES = 7


class Experiment:
    def __init__(self, root, git, output):
        self.root = root
        self.git = git
        self.commands = []
        self.output = output
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("GIT_", "AETHYME_"))}
        home = root / "home"
        home.mkdir()
        self.env.update(HOME=str(home), XDG_CONFIG_HOME=str(home),
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Playground", GIT_AUTHOR_EMAIL="fixture@example.invalid",
                        GIT_COMMITTER_NAME="Playground",
                        GIT_COMMITTER_EMAIL="fixture@example.invalid",
                        GIT_TERMINAL_PROMPT="0")

    def run(self, repo, *args, git=None, required=True):
        repo = repo.resolve(strict=True)
        if not repo.is_relative_to(self.root):
            raise ValueError("command escaped the owned Playground")
        argv = [git or self.git, "-C", str(repo), *args]
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        start = time.perf_counter_ns()
        result = subprocess.run(argv, env=self.env, capture_output=True, timeout=20)
        elapsed = (time.perf_counter_ns() - start) / 1e6
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        receipt = dict(argv=argv, returncode=result.returncode, elapsed_ms=elapsed,
                       child_cpu_ms=1000 * (after.ru_utime + after.ru_stime
                                            - before.ru_utime - before.ru_stime),
                       stdout=result.stdout.decode("utf-8", errors="replace"),
                       stderr=result.stderr.decode("utf-8", errors="replace"))
        self.commands.append(receipt)
        with (self.output / "commands.jsonl").open("a") as stream:
            stream.write(json.dumps(receipt) + "\n")
        if required and result.returncode:
            raise RuntimeError(json.dumps(receipt))
        return receipt

    def init(self, name, extra=()):
        repo = self.root / name
        repo.mkdir()
        self.run(repo, "init", "-b", "main", *extra)
        return repo


def distribution(rows):
    values = sorted(row["elapsed_ms"] for row in rows)
    return dict(samples_ms=[row["elapsed_ms"] for row in rows],
                median_ms=statistics.median(values), max_ms=max(values),
                child_cpu_ms=[row["child_cpu_ms"] for row in rows])


def run(root, git, other, output):
    ex = Experiment(root, git, output)
    report = dict(schema="aethyme.git-worktree-options/v1", platform=platform.platform(),
                  files=FILES, repetitions=SAMPLES,
                  limitations=["first observation is not cold-cache",
                               "small synthetic Git-only workload; no broker or gate speedup claim",
                               "fsmonitor daemon CPU/RSS is not included in child CPU samples",
                               "local supported-client probes do not prove all IDE/CI clients"],
                  git=ex.run(root, "--version")["stdout"].strip())
    if other:
        report["other_git"] = ex.run(root, "--version", git=other)["stdout"].strip()
    variants = {}
    for mode in ("baseline", "many-files", "fsmonitor"):
        repo = ex.init(mode)
        try:
            (repo / ".gitignore").write_text("target/\n")
            for i in range(FILES):
                target = repo / f"package-{i % 12}" / f"file-{i}.txt"
                target.parent.mkdir(exist_ok=True)
                target.write_text(f"synthetic tracked file {i}\n")
            ex.run(repo, "add", ".")
            ex.run(repo, "commit", "-qm", "synthetic baseline")
            if mode == "many-files":
                ex.run(repo, "config", "feature.manyFiles", "true")
                ex.run(repo, "update-index", "--index-version", "4")
            if mode == "fsmonitor":
                ex.run(repo, "config", "core.fsmonitor", "true")
            first = ex.run(repo, "status", "--porcelain=v1")
            states = {}
            for state in ("clean", "dirty", "untracked-and-ignored"):
                if state == "dirty":
                    (repo / "package-0/file-0.txt").write_text("changed\n")
                if state == "untracked-and-ignored":
                    for i in range(200):
                        (repo / f"untracked-{i}.txt").write_text("untracked\n")
                    ignored = repo / "target"
                    ignored.mkdir()
                    for i in range(200):
                        (ignored / f"ignored-{i}").write_bytes(b"x" * 1024)
                rows = [ex.run(repo, "status", "--porcelain=v1")
                        for _ in range(SAMPLES)]
                outputs = {row["stdout"] for row in rows}
                if len(outputs) != 1:
                    raise RuntimeError("status changed during a stable state")
                states[state] = dict(timing=distribution(rows),
                                     status_sha256=hashlib.sha256(rows[0]["stdout"].encode()).hexdigest(),
                                     status_lines=len(rows[0]["stdout"].splitlines()))
                if state != "clean" and "file-0.txt" not in rows[0]["stdout"]:
                    raise RuntimeError("tracked edit was not detected")
                if state == "untracked-and-ignored":
                    if "untracked-199.txt" not in rows[0]["stdout"] or "target/" in rows[0]["stdout"]:
                        raise RuntimeError("untracked/ignored status was incorrect")
            write = ex.run(repo, "add", "package-0/file-0.txt")
            if other:
                before = ex.run(repo, "status", "--porcelain=v1")["stdout"]
                compatibility = ex.run(repo, "status", "--porcelain=v1", git=other)
                if compatibility["stdout"] != before:
                    raise RuntimeError("alternate client status disagreed")
                ex.run(repo, "update-index", "--refresh", git=other)
                if ex.run(repo, "status", "--porcelain=v1")["stdout"] != before:
                    raise RuntimeError("alternate client index write changed status")
            cache_probe = ex.run(repo, "update-index", "--test-untracked-cache", required=False)
            variants[mode] = dict(first_observation_ms=first["elapsed_ms"], states=states,
                                  index_write_ms=write["elapsed_ms"],
                                  index_bytes=(repo / ".git/index").stat().st_size,
                                  untracked_cache_test_returncode=cache_probe["returncode"])
        finally:
            if mode == "fsmonitor":
                # Exact fixture only; never signal arbitrary host processes.
                ex.run(repo, "fsmonitor--daemon", "stop", required=False)
                status = ex.run(repo, "fsmonitor--daemon", "status", required=False)
                if status["returncode"] == 0:
                    raise RuntimeError("owned fixture monitor is still running")
    for state in ("clean", "dirty", "untracked-and-ignored"):
        if len({v["states"][state]["status_sha256"] for v in variants.values()}) != 1:
            raise RuntimeError(f"Git options changed status semantics: {state}")
    report["variants"] = variants

    # A concrete worktreeConfig consumer and a full-checkout fallback smoke.
    repo = ex.init("sparse-control")
    for name in ("package/src.txt", "outside/gate-input.txt"):
        path = repo / name
        path.parent.mkdir(exist_ok=True)
        path.write_text("required fixture input\n")
    ex.run(repo, "add", ".")
    ex.run(repo, "commit", "-qm", "synthetic sparse control")
    linked = root / "sparse-linked"
    ex.run(repo, "worktree", "add", "--detach", str(linked), "HEAD")
    ex.run(linked, "sparse-checkout", "set", "--cone", "package")
    missing = not (linked / "outside/gate-input.txt").exists()
    primary_unchanged = (repo / "outside/gate-input.txt").is_file()
    extension = ex.run(repo, "config", "--get", "extensions.worktreeConfig")
    ex.run(linked, "sparse-checkout", "disable")
    restored = (linked / "outside/gate-input.txt").is_file()
    if not (missing and primary_unchanged and restored):
        raise RuntimeError("sparse isolation or full-checkout recovery failed")
    report["sparse_control"] = dict(outside_gate_input_missing=missing,
                                    primary_unchanged=primary_unchanged,
                                    full_checkout_restored=restored,
                                    worktree_config_extension=extension["stdout"].strip())
    ex.run(repo, "worktree", "remove", str(linked))

    # Reftable capability/recovery smoke, not an existing-repository migration.
    reftable = ex.init("reftable", ("--ref-format=reftable",))
    ex.run(reftable, "commit", "--allow-empty", "-qm", "synthetic reftable")
    if other:
        report["reftable_alternate_client"] = ex.run(
            reftable, "rev-parse", "--verify", "HEAD", git=other, required=False)
    bundle = root / "reftable.bundle"
    ex.run(reftable, "bundle", "create", str(bundle), "--all")
    report["reftable_bundle_verified"] = ex.run(
        reftable, "bundle", "verify", str(bundle))["returncode"] == 0
    report["commands"] = ex.commands
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--fixture-parent", type=Path, required=True)
    parser.add_argument("--git", type=Path, default=Path("/opt/homebrew/bin/git"))
    parser.add_argument("--other-git", type=Path)
    args = parser.parse_args()
    output = args.output
    if not output.is_absolute() or output.exists() or output.is_symlink():
        parser.error("output must be a new absolute directory")
    output.parent.resolve(strict=True)
    parent = args.fixture_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("fixture parent must exist")
    output.mkdir(mode=0o700)
    root = Path(tempfile.mkdtemp(prefix="aethyme-playground-git-options-", dir=parent)).resolve()
    report = None
    try:
        report = run(root, str(args.git.resolve(strict=True)),
                     str(args.other_git.resolve(strict=True)) if args.other_git else None, output)
        report["harness_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        report["fixture_root"] = str(root)
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    finally:
        # Only the exact directory created above, never a supplied repository.
        if report is not None:
            shutil.rmtree(root)
        else:
            (output / "retained-fixture.txt").write_text(str(root) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "commands"}, indent=2))


if __name__ == "__main__":
    main()
