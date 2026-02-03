"""
Example Aethyme CLI Plugin.

This demonstrates how to create a custom plugin for the Aethyme CLI.

To use this plugin:
1. Copy this file to ~/.aethyme/plugins/example_plugin.py
2. Run: aethyme example-command
"""

import click


@click.command("example-command")
@click.option("--name", default="World", help="Name to greet")
@click.pass_context
def example_command(ctx, name):
    """
    Example plugin command.

    This is a sample command to demonstrate the plugin system.
    """
    click.echo(f"Hello, {name}!")
    click.echo(f"This is an example plugin command.")

    # You can access the main CLI context
    if ctx.obj.get("verbose"):
        click.echo("Verbose mode is enabled")


@click.command("analyze-patterns")
@click.argument("pattern")
@click.option("--repo", type=click.Path(exists=True), help="Repository path")
@click.pass_context
def analyze_patterns(ctx, pattern, repo):
    """
    Analyze code patterns in repository.

    Example of a more complex plugin command.
    """
    from pathlib import Path

    repo_path = Path(repo) if repo else Path.cwd()

    click.echo(f"Analyzing pattern '{pattern}' in {repo_path}")

    # Example: Find files matching pattern
    matches = list(repo_path.rglob(pattern))

    click.echo(f"\nFound {len(matches)} matches:")
    for match in matches[:10]:
        click.echo(f"  - {match}")

    if len(matches) > 10:
        click.echo(f"  ... and {len(matches) - 10} more")


# Plugin metadata
PLUGIN_METADATA = {
    "name": "example-plugin",
    "version": "1.0.0",
    "description": "Example plugin demonstrating CLI extensibility",
    "author": "Aethyme Team",
    "commands": [example_command, analyze_patterns],
}
