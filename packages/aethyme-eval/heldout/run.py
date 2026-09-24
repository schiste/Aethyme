"""Navigation eval: Aethyme Explore vs two ripgrep baselines.

Used for both the development set (``devset/``) and any held-out set. For
every question in a questions file this runs

* ``aethyme explore --repo <repo> --request <question> --format answer-json
  --depth 0 --show-observability`` and takes the answer, navigation-hint and
  verification-target paths in order, deduplicated;
* ``rg`` (naive): the question's non-stopword terms, each searched with
  ``rg -i -c -F`` over the repo, files ranked by how many distinct terms they
  hit (ties broken by total matching lines, then path);
* ``bm25``: the same terms searched with ``rg -i --count-matches -F``, files
  ranked by Okapi BM25 (term-frequency saturation ``k1``, document-length
  normalization ``b``, IDF over every file ``rg --files`` lists in the repo).

A question scores recall@k when any accepted answer path is among a method's
first k distinct paths, and primary recall@k when the answer flagged
``"primary": true`` is (a missing flag means true). MRR uses the first
accepted path within the first ``MAX_RANK`` paths. Every proportion carries a
95% Wilson interval; MRR carries a 95% percentile-bootstrap interval.

Repository roots come from, in order: ``--repo-root NAME=PATH``, a question's
own ``repo_root``, a ``repos.json`` (``{"name": "path"}``) next to the
questions file or given with ``--repos-json``, then ``~/Repositories/<name>``.

Results go to ``<results-dir>/<date>[-<label>].json`` with a markdown summary
next to it. Standard library only; read the README next to the questions
file before changing a question set.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

HELDOUT_ROOT = Path(__file__).resolve().parent
DEFAULT_QUESTIONS = HELDOUT_ROOT / "questions.jsonl"
REPOSITORIES_ROOT = Path.home() / "Repositories"

EXPLORE_TIMEOUT_SECONDS = 300
RG_TIMEOUT_SECONDS = 120
TOP_KS = (1, 3, 5, 8)
MAX_RANK = 25
KEPT_PATHS = 10
Z95 = 1.959963984540054
BOOTSTRAP_RESAMPLES = 2000
BM25_K1 = 1.2
BM25_B = 0.75
DOC_READ_LIMIT = 2 * 1024 * 1024
ANSWER_RULE_BAR = 0.95

METHODS: dict[str, str] = {
    "explore": "Explore",
    "rg": "naive rg",
    "bm25": "BM25 rg",
}

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
TOKEN_RE = re.compile(rb"[A-Za-z0-9_]+")


# --------------------------------------------------------------------------- inputs


def load_questions(path: Path) -> list[dict[str, Any]]:
    questions = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                questions.append(json.loads(line))
    return questions


def answer_is_primary(answer: dict[str, Any]) -> bool:
    return answer.get("primary", True) is not False


def resolve_repo_roots(
    questions: list[dict[str, Any]], questions_file: Path, repos_json: Path | None, flags: list[str]
) -> dict[str, Path]:
    """Map every repo name in ``questions`` to a root directory."""
    mapping: dict[str, Path] = {}
    json_path = repos_json or questions_file.parent / "repos.json"
    if json_path.is_file():
        for name, raw in json.loads(json_path.read_text(encoding="utf-8")).items():
            root = Path(os.path.expanduser(raw))
            mapping[name] = root if root.is_absolute() else json_path.parent / root
    for question in questions:
        raw = question.get("repo_root")
        if raw:
            mapping[question["repo"]] = Path(os.path.expanduser(raw))
    for flag in flags:
        name, sep, raw = flag.partition("=")
        if not sep or not name or not raw:
            raise SystemExit(f"--repo-root expects NAME=PATH, got {flag!r}")
        mapping[name] = Path(os.path.expanduser(raw))
    for question in questions:
        mapping.setdefault(question["repo"], REPOSITORIES_ROOT / question["repo"])
    return mapping


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


# --------------------------------------------------------------------------- explore


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


def answer_rule(observability: dict[str, Any]) -> dict[str, Any] | None:
    """The source fallback's answer-safety verdict, reported even when not promoted."""
    fallback = observability.get("source_fallback")
    if not isinstance(fallback, dict):
        return None
    safety = fallback.get("answer_safety")
    if not isinstance(safety, dict):
        return None
    return {"safe": bool(safety.get("safe")), "rule": safety.get("rule")}


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
        "--show-observability",
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
    observability = payload.get("observability") or {}
    fallback = observability.get("source_fallback") or {}
    result.update(
        {
            "status": payload.get("status"),
            "intent": payload.get("intent"),
            "degraded_reasons": payload.get("degraded_reasons"),
            "safe_to_use_as_answer": payload.get("safe_to_use_as_answer"),
            "readiness": observability.get("readiness") or {},
            "source_fallback": {
                key: fallback.get(key)
                for key in ("listed_files", "scanned_files", "complete", "reason", "terms")
            }
            if fallback
            else None,
            "answer_safety": answer_rule(observability),
            "answer_count": len(payload.get("answer") or []),
            "paths": explore_paths(payload, repo),
        }
    )
    return result


