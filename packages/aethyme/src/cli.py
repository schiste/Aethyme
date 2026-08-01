import sys
from pathlib import Path
from typing import TypeAlias, cast

import click
import structlog

# Add src to path for module imports
sys.path.insert(0, str(Path(__file__).parent.parent))


# Configure logging
structlog.configure(
    processors=[
        structlog.stdlib.filter_by_level,
        structlog.stdlib.add_logger_name,
        structlog.stdlib.add_log_level,
        structlog.stdlib.PositionalArgumentsFormatter(),
        structlog.processors.TimeStamper(fmt="iso"),
        structlog.processors.StackInfoRenderer(),
        structlog.processors.format_exc_info,
        structlog.dev.ConsoleRenderer(),
    ],
    context_class=dict,
    logger_factory=structlog.stdlib.LoggerFactory(),
    cache_logger_on_first_use=True,
)

logger = structlog.get_logger()

CLIState: TypeAlias = dict[str, str | bool | None]


class AethymeCLIGroup(click.Group):
    """Custom command group for clearer recovery from removed entry points."""

    def get_command(self, ctx: click.Context, cmd_name: str) -> click.Command | None:
        if cmd_name == "explore":
            raise click.UsageError(
                "'explore' was removed from the Python CLI on 2026-05-08. "
                "Use the native binary instead:\n\n"
                '  "$AETHYME_ROOT/rust/target/release/aethyme" explore '
                '--repo "$REPO" --request "<task>" --format answer-json\n\n'
                "Every command is now native: run `aethyme --help`."
            )
        return super().get_command(ctx, cmd_name)


def get_state(ctx: click.Context) -> CLIState:
    """Return the mutable CLI state."""
    if ctx.obj is None:
        ctx.obj = {}
    return cast(CLIState, ctx.obj)


@click.group(cls=AethymeCLIGroup)
@click.option(
    "--tenant-id",
    envvar="AETHYME_TENANT_ID",
    help="Tenant ID for multi-tenant isolation",
)
@click.option(
    "--json",
    "output_json_flag",
    is_flag=True,
    help="Output in JSON format",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Verbose output",
)
@click.pass_context
def cli(
    ctx: click.Context,
    tenant_id: str | None,
    output_json_flag: bool,
    verbose: bool,
) -> None:
    """Aethyme - Graph-based code intelligence system.

    Every command is native as of the python-retirement Phase 5 flip
    (2026-08-01): this Click tree carries no commands, and the router
    delegates nothing to it. The group itself survives only until the
    Phase 6 retirement sweep deletes `src/`.

    Examples:
        aethyme index --repo /path/to/repo
        aethyme query search UserService
        aethyme ai-ready --apply
        aethyme autofix --dry-run
    """
    state = get_state(ctx)
    state["tenant_id"] = tenant_id
    state["json"] = output_json_flag
    state["verbose"] = verbose


def main() -> None:
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()
