#!/usr/bin/env python3
"""Held-out navigation eval: Aethyme Explore vs a plain ripgrep baseline.

For every question in ``questions.jsonl`` this runs

* ``aethyme explore --repo <repo> --request <question> --format answer-json --depth 0``
  and takes the answer, navigation-hint and verification-target paths in
  order, deduplicated;
* a plain-search baseline: the question's non-stopword terms, each searched
  with ``rg -i`` over the repo, files ranked by how many distinct terms they
  hit (ties broken by total match count, then path).

A question scores a top-k hit when any ground-truth path appears in the
method's first k distinct paths. Results go to ``results/<date>.json`` and a
markdown summary next to it.

The question set is held out: read README.md before changing anything here.
Standard library only.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

HELDOUT_ROOT = Path(__file__).resolve().parent
DEFAULT_QUESTIONS = HELDOUT_ROOT / "questions.jsonl"
DEFAULT_RESULTS_DIR = HELDOUT_ROOT / "results"
REPOSITORIES_ROOT = Path.home() / "Repositories"

# Target repositories, keyed by the ``repo`` field of a question. None of them
# is an Aethyme playground or has been used for tuning.
REPOS: dict[str, Path] = {
    "SP42": REPOSITORIES_ROOT / "SP42",
    "aedventure": REPOSITORIES_ROOT / "aedventure",
    "blybot": REPOSITORIES_ROOT / "blybot",
}

EXPLORE_TIMEOUT_SECONDS = 300
RG_TIMEOUT_SECONDS = 120
TOP_KS = (1, 3)

# Deliberately generic English + question-word stopwords. Do not add
# question-specific words here: that would tune the baseline to the set.
STOPWORDS = frozenset(
    """
    a about above after again against all also am an and any are as at be because
    been before being below between both but by can could did do does doing down
    during each else ever every few for from further get gets got had has have
    having he her here hers him his how i if in into is it its itself just let
    like make makes me more most my no nor not now of off on once one only or
    other our out over own same she should so some such than that the their them
    then there these they this those through to too under until up very was way
    we were what when where which while who whom why will with would you your
    want wants need needs know tell thing things someone something actually
    change changes changed happen happens decide decided decides work works done
    read find show shows see use used using i'd i'm it's don't won't what's
    """.split()
)

TERM_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_\-]*")


def load_questions(path: Path) -> list[dict[str, Any]]:
    questions = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                questions.append(json.loads(line))
    return questions


def extract_terms(question: str) -> list[str]:
    """Distinct, lowercased non-stopword terms of length >= 3, in question order."""
    seen: list[str] = []
    for raw in TERM_RE.findall(question.replace("/", " ")):
        term = raw.strip("-_").lower()
        if len(term) < 3 or term in STOPWORDS or term.isdigit():
            continue
        if term not in seen:
            seen.append(term)
    return seen


def normalize_path(raw: str, repo: Path) -> str:
    path = raw.strip()
    path = re.sub(r":\d+(?:-\d+)?(?::\d+)?$", "", path)
    if path.startswith(str(repo)):
        path = os.path.relpath(path, repo)
    if path.startswith("./"):
        path = path[2:]
    return path


def _item_path(item: Any) -> str | None:
    if not isinstance(item, dict):
        return None
    for key in ("path", "file", "file_path"):
        value = item.get(key)
        if isinstance(value, str) and value:
            return value
    location = item.get("location")
    if isinstance(location, dict):
        return _item_path(location)
    return None


def explore_paths(payload: dict[str, Any], repo: Path) -> list[str]:
    """Ordered, deduplicated paths: answers, navigation hints, then targets."""
    ordered: list[str] = []

    def push(item: Any) -> None:
        raw = _item_path(item)
        if raw:
            path = normalize_path(raw, repo)
            if path not in ordered:
                ordered.append(path)

    for item in payload.get("answer") or []:
        push(item)
    for item in payload.get("navigation_hints") or []:
        push(item)
    subsystems = sorted(
        payload.get("subsystems") or [], key=lambda subsystem: subsystem.get("rank", 1 << 30)
    )
    for subsystem in subsystems:
        for item in subsystem.get("top_verification_targets") or []:
            push(item)
    return ordered


def run_explore(aethyme: str, repo: Path, question: str) -> dict[str, Any]:
    command = [
        aethyme,
        "explore",
        "--repo",
        str(repo),
        "--request",
        question,
        "--format",
        "answer-json",
        "--depth",
        "0",
    ]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=EXPLORE_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return {
            "wall_seconds": round(time.monotonic() - started, 3),
            "exit_code": None,
            "error": "timeout",
            "paths": [],
        }
    wall = round(time.monotonic() - started, 3)
    result: dict[str, Any] = {"wall_seconds": wall, "exit_code": completed.returncode}
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError:
        result.update(
            {
                "error": "unparseable_output",
                "stderr_tail": completed.stderr[-2000:],
                "paths": [],
            }
        )
        return result
    readiness = (payload.get("observability") or {}).get("readiness") or {}
    result.update(
        {
            "status": payload.get("status"),
            "intent": payload.get("intent"),
            "degraded_reasons": payload.get("degraded_reasons"),
            "safe_to_use_as_answer": payload.get("safe_to_use_as_answer"),
            "readiness": readiness,
            "source_fallback": (payload.get("observability") or {}).get("source_fallback"),
            "answer_count": len(payload.get("answer") or []),
            "paths": explore_paths(payload, repo),
        }
    )
    return result


def run_rg_baseline(rg: str, repo: Path, question: str) -> dict[str, Any]:
    terms = extract_terms(question)
    distinct_hits: dict[str, int] = {}
    total_matches: dict[str, int] = {}
    started = time.monotonic()
    for term in terms:
        completed = subprocess.run(
            [rg, "-n", "-i", "-c", "-F", "--", term, "."],
            cwd=repo,
            capture_output=True,
            text=True,
            timeout=RG_TIMEOUT_SECONDS,
            check=False,
        )
        for line in completed.stdout.splitlines():
            path, _, count = line.rpartition(":")
            if not path or not count.isdigit():
                continue
            path = normalize_path(path, repo)
            distinct_hits[path] = distinct_hits.get(path, 0) + 1
            total_matches[path] = total_matches.get(path, 0) + int(count)
    wall = round(time.monotonic() - started, 3)
    ranked = sorted(distinct_hits, key=lambda p: (-distinct_hits[p], -total_matches[p], p))
    return {
        "wall_seconds": wall,
        "terms": terms,
        "paths": ranked[:25],
        "top_scores": [[p, distinct_hits[p], total_matches[p]] for p in ranked[:5]],
        "files_matched": len(ranked),
    }


def score(paths: list[str], truth: set[str]) -> dict[str, Any]:
    first = next((index + 1 for index, path in enumerate(paths) if path in truth), None)
    scored: dict[str, Any] = {f"top{k}": first is not None and first <= k for k in TOP_KS}
    scored["first_hit_rank"] = first
    return scored


def snapshot_repo(repo: Path) -> dict[str, Any]:
    """What an eval run could have written: git status and the .aethyme tree."""
    status = subprocess.run(
        ["/usr/bin/git", "-C", str(repo), "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.splitlines()
    head = subprocess.run(
        ["/usr/bin/git", "-C", str(repo), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    aethyme_dir = repo / ".aethyme"
    files: dict[str, float] = {}
    if aethyme_dir.is_dir():
        for path in aethyme_dir.rglob("*"):
            if path.is_file():
                try:
                    files[str(path.relative_to(repo))] = path.stat().st_mtime
                except OSError:
                    continue
    return {
        "head": head,
        "aethyme_dir_exists": aethyme_dir.is_dir(),
        "aethyme_files": files,
        "status": sorted(status),
    }


def diff_snapshots(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    new_files = sorted(set(after["aethyme_files"]) - set(before["aethyme_files"]))
    modified = sorted(
        path
        for path, mtime in after["aethyme_files"].items()
        if path in before["aethyme_files"] and before["aethyme_files"][path] != mtime
    )
    return {
        "aethyme_dir_created": after["aethyme_dir_exists"] and not before["aethyme_dir_exists"],
        "new_aethyme_files": new_files,
        "modified_aethyme_files": modified,
        "new_status_entries": sorted(set(after["status"]) - set(before["status"])),
    }


def summarize(records: list[dict[str, Any]]) -> dict[str, Any]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for record in records:
        groups.setdefault(record["repo"], []).append(record)
    groups["total"] = records
    summary: dict[str, Any] = {}
    for name, rows in groups.items():
        entry: dict[str, Any] = {"questions": len(rows)}
        for method in ("explore", "rg"):
            for k in TOP_KS:
                entry[f"{method}_top{k}"] = sum(1 for row in rows if row[method]["score"][f"top{k}"])
            times = [row[method]["wall_seconds"] for row in rows]
            entry[f"{method}_median_seconds"] = round(statistics.median(times), 3) if times else None
        summary[name] = entry
    return summary


def render_markdown(result: dict[str, Any]) -> str:
    lines = [
        f"# Held-out navigation eval — {result['date']}",
        "",
        f"- aethyme: `{result['aethyme_version']}`",
        f"- rg: `{result['rg_version']}`",
        f"- questions: {len(result['records'])} (`{result['questions_file']}`)",
        "",
        "| Repo | N | Explore top-1 | Explore top-3 | rg top-1 | rg top-3 | Explore median s | rg median s |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for name, entry in result["summary"].items():
        n = entry["questions"]
        lines.append(
            f"| {name} | {n} | {entry['explore_top1']}/{n} | {entry['explore_top3']}/{n} | "
            f"{entry['rg_top1']}/{n} | {entry['rg_top3']}/{n} | "
            f"{entry['explore_median_seconds']} | {entry['rg_median_seconds']} |"
        )
    lines += [
        "",
        "## Per question",
        "",
        "| Id | Kind | Explore status | Explore rank | rg rank | Explore first path | rg first path |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for record in result["records"]:
        explore, rg = record["explore"], record["rg"]
        lines.append(
            f"| {record['id']} | {record['kind']} | {explore.get('status') or explore.get('error')} | "
            f"{explore['score']['first_hit_rank'] or '-'} | {rg['score']['first_hit_rank'] or '-'} | "
            f"`{(explore['paths'] or ['-'])[0]}` | `{(rg['paths'] or ['-'])[0]}` |"
        )
    lines += ["", "## Target-repo writes during the run", ""]
    for repo, delta in result["repo_writes"].items():
        lines.append(f"- {repo}: `{json.dumps(delta, sort_keys=True)}`")
    lines.append("")
    return "\n".join(lines)


def tool_version(command: list[str]) -> str:
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    return (completed.stdout or completed.stderr).strip().splitlines()[0] if completed else ""


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--questions", type=Path, default=DEFAULT_QUESTIONS)
    parser.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS_DIR)
    parser.add_argument("--aethyme", default=shutil.which("aethyme") or "aethyme")
    parser.add_argument(
        "--rg",
        default=os.environ.get("AETHYME_HELDOUT_RG") or shutil.which("rg") or "rg",
        help="ripgrep binary (set AETHYME_HELDOUT_RG to bypass a PATH shim)",
    )
    parser.add_argument("--only", action="append", default=[], help="question id(s) to run")
    parser.add_argument("--date", default=dt.date.today().isoformat())
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    questions = load_questions(args.questions)
    if args.only:
        questions = [question for question in questions if question["id"] in set(args.only)]
    repos_used = sorted({question["repo"] for question in questions})
    for name in repos_used:
        if not REPOS[name].is_dir():
            print(f"missing target repository: {REPOS[name]}", file=sys.stderr)
            return 2

    before = {name: snapshot_repo(REPOS[name]) for name in repos_used}
    records = []
    for question in questions:
        repo = REPOS[question["repo"]]
        truth = {answer["path"] for answer in question["answers"]}
        explore = run_explore(args.aethyme, repo, question["question"])
        explore["score"] = score(explore["paths"], truth)
        explore["paths"] = explore["paths"][:10]
        rg = run_rg_baseline(args.rg, repo, question["question"])
        rg["score"] = score(rg["paths"], truth)
        rg["paths"] = rg["paths"][:10]
        records.append(
            {
                "id": question["id"],
                "repo": question["repo"],
                "kind": question["kind"],
                "truth_paths": sorted(truth),
                "explore": explore,
                "rg": rg,
            }
        )
        print(
            f"{question['id']}: explore rank={explore['score']['first_hit_rank']} "
            f"({explore['wall_seconds']}s, {explore.get('status')}) "
            f"rg rank={rg['score']['first_hit_rank']} ({rg['wall_seconds']}s)",
            file=sys.stderr,
        )
    after = {name: snapshot_repo(REPOS[name]) for name in repos_used}

    result = {
        "schema": "aethyme-heldout-nav-v1",
        "date": args.date,
        "aethyme_version": tool_version([args.aethyme, "--version"]),
        "rg_version": tool_version([args.rg, "--version"]),
        "questions_file": str(args.questions.relative_to(HELDOUT_ROOT))
        if args.questions.is_relative_to(HELDOUT_ROOT)
        else str(args.questions),
        "repo_heads": {name: before[name]["head"] for name in repos_used},
        "repo_writes": {name: diff_snapshots(before[name], after[name]) for name in repos_used},
        "summary": summarize(records),
        "records": records,
    }
    args.results_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.results_dir / f"{args.date}.json"
    md_path = args.results_dir / f"{args.date}.md"
    json_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(render_markdown(result), encoding="utf-8")
    print(render_markdown(result))
    print(f"wrote {json_path} and {md_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
