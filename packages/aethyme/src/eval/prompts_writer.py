"""CLI shim that writes per-condition prompts + output schema to disk.

Invoked by the eval-plan's `build-inputs` phase for read-only diagnostic
eval types (`bug-fix-1`, `dead-code`, `impact-analysis`, ...). The shim
exists so the orchestrator's `cli_cmd` can be a single
`python -m src.eval.prompts_writer ...` call instead of an inline
`python -c "..."` template (which has shell-quoting issues with
em-dashes, quoted JSON, etc — we hit them on bug-fix-1).

Usage::

    python -m src.eval.prompts_writer \\
        --eval-type dead-code \\
        --target mediawiki \\
        --schema-out /tmp/aethyme-eval-output-schema.json \\
        --prompt-out control-cto-off=/tmp/aethyme-eval-control-cto-off-prompt.txt \\
        --prompt-out control-cto-on=/tmp/aethyme-eval-control-cto-on-prompt.txt \\
        --prompt-out explore=/tmp/aethyme-eval-explore-prompt.txt \\
        --prompt-out leverage=/tmp/aethyme-eval-leverage-prompt.txt \\
        --prompt-out task-conditioned=/tmp/aethyme-eval-task-conditioned-prompt.txt
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Callable

from .prompts import build_prompts
from .targets import TARGETS


# Map eval_type → output-schema constructor. Kept here rather than in
# `schemas.py` so the prompts module doesn't take on the schema imports
# (and so this module is the single authority over the artifact set).
def _schema_for(eval_type: str) -> dict[str, object]:
    from . import schemas

    table: dict[str, Callable[[], dict[str, object]]] = {
        "bug-fix-1": schemas.mediawiki_bug_fix_1_output_schema,
        "dead-code": schemas.mediawiki_dead_code_output_schema,
        "impact-analysis": schemas.mediawiki_impact_analysis_output_schema,
        "feature-localization": schemas.mediawiki_feature_localization_output_schema,
        "config-audit": schemas.mediawiki_config_audit_output_schema,
        "migration": schemas.mediawiki_migration_output_schema,
    }
    if eval_type not in table:
        raise ValueError(
            f"No output-schema constructor registered for eval_type={eval_type!r}; "
            f"known types: {sorted(table)}"
        )
    return table[eval_type]()


def _parse_prompt_out(value: str) -> tuple[str, str]:
    """Parse `--prompt-out condition=path` into a (condition, path) tuple."""
    if "=" not in value:
        raise argparse.ArgumentTypeError(
            f"--prompt-out expects condition=path; got {value!r}"
        )
    cond, _, path = value.partition("=")
    if not cond or not path:
        raise argparse.ArgumentTypeError(
            f"--prompt-out got empty condition or path: {value!r}"
        )
    return cond, path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--eval-type", required=True)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--schema-out", type=Path, required=True,
                        help="Path to write the output JSON schema.")
    parser.add_argument(
        "--prompt-out",
        action="append",
        type=_parse_prompt_out,
        default=[],
        help="condition=path; repeat once per condition.",
    )
    args = parser.parse_args(argv)

    target = TARGETS[args.target]
    prompts = build_prompts(args.eval_type, target)
    schema = _schema_for(args.eval_type)

    # Validate that every requested condition is in the prompt builder's output.
    requested = dict(args.prompt_out)
    missing = set(requested) - set(prompts)
    if missing:
        print(
            f"build_prompts({args.eval_type!r}) is missing conditions: "
            f"{sorted(missing)}. Available: {sorted(prompts)}",
            file=sys.stderr,
        )
        return 2
    extra = set(prompts) - set(requested)
    if extra:
        # Not fatal — caller can ignore conditions — but worth a notice.
        print(
            f"NOTE: build_prompts produced conditions {sorted(extra)} that "
            f"were not requested via --prompt-out; they will not be written.",
            file=sys.stderr,
        )

    # Write the prompt files.
    for cond, path_str in requested.items():
        path = Path(path_str)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(prompts[cond])

    # Write the output schema.
    args.schema_out.parent.mkdir(parents=True, exist_ok=True)
    args.schema_out.write_text(json.dumps(schema, indent=2))

    print(
        f"{args.eval_type} artifacts written: "
        f"{len(requested)} prompts + 1 schema for target={args.target}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
