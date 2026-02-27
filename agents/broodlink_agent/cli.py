# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""CLI entry point — Click-based commands + backward-compatible argparse mode."""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any

import click

from .config import AgentConfig


@click.group(invoke_without_command=True)
@click.option("--listen", is_flag=True, help="Run in NATS listener mode (legacy)")
@click.option("--no-think", is_flag=True, help="Disable thinking mode")
@click.pass_context
def cli(ctx: click.Context, listen: bool, no_think: bool) -> None:
    """Broodlink Python SDK — agent framework and tools."""
    ctx.ensure_object(dict)
    ctx.obj["config"] = AgentConfig.from_env()

    if no_think:
        ctx.obj["config"].think_mode = "no_think"

    # Backward compat: `broodlink --listen` or `python -m broodlink_agent --listen`
    if listen:
        config = ctx.obj["config"]
        if not config.agent_id:
            click.echo("ERROR: BROODLINK_AGENT_ID not set.", err=True)
            sys.exit(1)
        if not config.agent_jwt:
            click.echo("ERROR: BROODLINK_AGENT_JWT not set.", err=True)
            sys.exit(1)
        from .listener import listen_mode
        asyncio.run(listen_mode(config))
        return

    # If no subcommand, launch interactive mode
    if ctx.invoked_subcommand is None:
        config = ctx.obj["config"]
        if config.agent_id and config.agent_jwt:
            from .interactive import interactive_mode
            asyncio.run(interactive_mode(config))
        else:
            click.echo(ctx.get_help())


# ── Agent subcommands ───────────────────────────────────────────────

@cli.group()
def agent() -> None:
    """Manage agents."""


@agent.command("list")
@click.pass_context
def agent_list(ctx: click.Context) -> None:
    """List registered agents."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        agents = client.agents.list()
        if not agents:
            click.echo("No agents registered.")
            return
        for a in agents:
            status = "active" if a.get("active", a.get("is_active")) else "inactive"
            click.echo(f"  {a.get('agent_id', '?'):20s}  {a.get('role', ''):12s}  {status}")


# ── Memory subcommands ──────────────────────────────────────────────

@cli.group()
def memory() -> None:
    """Search and manage agent memory."""


@memory.command("search")
@click.argument("query")
@click.option("--limit", "-n", default=5, help="Max results")
@click.pass_context
def memory_search(ctx: click.Context, query: str, limit: int) -> None:
    """Semantic search across agent memory."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        results = client.memory.search(query, limit=limit)
        if not results:
            click.echo("No results.")
            return
        for r in results:
            score = r.score if hasattr(r, "score") else r.get("score", 0)
            topic = r.topic if hasattr(r, "topic") else r.get("topic", "")
            content = r.content if hasattr(r, "content") else r.get("content", "")
            click.echo(f"  [{score:.2f}] {topic}: {content[:80]}")


@memory.command("store")
@click.argument("topic")
@click.argument("content")
@click.option("--tags", "-t", default=None, help="Comma-separated tags")
@click.pass_context
def memory_store(ctx: click.Context, topic: str, content: str, tags: str | None) -> None:
    """Store a memory entry."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        result = client.memory.store(topic, content, tags=tags)
        click.echo(f"Stored: {topic}")


@memory.command("recall")
@click.option("--topic", "-t", default=None, help="Filter by topic substring")
@click.pass_context
def memory_recall(ctx: click.Context, topic: str | None) -> None:
    """List memory entries."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        entries = client.memory.recall(topic_search=topic)
        if not entries:
            click.echo("No entries.")
            return
        for m in entries:
            t = m.topic if hasattr(m, "topic") else m.get("topic", "")
            c = m.content if hasattr(m, "content") else m.get("content", "")
            click.echo(f"  {t}: {c[:80]}")


@memory.command("stats")
@click.pass_context
def memory_stats(ctx: click.Context) -> None:
    """Show memory statistics."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        stats = client.memory.stats()
        click.echo(json.dumps(stats if isinstance(stats, dict) else stats.model_dump(), indent=2, default=str))


# ── Tool subcommands ────────────────────────────────────────────────

@cli.group()
def tool() -> None:
    """Call bridge tools directly."""


@tool.command("call")
@click.argument("name")
@click.argument("params", required=False, default=None)
@click.pass_context
def tool_call(ctx: click.Context, name: str, params: str | None) -> None:
    """Call a bridge tool by name. PARAMS is a JSON string."""
    from .client import BroodlinkClient

    parsed: dict[str, Any] = {}
    if params:
        try:
            parsed = json.loads(params)
        except json.JSONDecodeError:
            click.echo(f"ERROR: Invalid JSON params: {params}", err=True)
            sys.exit(1)

    with BroodlinkClient(ctx.obj["config"]) as client:
        result = client.call(name, parsed)
        click.echo(json.dumps(result, indent=2, default=str))


@tool.command("list")
@click.pass_context
def tool_list(ctx: click.Context) -> None:
    """List available bridge tools."""
    from .client import BroodlinkClient

    with BroodlinkClient(ctx.obj["config"]) as client:
        tools = client.fetch_tools()
        for t in tools:
            func = t.get("function", t)
            click.echo(f"  {func.get('name', '?'):40s}  {func.get('description', '')[:60]}")


# ── Legacy entry point ──────────────────────────────────────────────

def main() -> None:
    """Legacy entry point for `python -m broodlink_agent`."""
    # If called with --listen or no args, use Click CLI
    cli(standalone_mode=True)
