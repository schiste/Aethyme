"""One-shot: scaffold calibration items from a finished eval run.

Reads the 5 per-condition outputs from a run directory and writes one
calibration item JSON per condition with everything pre-filled EXCEPT
human_score — that field is left as null for a human reviewer to fill
in. This preserves the calibration set's integrity: the anchor must
come from human judgment, not from another LLM or from the same scoring
pipeline we're trying to calibrate.

Usage:
    python scaffold_from_run.py \
        --run-dir /path/to/eval-runs/20260420T141853-mediawiki-bug-fix-1 \
        --eval-type bug-fix-1 \
        --target mediawiki \
        --out-dir calibration/bug-fix-1

After running, open each generated *.json, fill in human_score (0-100),
add a short `notes` line explaining your scoring, and save.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def scaffold_item(
    *,
    run_dir: Path,
    condition: str,
    eval_type: str,
    target: str,
    out_dir: Path,
    item_index: int,
) -> Path | None:
    cond_dir = run_dir / "conditions" / condition
    result_file = cond_dir / "result.json"
    if not result_file.exists():
        print(f"  skipping {condition}: no result.json")
        return None
    try:
        data = json.loads(result_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as e:
        print(f"  skipping {condition}: {e}")
        return None

    candidate = data.get("raw_output") or data.get("output_snapshot") or ""
    if not candidate or len(candidate) < 100:
        print(f"  skipping {condition}: candidate too short ({len(candidate)} chars)")
        return None

    calibration_id = (
        f"{target}-{eval_type}-{item_index:03d}-{condition}"
    )
    item = {
        "calibration_id": calibration_id,
        "eval_type": eval_type,
        "target": target,
        "source_run": run_dir.name,
        "source_condition": condition,
        "candidate": candidate,
        "human_score": None,       # <-- REVIEWER FILLS THIS IN (0-100)
        "notes": "",               # <-- optional: one line on why this score
        "task_override": None,
        "reference_override": None,
    }

    out_path = out_dir / f"{item_index:03d}-{condition}.json"
    out_path.write_text(json.dumps(item, indent=2) + "\n", encoding="utf-8")
    return out_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--eval-type", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)

    # Enumerate all condition directories so we cover the range naturally.
    conditions_dir = args.run_dir / "conditions"
    if not conditions_dir.exists():
        print(f"ERROR: {conditions_dir} does not exist")
        return 1

    created = []
    for i, cond_dir in enumerate(sorted(conditions_dir.iterdir()), 1):
        if not cond_dir.is_dir():
            continue
        out = scaffold_item(
            run_dir=args.run_dir,
            condition=cond_dir.name,
            eval_type=args.eval_type,
            target=args.target,
            out_dir=args.out_dir,
            item_index=i,
        )
        if out:
            created.append(out)
            print(f"  wrote {out.relative_to(Path.cwd()) if out.is_relative_to(Path.cwd()) else out}")

    print("")
    print(f"Scaffolded {len(created)} calibration items in {args.out_dir}")
    print("Next step: open each file, fill in `human_score` (0-100) based on")
    print("the judge's rubric (0-20 wrong, 21-40 partial, 41-60 gapped,")
    print("61-80 mostly correct, 81-100 comprehensive), then run the back-test.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
