"""Command-line interface for the internal eval sentinel."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .baseline import build_baseline, default_baseline_path, load_baseline, write_baseline
from .compare import compare_path
from .playground import PlaygroundSetup
from .report import render_json, render_markdown


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="aethyme-eval")
    subcommands = parser.add_subparsers(dest="command", required=True)

    summarize = subcommands.add_parser("summarize-history", help="build a baseline from run history")
    summarize.add_argument("--runs-jsonl", type=Path, required=True)
    summarize.add_argument("--output", type=Path, required=True)
    summarize.add_argument("--name", default="haiku-2026-05")
    summarize.add_argument("--model")
    summarize.add_argument(
        "--exclude-target",
        action="append",
        dest="exclude_targets",
        help="target slug to exclude from the baseline; defaults to aethyme",
    )
    summarize.add_argument(
        "--include-self-targets",
        action="store_true",
        help="include historical self-runs when rebuilding an audit baseline",
    )
    summarize.add_argument("--methodology-hash")
    summarize.add_argument("--source-note")

    compare = subcommands.add_parser("compare", help="compare run results against a baseline")
    compare.add_argument("results", type=Path)
    compare.add_argument("--baseline", type=Path)
    compare.add_argument("--format", choices=("json", "markdown"), default="markdown")
    compare.add_argument("--output", type=Path)
    compare.add_argument("--fail-on-regression", action="store_true")

    info = subcommands.add_parser("baseline-info", help="print the default baseline path")
    info.add_argument("--format", choices=("json", "text"), default="text")

    playground = subcommands.add_parser("playground-command", help="print setup-playground command")
    playground.add_argument("--aethyme-root", type=Path, default=Path("packages/aethyme"))
    playground.add_argument("--source", required=True)
    playground.add_argument("--name", required=True)
    playground.add_argument("--commit", required=True)
    playground.add_argument("--dest", type=Path, required=True)
    playground.add_argument("--force", action="store_true")

    args = parser.parse_args(argv)
    if args.command == "summarize-history":
        baseline = build_baseline(
            args.runs_jsonl,
            name=args.name,
            model=args.model,
            exclude_targets=()
            if args.include_self_targets
            else tuple(args.exclude_targets or ("aethyme",)),
            methodology_hash=args.methodology_hash,
            source_note=args.source_note,
        )
        write_baseline(baseline, args.output)
        print(str(args.output))
        return 0

    if args.command == "compare":
        baseline = load_baseline(args.baseline)
        payload = compare_path(args.results, baseline)
        text = render_json(payload) if args.format == "json" else render_markdown(payload)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(text, encoding="utf-8")
        else:
            print(text, end="")
        if args.fail_on_regression and _gate_failed(payload):
            return 2
        return 0

    if args.command == "baseline-info":
        path = default_baseline_path()
        if args.format == "json":
            print(render_json({"default_baseline": str(path)}), end="")
        else:
            print(path)
        return 0

    if args.command == "playground-command":
        setup = PlaygroundSetup(
            source=args.source,
            name=args.name,
            commit=args.commit,
            dest=args.dest,
            force=args.force,
        )
        print(" ".join(_quote(part) for part in setup.command(args.aethyme_root)))
        return 0

    return 1


def _quote(part: str) -> str:
    if not part or any(ch.isspace() for ch in part):
        return "'" + part.replace("'", "'\\''") + "'"
    return part


def _gate_failed(payload: dict) -> bool:
    summary = payload.get("summary") or {}
    return bool(
        summary.get("fail")
        or summary.get("missing_baseline")
        or summary.get("total", 0) == 0
    )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