# --------------------------------------------------------------------------- baselines


def _rg_counts(rg: str, repo: Path, term: str, flag: str) -> dict[str, int]:
    completed = subprocess.run(
        [rg, "-i", flag, "-F", "--", term, "."],
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=RG_TIMEOUT_SECONDS,
        check=False,
    )
    counts: dict[str, int] = {}
    for line in completed.stdout.splitlines():
        path, _, count = line.rpartition(":")
        if path and count.isdigit():
            counts[normalize_path(path, repo)] = int(count)
    return counts


def run_rg_baseline(rg: str, repo: Path, question: str) -> dict[str, Any]:
    terms = extract_terms(question)
    distinct_hits: dict[str, int] = {}
    total_matches: dict[str, int] = {}
    started = time.monotonic()
    for term in terms:
        for path, count in _rg_counts(rg, repo, term, "-c").items():
            distinct_hits[path] = distinct_hits.get(path, 0) + 1
            total_matches[path] = total_matches.get(path, 0) + count
    wall = round(time.monotonic() - started, 3)
    ranked = sorted(distinct_hits, key=lambda p: (-distinct_hits[p], -total_matches[p], p))
    return {
        "wall_seconds": wall,
        "terms": terms,
        "paths": ranked[:MAX_RANK],
        "top_scores": [[p, distinct_hits[p], total_matches[p]] for p in ranked[:5]],
        "files_matched": len(ranked),
    }


class Corpus:
    """Token lengths of every file ``rg --files`` lists, for BM25 normalization."""

    def __init__(self, rg: str, repo: Path) -> None:
        started = time.monotonic()
        listed = subprocess.run(
            [rg, "--files", "."],
            cwd=repo,
            capture_output=True,
            text=True,
            timeout=RG_TIMEOUT_SECONDS,
            check=False,
        ).stdout.splitlines()
        self.lengths: dict[str, int] = {}
        for raw in listed:
            path = normalize_path(raw, repo)
            try:
                with (repo / path).open("rb") as handle:
                    data = handle.read(DOC_READ_LIMIT)
            except OSError:
                continue
            if b"\0" in data[:8192]:
                continue  # rg skips binary files when searching; so does the corpus
            self.lengths[path] = max(1, len(TOKEN_RE.findall(data)))
        self.documents = len(self.lengths)
        self.average_length = sum(self.lengths.values()) / self.documents if self.documents else 1.0
        self.build_seconds = round(time.monotonic() - started, 3)


def run_bm25_baseline(rg: str, repo: Path, question: str, corpus: Corpus) -> dict[str, Any]:
    terms = extract_terms(question)
    scores: dict[str, float] = {}
    started = time.monotonic()
    n = corpus.documents
    for term in terms:
        counts = _rg_counts(rg, repo, term, "--count-matches")
        df = sum(1 for path in counts if path in corpus.lengths)
        if not df:
            continue
        idf = math.log(1.0 + (n - df + 0.5) / (df + 0.5))
        for path, tf in counts.items():
            length = corpus.lengths.get(path)
            if length is None:
                continue
            norm = BM25_K1 * (1.0 - BM25_B + BM25_B * length / corpus.average_length)
            scores[path] = scores.get(path, 0.0) + idf * tf * (BM25_K1 + 1.0) / (tf + norm)
    wall = round(time.monotonic() - started, 3)
    ranked = sorted(scores, key=lambda p: (-scores[p], p))
    return {
        "wall_seconds": wall,
        "terms": terms,
        "paths": ranked[:MAX_RANK],
        "top_scores": [[p, round(scores[p], 3)] for p in ranked[:5]],
        "files_matched": len(ranked),
    }


