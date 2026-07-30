import sys
from pathlib import Path
from typing import Any, TypeAlias, TypedDict, cast

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
                "The Python CLI still handles graph, task, intents, facts, and analyze."
            )
        return super().get_command(ctx, cmd_name)


class FixRecord(TypedDict):
    """Autofix change proposal emitted by individual fixers."""

    file_path: Path
    original_content: str
    new_content: str
    fix_type: str


def normalize_fixes(raw_fixes: list[dict[str, Any]]) -> list[FixRecord]:
    """Validate fixer output before patch generation."""
    normalized: list[FixRecord] = []
    for raw_fix in raw_fixes:
        file_path = raw_fix.get("file_path")
        original_content = raw_fix.get("original_content")
        new_content = raw_fix.get("new_content")
        fix_type = raw_fix.get("fix_type")

        if isinstance(file_path, str):
            path_value = Path(file_path)
        elif isinstance(file_path, Path):
            path_value = file_path
        else:
            raise ValueError(f"Invalid fix file_path: {file_path!r}")

        if not isinstance(original_content, str):
            raise ValueError(f"Invalid fix original_content for {path_value}")
        if not isinstance(new_content, str):
            raise ValueError(f"Invalid fix new_content for {path_value}")
        if not isinstance(fix_type, str):
            raise ValueError(f"Invalid fix fix_type for {path_value}")

        normalized.append(
            FixRecord(
                file_path=path_value,
                original_content=original_content,
                new_content=new_content,
                fix_type=fix_type,
            )
        )

    return normalized


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

    A powerful CLI for code indexing, querying, and AI-readiness analysis.

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




@cli.command(name="ai-ready")
@click.option(
    "--repo",
    type=click.Path(exists=True),
    help="Repository path (defaults to current directory)",
)
@click.option(
    "--repo-id",
    help="Repository ID for API mode",
)
@click.option(
    "--format",
    "-f",
    type=click.Choice(["json", "md", "both"]),
    default="md",
    help="Output format",
)
@click.option(
    "--output",
    "-o",
    type=click.Path(),
    help="Output file (defaults to stdout)",
)
@click.option(
    "--detectors",
    help="Comma-separated list of detectors to run (runs all if not specified)",
)
@click.pass_context
def ai_ready(
    ctx: click.Context,
    repo: str | None,
    repo_id: str | None,
    format: str,
    output: str | None,
    detectors: str | None,
) -> None:
    """Run AI-readiness scorecard on a repository."""
    from pathlib import Path

    from src.scorecard.engine import ScorecardEngine
    from src.scorecard.metrics import record_scan_metrics

    # Determine repository path
    if repo:
        repo_path = Path(repo).resolve()
    else:
        repo_path = Path.cwd()

    if not repo_path.exists():
        click.echo(f"Error: Repository path does not exist: {repo_path}", err=True)
        sys.exit(2)

    click.echo(f"Running AI-readiness scorecard on: {repo_path}")
    click.echo()

    # Tenant/org lookups retired with the PostgreSQL lineage (2026-07-13).
    tenant_id = cast(str | None, get_state(ctx).get("tenant_id"))

    # Parse detectors list
    detector_list = None
    if detectors:
        detector_list = [d.strip() for d in detectors.split(",")]

    # Run scan
    try:
        engine = ScorecardEngine(
            repo_path=repo_path, repository_id=repo_id, tenant_id=tenant_id
        )

        with click.progressbar(
            length=100, label="Scanning repository", show_eta=True
        ) as bar:
            result = engine.scan(detectors=detector_list)
            bar.update(100)

        # Record metrics
        record_scan_metrics(result.report, tenant_id=tenant_id, repository_id=repo_id)

    except Exception as exc:
        click.echo(f"Error during scan: {exc}", err=True)
        logger.error("Scorecard scan failed", error=str(exc), exc_info=True)
        sys.exit(2)

    click.echo()
    click.echo(f"Scan completed: Score {result.score}/100")
    click.echo(
        f"Findings: {result.report.total_findings} total "
        f"({result.report.blocker_count} blockers, "
        f"{result.report.warning_count} warnings, "
        f"{result.report.info_count} info)"
    )
    click.echo()

    # Generate output
    if format in ["json", "both"]:
        json_output = result.to_json()
        if output and format == "json":
            output_path = Path(output)
            output_path.write_text(json_output)
            click.echo(f"JSON report written to: {output_path}")
        elif format == "both":
            output_base = Path(output) if output else Path("scorecard-report")
            json_path = (
                output_base
                if output_base.suffix == ".json"
                else output_base.with_suffix(".json")
            )
            json_path.write_text(json_output)
            click.echo(f"JSON report written to: {json_path}")
        else:
            click.echo(json_output)

    if format in ["md", "both"]:
        md_output = result.to_markdown()
        if output and format == "md":
            output_path = Path(output)
            output_path.write_text(md_output)
            click.echo(f"Markdown report written to: {output_path}")
        elif format == "both":
            output_base = Path(output) if output else Path("scorecard-report")
            md_path = (
                output_base
                if output_base.suffix == ".md"
                else output_base.with_suffix(".md")
            )
            md_path.write_text(md_output)
            click.echo(f"Markdown report written to: {md_path}")
        else:
            click.echo(md_output)

    # Exit code based on findings
    # 0 = ready (score >= 90)
    # 1 = warnings (score >= 50)
    # 2 = blockers (score < 50)
    if result.report.blocker_count > 0 or result.score < 50:
        sys.exit(2)
    elif result.report.warning_count > 0 or result.score < 90:
        sys.exit(1)
    else:
        sys.exit(0)


