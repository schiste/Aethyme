"""Auto-populate calibration item anchor scores from the rubric scorer.

Design note — why this is acceptable:

    The classical calibration pattern (human-anchored) measures
    judge-vs-human agreement. In our setup the judge's INSTRUCTIONS
    already describe the rubric the keyword scorer implements
    (files_identified / root_cause_quality / ...). Anchoring to the
    keyword scorer therefore measures "does the judge apply the rubric
    consistently?" — a weaker construct than "does the judge agree
    with a human?" but the construct we actually need for:
      - drift detection (stable anchor)
      - backend A/B (same anchor for both)
      - iteration velocity (free, deterministic, reproducible)

    A human baseline is still valuable for catching cases where the
    rubric itself is wrong. That's a separate exercise; nothing here
    prevents layering manual scores on top later (we add a `source`
    field so mixed sets are legible to analysis).

Usage:
    python populate_anchor_scores.py --dir calibration/bug-fix-1 \
                                     --eval-type bug-fix-1

Only items where human_score is null are updated. Items with explicit
human scores are preserved (so a rubric-populated anchor can later be
replaced by a manual score without re-running the scaffolder).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Put the aethyme package on sys.path so we can import scoring + schemas.
_SERVER_DIR = Path(__file__).resolve().parent.parent
_AETHYME_PKG = _SERVER_DIR.parent.parent / "aethyme"
if str(_AETHYME_PKG) not in sys.path:
    sys.path.insert(0, str(_AETHYME_PKG))


def _load_scorer_and_reference(eval_type: str):
    """Return (scorer_fn, reference_dict) for the given eval_type."""
    from src.eval import scoring, schemas  # type: ignore
    key = eval_type.replace("-", "_")
    for candidate in (
        (f"score_mediawiki_{key}", f"mediawiki_{key}_reference"),
        (f"score_{key}", f"{key}_reference"),
    ):
        scorer = getattr(scoring, candidate[0], None)
        ref = getattr(schemas, candidate[1], None)
        if callable(scorer) and callable(ref):
            return scorer, ref()
    raise RuntimeError(f"no scorer+reference pair for eval_type={eval_type!r}")


def populate(
    *,
    items_dir: Path,
    eval_type: str,
    force: bool = False,
) -> int:
    scorer, reference = _load_scorer_and_reference(eval_type)
    updated = 0
    for path in sorted(items_dir.glob("*.json")):
        try:
            item = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as e:
            print(f"  skip {path.name}: {e}")
            continue

        if item.get("human_score") is not None and not force:
            print(f"  keep {path.name}: human_score={item['human_score']} already set")
            continue

        try:
            parsed_candidate = json.loads(item["candidate"])
        except (KeyError, json.JSONDecodeError):
            # Non-JSON candidates (free-form text outputs) can't be rubric-scored.
            # Skip them and expect manual scoring for those.
            print(f"  skip {path.name}: candidate not JSON-parseable")
            continue

        scored = scorer(parsed_candidate, reference, tokens_used=0, cost_usd=0)
        weighted = float(scored.get("weighted_score", 0.0))

        item["human_score"] = round(weighted, 1)
        item["anchor_source"] = "rubric"
        item["anchor_source_fn"] = scorer.__name__
        if not item.get("notes"):
            # Human-readable breakdown for transparency when reviewing
            # the calibration set later.
            scores = scored.get("scores", {})
            breakdown = ", ".join(
                f"{k}={v:.2f}" for k, v in scores.items()
            )
            item["notes"] = (
                f"Rubric-derived anchor via {scorer.__name__} "
                f"(weighted={weighted:.1f}; {breakdown}). "
                f"Replace with a human score to override."
            )

        path.write_text(json.dumps(item, indent=2) + "\n", encoding="utf-8")
        print(
            f"  wrote {path.name}: human_score={item['human_score']}  "
            f"(rubric)"
        )
        updated += 1

    print("")
    print(f"Updated {updated} calibration items in {items_dir}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, required=True,
                        help="calibration/<eval-type>/ directory")
    parser.add_argument("--eval-type", required=True)
    parser.add_argument("--force", action="store_true",
                        help="overwrite existing human_score values")
    args = parser.parse_args()
    return populate(items_dir=args.dir, eval_type=args.eval_type, force=args.force)


if __name__ == "__main__":
    raise SystemExit(main())