# --------------------------------------------------------------------------- scoring


def first_rank(paths: list[str], truth: set[str]) -> int | None:
    for index, path in enumerate(paths[:MAX_RANK]):
        if path in truth:
            return index + 1
    return None


def score(paths: list[str], accepted: set[str], primary: set[str]) -> dict[str, Any]:
    return {
        "first_hit_rank": first_rank(paths, accepted),
        "primary_rank": first_rank(paths, primary),
    }


def wilson(successes: int, n: int) -> list[float] | None:
    if n == 0:
        return None
    p = successes / n
    denominator = 1 + Z95**2 / n
    centre = (p + Z95**2 / (2 * n)) / denominator
    half = Z95 * math.sqrt(p * (1 - p) / n + Z95**2 / (4 * n * n)) / denominator
    return [round(max(0.0, centre - half), 3), round(min(1.0, centre + half), 3)]


def bootstrap_mean(values: list[float], seed: str) -> list[float] | None:
    if not values:
        return None
    rng = random.Random(seed)
    n = len(values)
    means = sorted(
        sum(rng.choice(values) for _ in range(n)) / n for _ in range(BOOTSTRAP_RESAMPLES)
    )
    return [
        round(means[int(0.025 * BOOTSTRAP_RESAMPLES)], 3),
        round(means[int(0.975 * BOOTSTRAP_RESAMPLES) - 1], 3),
    ]


def rank_metrics(ranks: list[int | None], seed: str) -> dict[str, Any]:
    n = len(ranks)
    metrics: dict[str, Any] = {}
    for k in TOP_KS:
        hits = sum(1 for rank in ranks if rank is not None and rank <= k)
        metrics[f"r@{k}"] = {
            "hits": hits,
            "n": n,
            "rate": round(hits / n, 3) if n else None,
            "ci": wilson(hits, n),
        }
    reciprocal = [1.0 / rank if rank else 0.0 for rank in ranks]
    metrics["mrr"] = {
        "value": round(sum(reciprocal) / n, 3) if n else None,
        "ci": bootstrap_mean(reciprocal, seed),
    }
    return metrics


def group_summary(rows: list[dict[str, Any]], methods: list[str], name: str) -> dict[str, Any]:
    entry: dict[str, Any] = {"questions": len(rows)}
    for method in methods:
        accepted = [row[method]["score"]["first_hit_rank"] for row in rows]
        primary = [row[method]["score"]["primary_rank"] for row in rows]
        times = [row[method]["wall_seconds"] for row in rows]
        entry[method] = {
            "any": rank_metrics(accepted, f"{name}:{method}:any"),
            "primary": rank_metrics(primary, f"{name}:{method}:primary"),
            "median_seconds": round(statistics.median(times), 3) if times else None,
        }
    return entry


def summarize(records: list[dict[str, Any]], methods: list[str]) -> dict[str, Any]:
    summary: dict[str, Any] = {}
    for field in ("repo", "kind", "origin"):
        groups: dict[str, list[dict[str, Any]]] = {}
        for record in records:
            groups.setdefault(str(record.get(field)), []).append(record)
        summary[f"by_{field}"] = {
            name: group_summary(rows, methods, f"{field}={name}")
            for name, rows in sorted(groups.items())
        }
    summary["total"] = group_summary(records, methods, "total")
    return summary


