"""CLI entry point for the judge calibration back-test.

Runs the calibration set through each supported judge backend
(codex, claude-haiku) independently and compares mean drift against
human anchors. The output tells you which backend is more accurate for
the current agent population — a direct answer to the self-preference
question.

Usage (from packages/aethyme-eval-ui/server/):
    python calibration_backtest_cli.py --eval-type bug-fix-1
    python calibration_backtest_cli.py --eval-type bug-fix-1 --samples 5
    python calibration_backtest_cli.py --eval-type bug-fix-1 --backends codex

Output is human-readable to stdout. The full per-backend result JSON is
written to --json-out if provided, so it can be archived with the batch.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# This file lives in server/, and calibration_check assumes server/ is on
# sys.path. Make sure of that before the import.
_SERVER_DIR = Path(__file__).resolve().parent
if str(_SERVER_DIR) not in sys.path:
    sys.path.insert(0, str(_SERVER_DIR))

from calibration_check import run_calibration_backtest  # noqa: E402


def _default_aethyme_pkg() -> Path:
    # server/ is packages/aethyme-eval-ui/server/
    # aethyme pkg is packages/aethyme/
    return _SERVER_DIR.parent.parent / "aethyme"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--eval-type", required=True, help="e.g. bug-fix-1")
    parser.add_argument(
        "--backends",
        nargs="+",
        default=["codex", "claude-haiku"],
        help="judge backends to compare",
    )
    parser.add_argument("--samples", type=int, default=3,
                        help="intra-rater samples per item (default 3)")
    parser.add_argument("--tab-directory", default=None,
                        help="cwd for the judge Chau7 tab (default: aethyme pkg root)")
    parser.add_argument("--aethyme-pkg", default=None,
                        help="path to packages/aethyme/ (default: auto-detect)")
    parser.add_argument("--json-out", default=None,
                        help="write full result JSON to this path")
    args = parser.parse_args()

    aethyme_pkg = Path(args.aethyme_pkg) if args.aethyme_pkg else _default_aethyme_pkg()
    tab_directory = args.tab_directory or str(aethyme_pkg)

    def _log(msg: str) -> None:
        print(msg, flush=True)

    _log(f"Running calibration back-test for {args.eval_type}")
    _log(f"  backends: {args.backends}")
    _log(f"  samples per item: {args.samples}")
    _log(f"  tab directory: {tab_directory}")
    _log("")

    result = run_calibration_backtest(
        eval_type=args.eval_type,
        tab_directory=tab_directory,
        aethyme_pkg_path=str(aethyme_pkg),
        backends=tuple(args.backends),
        samples=args.samples,
        log=_log,
    )

    _log("")
    _log("=" * 60)
    _log(f"Back-test result for {args.eval_type}")
    _log("=" * 60)
    for backend, r in result["per_backend"].items():
        mean_drift = r.get("mean_drift")
        max_drift = r.get("max_drift")
        scored = r.get("items_scored", 0)
        found = r.get("items_found", 0)
        passes = r.get("passes")
        err = r.get("error") or r.get("notes") or ""
        _log(
            f"  {backend:16s}  mean_drift={mean_drift!s:>6}  max_drift={max_drift!s:>6}  "
            f"scored={scored}/{found}  passes={passes}"
        )
        if err:
            _log(f"                    note: {err}")
    _log("")
    _log(f"Winner: {result['winner']}")
    _log(f"  {result['winner_reason']}")
    _log(f"  (tie threshold: {result['tie_threshold']} drift points)")

    if args.json_out:
        Path(args.json_out).write_text(
            json.dumps(result, indent=2), encoding="utf-8"
        )
        _log("")
        _log(f"Full result JSON written to: {args.json_out}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
