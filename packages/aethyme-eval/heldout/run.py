"""Navigation eval: Aethyme Explore vs three ripgrep baselines.

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
  normalization ``b``, IDF over every file ``rg --files`` lists in the repo);
* ``bm25plus`` (opt-in via ``--methods``): BM25 over suffix-stemmed terms,
  restricted to code files, plus an IDF bonus when a term appears in the path.

A question scores recall@k when any accepted answer path is among a method's
first k distinct paths, and primary recall@k when the answer flagged
``"primary": true`` is (a missing flag means true). Span recall@k also needs
one of the method's returned line spans for that path to overlap an accepted
line range. MRR uses the first hit within the first ``MAX_RANK`` paths. Every
proportion carries a 95% Wilson interval; MRR carries a 95% percentile-bootstrap
interval. ``--compare A.json B.json`` pairs two runs (McNemar exact test and a
paired bootstrap) without running anything.

Explore runs with a run-private symbol-index cache (``AETHYME_HOST_CACHE_DIR``
pointed at a temp dir; see ``--host-cache``) so results do not depend on what
other sessions left in the host cache.

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
import tempfile
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
# Keep the whole scored list so results can be rescored offline (span overlap,
# new answer sets) without rerunning every method.
KEPT_PATHS = MAX_RANK
# Baselines return files, not spans. For span-overlap scoring a baseline's
# "span" is what an agent would see first: a +/-SNIPPET_RADIUS window around
# the SNIPPET_LINES lines that match the most distinct query terms. Only the
# first SPAN_TOP ranked files get spans (the rest cannot score span@k<=8).
SPAN_TOP = 8
SNIPPET_LINES = 3
SNIPPET_RADIUS = 2
WHOLE_FILE = (1, 1 << 31)
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
    "bm25plus": "BM25+ rg (stem, code-only, path bonus)",
}
DEFAULT_METHODS = ("explore", "rg", "bm25")

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


def _item_spans(item: Any) -> list[list[int]]:
    """Line ranges an Explore item points at: its own range plus evidence line_refs."""
    if not isinstance(item, dict):
        return []
    spans: list[list[int]] = []

    def add(start: Any, end: Any) -> None:
        if isinstance(start, int) and start > 0:
            spans.append([start, end if isinstance(end, int) and end >= start else start])

    add(item.get("start_line", item.get("line")), item.get("end_line"))
    location = item.get("location")
    if isinstance(location, dict):
        add(location.get("start_line", location.get("line")), location.get("end_line"))
    evidence = item.get("evidence")
    if isinstance(evidence, dict):
        for ref in evidence.get("line_refs") or []:
            if isinstance(ref, dict):
                add(ref.get("line", ref.get("start_line")), ref.get("end_line"))
    return spans


def explore_paths(
    payload: dict[str, Any], repo: Path, spans: dict[str, list[list[int]]] | None = None
) -> list[str]:
    """Ordered, deduplicated paths: answers, navigation hints, then targets.

    When ``spans`` is given it is filled with every line range Explore attached
    to each path, across all the items that named it.
    """
    ordered: list[str] = []

    def push(item: Any) -> None:
        raw = _item_path(item)
        if raw:
            path = normalize_path(raw, repo)
            if spans is not None:
                spans.setdefault(path, []).extend(_item_spans(item))
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


def run_explore(
    aethyme: str, repo: Path, question: str, env: dict[str, str] | None = None
) -> dict[str, Any]:
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
            env=env,
        )
    except subprocess.TimeoutExpired:
        return {
            "wall_seconds": round(time.monotonic() - started, 3),
            "exit_code": None,
            "error": "timeout",
            "paths": [],
            "spans": {},
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
                "spans": {},
            }
        )
        return result
    observability = payload.get("observability") or {}
    fallback = observability.get("source_fallback") or {}
    spans: dict[str, list[list[int]]] = {}
    paths = explore_paths(payload, repo, spans)
    result.update(
        {
            "status": payload.get("status"),
            "intent": payload.get("intent"),
            "degraded_reasons": payload.get("degraded_reasons"),
            "safe_to_use_as_answer": payload.get("safe_to_use_as_answer"),
            "truncated": payload.get("truncated"),
            "output_chars": len(completed.stdout),
            "readiness": observability.get("readiness") or {},
            "source_fallback": {
                key: fallback.get(key)
                for key in (
                    "listed_files",
                    "scanned_files",
                    "complete",
                    "reason",
                    "terms",
                    "elapsed_ms",
                    "budget_ms",
                    "symbol_index",
                )
            }
            if fallback
            else None,
            "answer_safety": answer_rule(observability),
            "answer_count": len(payload.get("answer") or []),
            "answer_paths": [
                normalize_path(p, repo)
                for p in (_item_path(item) for item in payload.get("answer") or [])
                if p
            ],
            "paths": paths,
            "spans": {path: spans.get(path, []) for path in paths[:MAX_RANK]},
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


def baseline_spans(
    rg: str, repo: Path, terms: list[str], paths: list[str]
) -> dict[str, list[list[int]]]:
    """Snippet windows an agent would see first in each of the top baseline files.

    One ``rg -n`` over the top ``SPAN_TOP`` files; per file, the ``SNIPPET_LINES``
    lines matching the most distinct terms (earliest first on ties), each widened
    by ``SNIPPET_RADIUS`` lines.
    """
    top = paths[:SPAN_TOP]
    if not top or not terms:
        return {}
    command = [rg, "-n", "-i", "-F", "--no-heading", "--with-filename"]
    for term in terms:
        command += ["-e", term]
    completed = subprocess.run(
        command + ["--", *top],
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=RG_TIMEOUT_SECONDS,
        check=False,
    )
    density: dict[str, list[tuple[int, int]]] = {}
    for line in completed.stdout.splitlines():
        match = re.match(r"^(.*?):(\d+):(.*)$", line)
        if not match:
            continue
        path = normalize_path(match.group(1), repo)
        text = match.group(3).lower()
        hits = sum(1 for term in terms if term in text)
        density.setdefault(path, []).append((hits, int(match.group(2))))
    spans: dict[str, list[list[int]]] = {}
    for path, lines in density.items():
        best = sorted(lines, key=lambda item: (-item[0], item[1]))[:SNIPPET_LINES]
        spans[path] = [
            [max(1, number - SNIPPET_RADIUS), number + SNIPPET_RADIUS] for _, number in best
        ]
    return spans


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
        "spans": baseline_spans(rg, repo, terms, ranked),
        "ties_at_top": sum(
            1
            for p in ranked[1:]
            if ranked
            and (distinct_hits[p], total_matches[p])
            == (distinct_hits[ranked[0]], total_matches[ranked[0]])
        ),
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
        "spans": baseline_spans(rg, repo, terms, ranked),
        "top_scores": [[p, round(scores[p], 3)] for p in ranked[:5]],
        "files_matched": len(ranked),
    }


# The refinements an agent applies after one look at plain rg output: search
# the word stem, skip prose/data/test files, and notice when a file's path
# names the concept. Generic on purpose: no repo- or question-specific lists.
NON_CODE_RE = re.compile(
    r"(\.(md|mdx|markdown|rst|txt|adoc|html?|json|jsonl|csv|tsv|lock|svg|map|min\.js|log|xml|pdf)$)"
    r"|(^|/)(docs?|documentation|tests?|__tests__|spec|fixtures?|testdata)/"
    r"|(^|/)test_[^/]*$|[._-](test|spec)\.[a-z0-9]+$|_test\.[a-z0-9]+$",
    re.IGNORECASE,
)
SUFFIXES = ("ations", "ation", "ings", "ing", "ies", "ied", "ed", "es", "s", "ly")


def stem(term: str) -> str:
    for suffix in SUFFIXES:
        if term.endswith(suffix) and len(term) - len(suffix) >= 4:
            return term[: -len(suffix)] + ("y" if suffix in ("ies", "ied") else "")
    return term


def run_bm25_plus_baseline(rg: str, repo: Path, question: str, corpus: Corpus) -> dict[str, Any]:
    """BM25 over stemmed terms, code files only, plus a path-token bonus."""
    terms = list(dict.fromkeys(stem(term) for term in extract_terms(question)))
    code = {path: length for path, length in corpus.lengths.items() if not NON_CODE_RE.search(path)}
    n = len(code) or 1
    average = sum(code.values()) / n if code else 1.0
    scores: dict[str, float] = {}
    started = time.monotonic()
    for term in terms:
        counts = {
            p: c for p, c in _rg_counts(rg, repo, term, "--count-matches").items() if p in code
        }
        in_path = [p for p in code if term in p.lower()]
        df = len(set(counts) | set(in_path))
        if not df:
            continue
        idf = math.log(1.0 + (n - df + 0.5) / (df + 0.5))
        for path, tf in counts.items():
            norm = BM25_K1 * (1.0 - BM25_B + BM25_B * code[path] / average)
            scores[path] = scores.get(path, 0.0) + idf * tf * (BM25_K1 + 1.0) / (tf + norm)
        for path in in_path:
            scores[path] = scores.get(path, 0.0) + idf  # path names the concept
    wall = round(time.monotonic() - started, 3)
    ranked = sorted(scores, key=lambda p: (-scores[p], p))
    return {
        "wall_seconds": wall,
        "terms": terms,
        "paths": ranked[:MAX_RANK],
        "spans": baseline_spans(rg, repo, terms, ranked),
        "top_scores": [[p, round(scores[p], 3)] for p in ranked[:5]],
        "files_matched": len(ranked),
    }


# --------------------------------------------------------------------------- scoring


def first_rank(paths: list[str], truth: set[str]) -> int | None:
    for index, path in enumerate(paths[:MAX_RANK]):
        if path in truth:
            return index + 1
    return None


def answer_ranges(
    answers: list[dict[str, Any]], primary_only: bool
) -> dict[str, list[tuple[int, int]]]:
    """Accepted line ranges per path; an answer without lines accepts the whole file."""
    ranges: dict[str, list[tuple[int, int]]] = {}
    for answer in answers:
        if primary_only and not answer_is_primary(answer):
            continue
        start = answer.get("start_line")
        end = answer.get("end_line", start)
        span = (int(start), int(end)) if isinstance(start, int) else WHOLE_FILE
        ranges.setdefault(answer["path"], []).append(span)
    return ranges


def first_span_rank(
    paths: list[str],
    spans: dict[str, list[list[int]]] | None,
    ranges: dict[str, list[tuple[int, int]]],
) -> int | None:
    """First rank whose path is accepted AND one of its returned spans overlaps an accepted range."""
    spans = spans or {}
    for index, path in enumerate(paths[:MAX_RANK]):
        wanted = ranges.get(path)
        if not wanted:
            continue
        for start, end in spans.get(path, []):
            if any(start <= hi and end >= lo for lo, hi in wanted):
                return index + 1
    return None


def score(
    paths: list[str],
    accepted: set[str],
    primary: set[str],
    spans: dict[str, list[list[int]]] | None = None,
    answers: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "first_hit_rank": first_rank(paths, accepted),
        "primary_rank": first_rank(paths, primary),
    }
    if answers is not None:
        result["span_rank"] = first_span_rank(paths, spans, answer_ranges(answers, False))
        result["span_primary_rank"] = first_span_rank(paths, spans, answer_ranges(answers, True))
    return result


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
        if all("span_rank" in row[method]["score"] for row in rows):
            entry[method]["span"] = rank_metrics(
                [row[method]["score"]["span_rank"] for row in rows], f"{name}:{method}:span"
            )
            entry[method]["span_primary"] = rank_metrics(
                [row[method]["score"]["span_primary_rank"] for row in rows],
                f"{name}:{method}:span_primary",
            )
    return entry


def explore_completeness(record: dict[str, Any]) -> str:
    explore = record.get("explore") or {}
    if explore.get("error"):
        return f"error:{explore['error']}"
    fallback = explore.get("source_fallback") or {}
    return "complete" if fallback.get("complete", True) else "incomplete"


def summarize(records: list[dict[str, Any]], methods: list[str]) -> dict[str, Any]:
    summary: dict[str, Any] = {}
    for record in records:
        record.setdefault("explore_search", explore_completeness(record))
    for field in ("repo", "kind", "origin", "explore_search"):
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
            if which not in data:
                continue
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
        "## Total, span overlap (a hit needs a returned span inside an accepted range)",
        "",
        "Explore spans are its `line_refs`; a baseline's spans are +/-"
        f"{SNIPPET_RADIUS}-line windows around its {SNIPPET_LINES} densest match lines.",
        "",
        *metric_table({"total": summary["total"]}, methods, "span", TOP_KS),
        "",
        *metric_table({"total": summary["total"]}, methods, "span_primary", TOP_KS),
        "",
        "## By Explore search completeness (any accepted answer)",
        "",
        *metric_table(summary["by_explore_search"], methods, "any", (1, 3, 8)),
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


# --------------------------------------------------------------------------- paired comparison


def mcnemar_exact(b: int, c: int) -> float:
    """Two-sided exact McNemar p-value: binomial test of b vs c discordant pairs."""
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    tail = sum(math.comb(n, i) for i in range(k + 1)) / 2**n
    return min(1.0, 2 * tail)


def paired_bootstrap(diffs: list[float], seed: str) -> tuple[list[float], float]:
    """95% percentile CI of the mean paired difference, and a two-sided bootstrap p-value."""
    rng = random.Random(seed)
    n = len(diffs)
    means = sorted(sum(rng.choice(diffs) for _ in range(n)) / n for _ in range(BOOTSTRAP_RESAMPLES))
    lower = means[int(0.025 * BOOTSTRAP_RESAMPLES)]
    upper = means[int(0.975 * BOOTSTRAP_RESAMPLES) - 1]
    below = sum(1 for m in means if m <= 0) / BOOTSTRAP_RESAMPLES
    above = sum(1 for m in means if m >= 0) / BOOTSTRAP_RESAMPLES
    return [round(lower, 3), round(upper, 3)], round(min(1.0, 2 * min(below, above)), 4)


def _ranks(record: dict[str, Any], method: str, which: str) -> int | None:
    key = {
        "any": "first_hit_rank",
        "primary": "primary_rank",
        "span": "span_rank",
        "span_primary": "span_primary_rank",
    }[which]
    return (record.get(method) or {}).get("score", {}).get(key)


def compare_runs(path_a: Path, path_b: Path, method_a: str, method_b: str) -> str:
    """Paired comparison of two result files (or two methods of one file) on shared question ids."""
    runs = [json.loads(Path(p).read_text(encoding="utf-8")) for p in (path_a, path_b)]
    records = [
        {r["id"]: r for r in run["records"] if r.get(m)}
        for run, m in zip(runs, (method_a, method_b), strict=True)
    ]
    ids = sorted(set(records[0]) & set(records[1]))
    title = f"{path_a.name}:{method_a} (A) vs {path_b.name}:{method_b} (B)"
    lines = [
        f"# Paired comparison: {title}",
        "",
        f"- shared questions: {len(ids)}"
        f" (A only {len(set(records[0]) - set(records[1]))}, B only {len(set(records[1]) - set(records[0]))})",
        "- recall: exact McNemar on discordant pairs; MRR: paired bootstrap of the per-question"
        f" reciprocal-rank difference ({BOOTSTRAP_RESAMPLES} resamples, fixed seed)",
        "",
        "| Metric | A | B | A-only | B-only | diff (B-A) [95% CI] | p |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for which in ("any", "primary", "span", "span_primary"):
        pairs = [
            (_ranks(records[0][i], method_a, which), _ranks(records[1][i], method_b, which))
            for i in ids
        ]
        if which.startswith("span") and not all(
            "span_rank" in records[side][i][m]["score"]
            for i in ids
            for side, m in ((0, method_a), (1, method_b))
        ):
            continue
        for k in TOP_KS:
            hit = [(a is not None and a <= k, b is not None and b <= k) for a, b in pairs]
            only_a = sum(1 for a, b in hit if a and not b)
            only_b = sum(1 for a, b in hit if b and not a)
            ci, _ = paired_bootstrap([float(b) - float(a) for a, b in hit], f"cmp:{which}:{k}")
            lines.append(
                f"| {which} R@{k} | {sum(a for a, _ in hit)} | {sum(b for _, b in hit)} | {only_a} | "
                f"{only_b} | {(only_b - only_a) / len(ids):+.3f} [{ci[0]:+.3f}, {ci[1]:+.3f}] | "
                f"{mcnemar_exact(only_a, only_b):.3f} |"
            )
        rr = [((1.0 / a) if a else 0.0, (1.0 / b) if b else 0.0) for a, b in pairs]
        ci, p = paired_bootstrap([b - a for a, b in rr], f"cmp:{which}:mrr")
        mean_a = sum(a for a, _ in rr) / len(ids)
        mean_b = sum(b for _, b in rr) / len(ids)
        lines.append(
            f"| {which} MRR | {mean_a:.3f} | {mean_b:.3f} | | | {mean_b - mean_a:+.3f} "
            f"[{ci[0]:+.3f}, {ci[1]:+.3f}] | {p:.3f} |"
        )
    lines += [
        "",
        "A difference is only evidence when p < 0.05 and the CI excludes 0. With N ~ 60,"
        " expect to need roughly 8+ net discordant questions at R@3 before that happens.",
        "",
    ]
    return "\n".join(lines)


# --------------------------------------------------------------------------- isolation


def explore_environment(mode: str, scratch: Path) -> dict[str, str]:
    """Environment for Explore calls.

    ``shared`` (the historical behaviour) uses the host symbol-index cache in
    ~/Library/Caches/Aethyme, which other sessions and earlier runs warm. Explore's
    source fallback has a 2 s wall-clock budget, so cache state changes how many
    files are scanned and therefore the ranking on large repos. ``warm`` and
    ``cold`` point AETHYME_HOST_CACHE_DIR at a run-private directory: ``warm``
    fills it first (see ``warm_up``), ``cold`` starts every question empty.
    Output measurement is forced off so inspection commands stay write-free.
    """
    env = dict(os.environ)
    env["AETHYME_MEASURE_OUTPUT"] = "0"
    if mode != "shared":
        env["AETHYME_HOST_CACHE_DIR"] = str(scratch)
    return env


def warm_up(aethyme: str, repo: Path, env: dict[str, str], attempts: int = 8) -> list[bool]:
    """Run a throwaway query until the symbol index is fully cached.

    A complete scan is not enough: a cheap query can finish inside the budget
    while most files are still unparsed, and the next heavy question then
    parses (and times out) instead. Stop only when a complete scan parsed
    nothing new.
    """
    history: list[bool] = []
    for _ in range(attempts):
        outcome = run_explore(aethyme, repo, "where is the main entry point configured", env)
        fallback = outcome.get("source_fallback") or {}
        parsed = (fallback.get("symbol_index") or {}).get("parsed_files")
        settled = bool(fallback.get("complete")) and parsed == 0
        history.append(settled)
        if settled:
            break
    return history


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
        default=",".join(DEFAULT_METHODS),
        help=f"comma-separated subset of {','.join(METHODS)}",
    )
    parser.add_argument("--label", default="", help="suffix for the result file names")
    parser.add_argument("--date", default=dt.date.today().isoformat())
    parser.add_argument(
        "--host-cache",
        choices=("shared", "warm", "cold"),
        default="warm",
        help="Explore symbol-index cache: shared host cache (irreproducible), a run-private"
        " cache warmed to a complete scan per repo first (default), or empty per question",
    )
    parser.add_argument(
        "--compare",
        nargs=2,
        type=Path,
        metavar=("A.json", "B.json"),
        help="paired comparison of two result files on shared question ids; no runs",
    )
    parser.add_argument(
        "--compare-methods",
        default="explore,explore",
        help="METHOD_A,METHOD_B for --compare (e.g. explore,bm25 with the same file twice)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.compare:
        method_a, _, method_b = args.compare_methods.partition(",")
        print(compare_runs(args.compare[0], args.compare[1], method_a, method_b or method_a))
        return 0
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
    cache_root = Path(tempfile.mkdtemp(prefix="aethyme-eval-cache-"))
    explore_env = explore_environment(args.host_cache, cache_root)
    warmups: dict[str, list[bool]] = {}
    if "explore" in methods and args.host_cache == "warm":
        for name in repos_used:
            warmups[name] = warm_up(args.aethyme, roots[name], explore_env)
            print(f"warm-up {name}: complete after {warmups[name]}", file=sys.stderr)

    def explore_runner(repo: Path, _name: str, text: str) -> dict[str, Any]:
        if args.host_cache == "cold":
            shutil.rmtree(cache_root, ignore_errors=True)
            cache_root.mkdir(parents=True, exist_ok=True)
        return run_explore(args.aethyme, repo, text, explore_env)

    runners: dict[str, Callable[[Path, str, str], dict[str, Any]]] = {
        "explore": explore_runner,
        "rg": lambda repo, _name, text: run_rg_baseline(args.rg, repo, text),
        "bm25": lambda repo, name, text: run_bm25_baseline(
            args.rg, repo, text, corpora.setdefault(name, Corpus(args.rg, repo))
        ),
        "bm25plus": lambda repo, name, text: run_bm25_plus_baseline(
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
            outcome["score"] = score(
                outcome["paths"], accepted, primary, outcome.get("spans"), question["answers"]
            )
            outcome["paths"] = outcome["paths"][:KEPT_PATHS]
            record[method] = outcome
            progress.append(
                f"{method}={outcome['score']['first_hit_rank']}/{outcome['score']['primary_rank']}"
                f" ({outcome['wall_seconds']}s)"
            )
        records.append(record)
        print(" ".join(progress), file=sys.stderr)
    after = {name: snapshot_repo(roots[name]) for name in repos_used}
    shutil.rmtree(cache_root, ignore_errors=True)

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
        "host_cache": args.host_cache,
        "warm_up_complete_history": warmups,
        # mtime diffs cannot attribute writes: broker hooks and other agent sessions
        # write the same .aethyme files concurrently. Treat this as a tripwire only.
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