def answer_rule_precision(records: list[dict[str, Any]]) -> dict[str, Any]:
    """How often the fallback's verdict fires, and how often its top hit is then right."""
    rules: dict[str, dict[str, int]] = {}
    for record in records:
        explore = record.get("explore") or {}
        verdict = explore.get("answer_safety")
        rule = (
            "absent"
            if verdict is None
            else f"{'safe' if verdict['safe'] else 'unsafe'}:{verdict['rule']}"
        )
        bucket = rules.setdefault(rule, {"n": 0, "top1_any": 0, "top1_primary": 0})
        bucket["n"] += 1
        bucket["top1_any"] += explore.get("score", {}).get("first_hit_rank") == 1
        bucket["top1_primary"] += explore.get("score", {}).get("primary_rank") == 1
    report: dict[str, Any] = {"bar": ANSWER_RULE_BAR, "rules": {}}
    safe_n = safe_any = safe_primary = 0
    for rule, bucket in sorted(rules.items()):
        report["rules"][rule] = {
            **bucket,
            "precision_any": round(bucket["top1_any"] / bucket["n"], 3),
            "ci_any": wilson(bucket["top1_any"], bucket["n"]),
            "precision_primary": round(bucket["top1_primary"] / bucket["n"], 3),
            "ci_primary": wilson(bucket["top1_primary"], bucket["n"]),
        }
        if rule.startswith("safe:"):
            safe_n += bucket["n"]
            safe_any += bucket["top1_any"]
            safe_primary += bucket["top1_primary"]
    report["safe"] = {
        "n": safe_n,
        "of": len(records),
        "top1_any": safe_any,
        "precision_any": round(safe_any / safe_n, 3) if safe_n else None,
        "ci_any": wilson(safe_any, safe_n),
        "top1_primary": safe_primary,
        "ci_primary": wilson(safe_primary, safe_n),
        "meets_bar": bool(safe_n) and (wilson(safe_any, safe_n) or [0.0])[0] >= ANSWER_RULE_BAR,
    }
    return report


# --------------------------------------------------------------------------- audit


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


# --------------------------------------------------------------------------- output


def _fmt_rate(metric: dict[str, Any]) -> str:
    ci = metric["ci"]
    return f"{metric['hits']}/{metric['n']} [{ci[0]:.2f}–{ci[1]:.2f}]" if ci else "-"


def _fmt_mrr(metric: dict[str, Any]) -> str:
    ci = metric["ci"]
    return f"{metric['value']:.3f} [{ci[0]:.2f}–{ci[1]:.2f}]" if ci else "-"


def metric_table(
    groups: dict[str, Any], methods: list[str], which: str, ks: Iterable[int]
) -> list[str]:
    ks = list(ks)
    header = ["Group", "N", "Method"] + [f"R@{k}" for k in ks] + ["MRR", "median s"]
    lines = ["| " + " | ".join(header) + " |", "|" + " --- |" * len(header)]
    for name, entry in groups.items():
        for method in methods:
            data = entry[method]
            cells = [name, str(entry["questions"]), METHODS[method]]
            cells += [_fmt_rate(data[which][f"r@{k}"]) for k in ks]
            cells += [_fmt_mrr(data[which]["mrr"]), str(data["median_seconds"])]
            lines.append("| " + " | ".join(cells) + " |")
    return lines