@cli.command()
@click.argument("repo_path", type=click.Path(exists=True))
@click.option("--dry-run", is_flag=True, help="Show changes without applying")
@click.option("--apply", "do_apply", is_flag=True, help="Apply changes to disk")
@click.option("--pr", is_flag=True, help="Create pull request")
@click.option(
    "--fix-type",
    type=click.Choice(["all", "docs", "links", "selectors", "i18n", "format"]),
    default="all",
    help="Type of fix to apply",
)
@click.option("--skip-approval", is_flag=True, help="Skip approval for risky changes")
@click.pass_context
def autofix(
    ctx: click.Context,
    repo_path: str,
    dry_run: bool,
    do_apply: bool,
    pr: bool,
    fix_type: str,
    skip_approval: bool,
) -> None:
    """Apply automated fixes to codebase issues."""
    from pathlib import Path

    from src.autofixers.fixers import (
        DocsRegenerator,
        FormatFixer,
        I18nScaffolder,
        LinkFixer,
        SelectorInserter,
    )
    from src.autofixers.github import GitHubIntegration
    from src.autofixers.patch import PatchGenerator
    from src.autofixers.safety import SafetyEngine

    repository_path = Path(repo_path).resolve()

    click.echo("\nAethyme Autofixer")
    click.echo("=" * 60)
    click.echo(f"Repository: {repository_path}")
    click.echo(f"Fix type: {fix_type}")
    click.echo(
        f"Mode: {'DRY RUN' if dry_run else 'APPLY' if do_apply else 'PR' if pr else 'DRY RUN'}"
    )
    click.echo("")

    # Initialize components
    safety_engine = SafetyEngine()
    patch_gen = PatchGenerator(repository_path, safety_engine)

    # Collect fixes based on type
    all_fixes: list[FixRecord] = []

    if fix_type in ["all", "docs"]:
        click.echo("Scanning for documentation issues...")
        docs_fixer = DocsRegenerator(repository_path)
        docs_fixes = normalize_fixes(docs_fixer.create_folder_docs())
        all_fixes.extend(docs_fixes)
        click.echo(f"  Found {len(docs_fixes)} documentation fixes")

    if fix_type in ["all", "links"]:
        click.echo("Scanning for link issues...")
        link_fixer = LinkFixer(repository_path)
        link_fixes = normalize_fixes(link_fixer.process_directory())
        all_fixes.extend(link_fixes)
        click.echo(f"  Found {len(link_fixes)} link fixes")

    if fix_type in ["all", "selectors"]:
        click.echo("Scanning for missing test selectors...")
        selector_fixer = SelectorInserter(repository_path)
        selector_fixes = normalize_fixes(selector_fixer.process_directory())
        all_fixes.extend(selector_fixes)
        click.echo(f"  Found {len(selector_fixes)} selector fixes")

    if fix_type in ["all", "i18n"]:
        click.echo("Scanning for hardcoded strings...")
        i18n_fixer = I18nScaffolder(repository_path)
        i18n_fixes = normalize_fixes(i18n_fixer.process_directory())
        all_fixes.extend(i18n_fixes)
        click.echo(f"  Found {len(i18n_fixes)} i18n fixes")

    if fix_type in ["all", "format"]:
        click.echo("Scanning for formatting issues...")
        format_fixer = FormatFixer(repository_path)
        format_fixes = normalize_fixes(format_fixer.process_directory())
        all_fixes.extend(format_fixes)
        click.echo(f"  Found {len(format_fixes)} formatting fixes")

    # Add patches
    click.echo("\nGenerating patches...")
    for fix in all_fixes:
        patch_gen.add_patch(
            fix["file_path"],
            fix["original_content"],
            fix["new_content"],
            fix["fix_type"],
        )

    summary = patch_gen.get_summary()

    # Display summary
    click.echo("\n" + "=" * 60)
    click.echo("Summary")
    click.echo("=" * 60)
    click.echo(f"Total files: {summary['total_files']}")
    click.echo(f"Low risk: {summary['total_low_risk']}")
    click.echo(f"Medium risk: {summary['total_medium_risk']}")
    click.echo(f"High risk: {summary['total_high_risk']}")
    click.echo("")

    for fix_type_key, count in summary["by_fix_type"].items():
        click.echo(f"  {fix_type_key}: {count} files")

    if summary["total_files"] == 0:
        click.echo("\nNo fixes needed!")
        return

    # Execute based on mode
    if dry_run or (not do_apply and not pr):
        # Dry run mode
        result = patch_gen.dry_run()
        click.echo("\n" + "=" * 60)
        click.echo("Changes Preview (Dry Run)")
        click.echo("=" * 60)

        diff = result["diff"]
        if diff:
            click.echo(diff)
        else:
            click.echo("No changes to show")

        click.echo("\nRun with --apply to apply these changes")
        click.echo("Run with --pr to create a pull request")

    elif do_apply:
        # Apply mode
        if summary["requires_approval"] > 0 and not skip_approval:
            click.echo(
                f"\nWarning: {summary['requires_approval']} files require approval"
            )
            if not click.confirm("Continue anyway?"):
                return

        click.echo("\nApplying fixes...")
        result = patch_gen.apply(skip_approval=skip_approval)

        if result["status"] == "requires_approval":
            click.echo("\nSome fixes require approval:")
            for patch_info in result["requires_approval"]:
                click.echo(f"  {patch_info['file']} ({patch_info['risk_level']})")
            click.echo("\nUse --skip-approval to apply anyway (not recommended)")
            return

        if result["status"] in ["success", "partial"]:
            click.echo(f"\nApplied {len(result['applied'])} files")
            if result.get("failed"):
                click.echo(f"Failed {len(result['failed'])} files:")
                for f in result["failed"]:
                    click.echo(f"  {f}")
        else:
            click.echo(f"\nFailed to apply fixes: {result}")

    elif pr:
        # PR mode
        click.echo("\nCreating pull request...")

        gh_integration = GitHubIntegration(repository_path)

        # Check working tree
        if not gh_integration.is_clean_working_tree():
            click.echo("Error: Working tree has uncommitted changes")
            click.echo("Please commit or stash changes before creating PR")
            return

        # Create PR
        result = gh_integration.create_autofix_pr(patch_gen)

        if result and result.get("url"):
            click.echo(f"\nPull request created: {result['url']}")
            click.echo(f"Branch: {result['branch']}")
            click.echo(f"Commit: {result['commit'][:8]}")
        elif result and result.get("status") == "requires_approval":
            click.echo("\nSome fixes require approval before creating PR")
            click.echo("Use the web UI to approve, or use --apply --skip-approval")
        else:
            click.echo("\nFailed to create pull request")


def main() -> None:
    """Main entry point for CLI."""
    cli(obj={})


if __name__ == "__main__":
    main()
