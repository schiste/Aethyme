#!/usr/bin/env python3
"""Opt-in ref-heavy backend trials in one owned synthetic Playground only."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import shutil
import tempfile

from git_worktree_options import Experiment, distribution


def refs(ex, repo, git=None):
    return ex.run(repo, "for-each-ref", "--format=%(refname) %(objectname)",
                  "refs/heads/", git=git)["stdout"].splitlines()


def measure(root, output, git, other, count, samples):
    ex = Experiment(root, git, output)
    clients = list(dict.fromkeys([git] + ([other] if other else [])))
    report = dict(schema="aethyme.git-ref-backends/v1", platform=platform.platform(),
                  ref_count=count, repetitions=samples, clients=[], cases=[],
                  limitations=[
                      "sequential synthetic warm/first-observation samples, not cold-cache or p99",
                      "all synthetic branch tips share one commit; no history-transfer speedup claim",
                      "no actual broker, IDE, hosted GitHub, network filesystem or old-client qualification",
                      "child CPU excludes persistent services; no production configuration changed",
                  ])
    for client in clients:
        report["clients"].append(dict(path=client, version=ex.run(root, "--version", git=client)["stdout"].strip()))
    for client_index, client in enumerate(clients):
        for variant in ["files-loose", "files-packed", "reftable"]:
            backend = "reftable" if variant == "reftable" else "files"
            name = f"client-{client_index}-{variant}"
            primary = root / name
            primary.mkdir()
            ex.run(primary, "init", "-b", "main", f"--ref-format={backend}", git=client)
            (primary / "README.md").write_text("Synthetic Playground.\n")
            ex.run(primary, "add", "README.md", git=client)
            ex.run(primary, "-c", "maintenance.auto=false", "-c", "gc.auto=0",
                   "commit", "-qm", "synthetic seed", git=client)
            head = ex.run(primary, "rev-parse", "HEAD", git=client)["stdout"].strip()
            names = [f"refs/heads/agent/session-{i:06d}" for i in range(count)]
            transaction = "start\n" + "".join(f"create {name} {head}\n" for name in names) + "prepare\ncommit\n"
            ex.run(primary, "update-ref", "--stdin", git=client, input_text=transaction)
            if variant == "files-packed":
                ex.run(primary, "pack-refs", "--all", git=client)
            expected = sorted([f"refs/heads/main {head}"] + [f"{name} {head}" for name in names])
            assert refs(ex, primary, client) == expected
            for alternate in clients:
                assert refs(ex, primary, alternate) == expected, "alternate reader disagrees"
            mirror = root / f"{name}-fetch-target"
            mirror.mkdir()
            ex.run(mirror, "init", "--bare", "--ref-format=files", git=client)
            fetch_args = ("-c", "maintenance.auto=false", "-c", "gc.auto=0", "fetch",
                          "--no-tags", str(primary), "+refs/heads/*:refs/heads/*")
            initial_fetch = ex.run(mirror, *fetch_args, git=client)
            assert refs(ex, mirror, client) == expected
            timings = {key: [] for key in ["lookup", "enumerate", "fetch_unchanged", "worktree_add", "worktree_remove"]}
            for sample in range(samples):
                row = ex.run(primary, "rev-parse", "--verify", names[count // 2], git=client)
                assert row["stdout"].strip() == head
                timings["lookup"].append(row)
                row = ex.run(primary, "for-each-ref", "--format=%(refname) %(objectname)", "refs/heads/", git=client)
                assert row["stdout"].splitlines() == expected
                timings["enumerate"].append(row)
                timings["fetch_unchanged"].append(ex.run(mirror, *fetch_args, git=client))
                checkout = root / f"{name}-checkout-{sample}"
                timings["worktree_add"].append(ex.run(primary, "-c", "maintenance.auto=false", "worktree", "add", "--detach", str(checkout), head, git=client))
                assert (checkout / "README.md").read_text() == "Synthetic Playground.\n"
                # Assert the linked checkout can resolve branch refs with both clients.
                for alternate in clients:
                    assert ex.run(checkout, "rev-parse", "--verify", names[-1], git=alternate)["stdout"].strip() == head
                timings["worktree_remove"].append(ex.run(primary, "worktree", "remove", str(checkout), git=client))
                assert not checkout.exists()
            bundle = root / f"{name}.bundle"
            ex.run(primary, "bundle", "create", str(bundle), "--all", git=client)
            ex.run(primary, "bundle", "verify", str(bundle), git=client)
            restored = root / f"{name}-restored-files"
            restored.mkdir()
            ex.run(restored, "init", "--bare", "--ref-format=files", git=client)
            ex.run(restored, "-c", "maintenance.auto=false", "fetch", str(bundle), "+refs/heads/*:refs/heads/*", git=client)
            assert refs(ex, restored, client) == expected
            ex.run(restored, "fsck", "--full", git=client)
            # Exercise conversion and rollback only in this owned fixture,
            # after removing every linked checkout. This is not crash recovery.
            other_backend = "files" if backend == "reftable" else "reftable"
            ex.run(primary, "refs", "migrate", f"--ref-format={other_backend}", git=client)
            assert refs(ex, primary, client) == expected
            ex.run(primary, "refs", "migrate", f"--ref-format={backend}", git=client)
            assert refs(ex, primary, client) == expected
            ex.run(primary, "fsck", "--full", git=client)
            report["cases"].append(dict(client=client, backend=backend, variant=variant,
                initial_fetch_ms=initial_fetch["elapsed_ms"],
                timings={key: distribution(rows) for key, rows in timings.items()},
                exact_ref_controls=True, alternate_clients=len(clients),
                bundle_restored_to_files=True, migration_round_trip=True))
    report["commands"] = ex.commands
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--fixture-parent", type=Path, required=True)
    parser.add_argument("--git", type=Path, default=Path("/opt/homebrew/bin/git"))
    parser.add_argument("--other-git", type=Path)
    parser.add_argument("--refs", type=int, default=2000)
    parser.add_argument("--samples", type=int, default=7)
    args = parser.parse_args()
    if not 100 <= args.refs <= 20000 or not 1 <= args.samples <= 21:
        parser.error("refs must be 100..20000 and samples must be 1..21")
    output = args.output
    if not output.is_absolute() or output.exists() or output.is_symlink():
        parser.error("output must be a new absolute directory")
    output.parent.resolve(strict=True)
    parent = args.fixture_parent.resolve(strict=True)
    if not parent.is_dir():
        parser.error("fixture parent must exist")
    git = str(args.git.resolve(strict=True))
    other = str(args.other_git.resolve(strict=True)) if args.other_git else None
    output.mkdir(mode=0o700)
    root = Path(tempfile.mkdtemp(prefix="aethyme-playground-ref-backends-", dir=parent)).resolve()
    report = None
    try:
        report = measure(root, output, git, other, args.refs, args.samples)
        report["harness_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        report["command_runner_sha256"] = hashlib.sha256(Path(__file__).with_name("git_worktree_options.py").read_bytes()).hexdigest()
        report["fixture_root"] = str(root)
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    finally:
        if report is not None:
            shutil.rmtree(root)
        else:
            (output / "retained-fixture.txt").write_text(str(root) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "commands"}, indent=2))


if __name__ == "__main__":
    main()