def render_markdown(result: dict[str, Any]) -> str:
    methods = result["methods"]
    summary = result["summary"]
    lines = [
        f"# Navigation eval — {result['date']}{' (' + result['label'] + ')' if result['label'] else ''}",
        "",
        f"- aethyme: `{result['aethyme_version']}`",
        f"- rg: `{result['rg_version']}`",
        f"- questions: {len(result['records'])} (`{result['questions_file']}`)",
        "- intervals: 95% Wilson for recall, 95% percentile bootstrap for MRR",
        "",
        "## Total, any accepted answer",
        "",
        *metric_table({"total": summary["total"]}, methods, "any", TOP_KS),
        "",
        "## Total, primary answer only",
        "",
        *metric_table({"total": summary["total"]}, methods, "primary", TOP_KS),
        "",
        "## By repository (any accepted answer)",
        "",
        *metric_table(summary["by_repo"], methods, "any", (1, 3, 8)),
        "",
        "## By kind (any accepted answer)",
        "",
        *metric_table(summary["by_kind"], methods, "any", (1, 3, 8)),
        "",
        "## By origin (any accepted answer)",
        "",
        *metric_table(summary["by_origin"], methods, "any", (1, 3, 8)),
        "",
    ]
    rules = result.get("answer_rule_precision")
    if rules:
        lines += [
            f"## Answer-rule precision (bar {rules['bar']:.0%})",
            "",
            "Top-1 correctness grouped by `observability.source_fallback.answer_safety`.",
            "",
            "| Verdict | N | top-1 any | top-1 primary |",
            "| --- | --- | --- | --- |",
        ]
        for rule, bucket in rules["rules"].items():
            ci_any, ci_primary = bucket["ci_any"], bucket["ci_primary"]
            lines.append(
                f"| {rule} | {bucket['n']} | {bucket['top1_any']}/{bucket['n']} "
                f"[{ci_any[0]:.2f}–{ci_any[1]:.2f}] | {bucket['top1_primary']}/{bucket['n']} "
                f"[{ci_primary[0]:.2f}–{ci_primary[1]:.2f}] |"
            )
        safe = rules["safe"]
        lines += [
            "",
            f"Safe verdicts: {safe['n']}/{safe['of']}; top-1 correct {safe['top1_any']}"
            f"/{safe['n']}, CI {safe['ci_any']}; meets bar (CI lower bound ≥ bar): "
            f"{safe['meets_bar']}.",
            "",
        ]
    header = ["Id", "Repo", "Kind"] + [f"{METHODS[m]} rank" for m in methods]
    lines += [
        "## Per question (first accepted rank / primary rank)",
        "",
        "| " + " | ".join(header + ["Explore status", "Explore first path"]) + " |",
        "|" + " --- |" * (len(header) + 2),
    ]
    for record in result["records"]:
        cells = [record["id"], record["repo"], record["kind"]]
        for method in methods:
            scored = record[method]["score"]
            cells.append(f"{scored['first_hit_rank'] or '-'} / {scored['primary_rank'] or '-'}")
        explore = record.get("explore") or {"paths": []}
        cells.append(str(explore.get("status") or explore.get("error") or "-"))
        cells.append(f"`{(explore['paths'] or ['-'])[0]}`")
        lines.append("| " + " | ".join(cells) + " |")
    lines += ["", "## Target-repo writes during the run", ""]
    for repo, delta in result["repo_writes"].items():
        lines.append(f"- {repo}: `{json.dumps(delta, sort_keys=True)}`")
    lines.append("")
    return "\n".join(lines)


