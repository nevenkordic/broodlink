# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""CLI entry point with argparse."""

from __future__ import annotations

import argparse
import asyncio
import sys

from .config import AgentConfig


def main() -> None:
    """Parse CLI arguments and run the appropriate mode."""
    parser = argparse.ArgumentParser(
        description="Broodlink agent — connects any OpenAI-compatible LLM to Broodlink"
    )
    parser.add_argument(
        "--listen", action="store_true",
        help="Run in NATS listener mode (receive tasks from coordinator)",
    )
    parser.add_argument(
        "--task", type=str, default=None,
        help="Process a single task by ID, then exit",
    )
    parser.add_argument(
        "--think", action="store_true", default=True,
        help="Enable thinking/reasoning mode (default)",
    )
    parser.add_argument(
        "--no-think", action="store_true",
        help="Disable thinking mode for small models",
    )
    args = parser.parse_args()

    config = AgentConfig.from_env()

    if args.no_think:
        config.think_mode = "no_think"

    if not config.agent_id:
        print("ERROR: BROODLINK_AGENT_ID not set.", file=sys.stderr)
        sys.exit(1)
    if not config.agent_jwt:
        print("ERROR: BROODLINK_AGENT_JWT not set.", file=sys.stderr)
        sys.exit(1)

    if args.listen:
        from .listener import listen_mode
        asyncio.run(listen_mode(config))
    else:
        from .interactive import interactive_mode
        asyncio.run(interactive_mode(config))
