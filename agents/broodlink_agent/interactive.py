# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Interactive REPL mode for the Broodlink agent."""

from __future__ import annotations

import asyncio

import aiohttp

from .bridge import BridgeClient
from .config import AgentConfig
from .llm import LLMClient
from .prompt import load_memories, load_system_prompt
from .tools import chat_turn


async def interactive_mode(config: AgentConfig) -> None:
    """Interactive CLI loop — type messages, get LLM responses with tool access."""
    async with aiohttp.ClientSession() as session:
        bridge = BridgeClient(session, config)
        llm = LLMClient(session, config)

        print(f"Fetching tools from {config.tools_url}...")
        tools = await bridge.fetch_tools()
        print(f"  Loaded {len(tools)} tools.")

        print("Loading memories...")
        memories_text = await load_memories(bridge)
        system_prompt = load_system_prompt(config, memories_text)

        print(f"Registering agent '{config.agent_id}'...")
        reg = await bridge.call("agent_upsert", {
            "agent_id": config.agent_id,
            "display_name": config.agent_id,
            "role": config.agent_role,
            "cost_tier": config.cost_tier,
            "active": True,
        })
        if "error" in reg:
            print(f"  WARNING: Registration failed: {reg['error']}")
        else:
            print("  Registered.")

        print(f"\nBroodlink Agent [{config.agent_id}] ready. Type 'quit' to exit.\n")

        history: list[dict] = []

        while True:
            try:
                user_input = input("> ").strip()
            except (EOFError, KeyboardInterrupt):
                print("\nGoodbye.")
                break

            if not user_input:
                continue
            if user_input.lower() in ("quit", "exit"):
                break

            history.append({"role": "user", "content": user_input})

            if len(history) > config.max_history * 2:
                history = history[-(config.max_history * 2):]

            try:
                response = await chat_turn(config, bridge, llm, system_prompt, history, tools)
                history.append({"role": "assistant", "content": response})
                print(f"\n{response}\n")
            except aiohttp.ClientConnectorError:
                print(f"\nERROR: Cannot connect to LLM at {config.lm_url}. Is it running?\n")
            except asyncio.TimeoutError:
                print(f"\nERROR: Request timed out after {config.request_timeout}s.\n")
            except Exception as e:
                print(f"\nERROR: {e}\n")