def tool_version(command: list[str]) -> str:
    try:
        completed = subprocess.run(command, capture_output=True, text=True, check=False)
    except OSError as error:
        return f"unavailable: {error}"
    output = (completed.stdout or completed.stderr).strip()
    return output.splitlines()[0] if output else ""


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--questions", type=Path, default=DEFAULT_QUESTIONS)
    parser.add_argument(
        "--results-dir", type=Path, help="default: results/ next to the questions file"
    )
    parser.add_argument("--aethyme", default=shutil.which("aethyme") or "aethyme")
    parser.add_argument(
        "--rg",
        default=os.environ.get("AETHYME_HELDOUT_RG") or shutil.which("rg") or "rg",
        help="ripgrep binary (set AETHYME_HELDOUT_RG to bypass a PATH shim)",
    )
    parser.add_argument("--only", action="append", default=[], help="question id(s) to run")
    parser.add_argument(
        "--repo-root",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="root for a repo name (overrides repos.json and the question file)",
    )
    parser.add_argument("--repos-json", type=Path, help="default: repos.json next to the questions")
    parser.add_argument(
        "--methods",
        default=",".join(METHODS),
        help=f"comma-separated subset of {','.join(METHODS)}",
    )
    parser.add_argument("--label", default="", help="suffix for the result file names")
    parser.add_argument("--date", default=dt.date.today().isoformat())
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    methods = [method for method in args.methods.split(",") if method]
    unknown = [method for method in methods if method not in METHODS]
    if unknown:
        print(f"unknown method(s): {unknown}", file=sys.stderr)
        return 2
    questions_file = args.questions.resolve()
    questions = load_questions(questions_file)
    if args.only:
        questions = [question for question in questions if question["id"] in set(args.only)]
    roots = resolve_repo_roots(questions, questions_file, args.repos_json, args.repo_root)
    repos_used = sorted({question["repo"] for question in questions})
    for name in repos_used:
        if not roots[name].is_dir():
            print(f"missing target repository {name}: {roots[name]}", file=sys.stderr)
            return 2

    corpora: dict[str, Corpus] = {}
    before = {name: snapshot_repo(roots[name]) for name in repos_used}
    runners: dict[str, Callable[[Path, str, str], dict[str, Any]]] = {
        "explore": lambda repo, _name, text: run_explore(args.aethyme, repo, text),
        "rg": lambda repo, _name, text: run_rg_baseline(args.rg, repo, text),
        "bm25": lambda repo, name, text: run_bm25_baseline(
            args.rg, repo, text, corpora.setdefault(name, Corpus(args.rg, repo))
        ),
    }
    records = []
    for question in questions:
        repo = roots[question["repo"]]
        accepted = {answer["path"] for answer in question["answers"]}
        primary = {answer["path"] for answer in question["answers"] if answer_is_primary(answer)}
        record: dict[str, Any] = {
            "id": question["id"],
            "repo": question["repo"],
            "kind": question["kind"],
            "origin": question.get("origin"),
            "accepted_paths": sorted(accepted),
            "primary_paths": sorted(primary),
        }
        progress = [question["id"]]
        for method in methods:
            outcome = runners[method](repo, question["repo"], question["question"])
            outcome["score"] = score(outcome["paths"], accepted, primary)
            outcome["paths"] = outcome["paths"][:KEPT_PATHS]
            record[method] = outcome
            progress.append(
                f"{method}={outcome['score']['first_hit_rank']}/{outcome['score']['primary_rank']}"
                f" ({outcome['wall_seconds']}s)"
            )
        records.append(record)
        print(" ".join(progress), file=sys.stderr)
    after = {name: snapshot_repo(roots[name]) for name in repos_used}

    result: dict[str, Any] = {
        "schema": "aethyme-nav-eval-v2",
        "date": args.date,
        "label": args.label,
        "methods": methods,
        "aethyme_version": tool_version([args.aethyme, "--version"])
        if "explore" in methods
        else None,
        "aethyme_binary": args.aethyme if "explore" in methods else None,
        "rg_version": tool_version([args.rg, "--version"]),
        "questions_file": os.path.relpath(questions_file, HELDOUT_ROOT.parent),
        "metric_notes": {
            "top_ks": list(TOP_KS),
            "max_rank": MAX_RANK,
            "recall_ci": "95% Wilson score interval",
            "mrr_ci": f"95% percentile bootstrap, {BOOTSTRAP_RESAMPLES} resamples, fixed seed",
            "bm25": {"k1": BM25_K1, "b": BM25_B, "idf": "ln(1 + (N - df + 0.5) / (df + 0.5))"},
        },
        "repo_roots": {name: str(roots[name]) for name in repos_used},
        "repo_heads": {name: before[name]["head"] for name in repos_used},
        "bm25_corpus": {
            name: {
                "documents": corpus.documents,
                "average_tokens": round(corpus.average_length, 1),
                "build_seconds": corpus.build_seconds,
            }
            for name, corpus in corpora.items()
        },
        "repo_writes": {name: diff_snapshots(before[name], after[name]) for name in repos_used},
        "summary": summarize(records, methods),
        "answer_rule_precision": answer_rule_precision(records) if "explore" in methods else None,
        "records": records,
    }
    results_dir = args.results_dir or questions_file.parent / "results"
    results_dir.mkdir(parents=True, exist_ok=True)
    stem = f"{args.date}-{args.label}" if args.label else args.date
    json_path = results_dir / f"{stem}.json"
    md_path = results_dir / f"{stem}.md"
    json_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(render_markdown(result), encoding="utf-8")
    print(render_markdown(result))
    print(f"wrote {json_path} and {md_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
