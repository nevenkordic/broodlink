# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tool execution loop — bridges LLM tool calls to beads-bridge."""

from __future__ import annotations

import json

from .bridge import BridgeClient
from .config import AgentConfig
from .llm import LLMClient


async def execute_tool_call(bridge: BridgeClient, tool_call: dict) -> str:
    """Execute a single tool call via beads-bridge and return the result as text."""
    func = tool_call.get("function", {})
    name = func.get("name", "")
    try:
        args = json.loads(func.get("arguments", "{}"))
    except json.JSONDecodeError:
        args = {}

    result = await bridge.call(name, args)
    return json.dumps(result, default=str)


async def chat_turn(
    config: AgentConfig,
    bridge: BridgeClient,
    llm: LLMClient,
    system_prompt: str,
    history: list[dict],
    tools: list[dict],
) -> str:
    """Run one full chat turn with tool-calling loop. Returns assistant text."""
    messages = [{"role": "system", "content": system_prompt}] + history

    for _ in range(config.max_tool_rounds):
        result = await llm.complete(messages, tools=tools)

        tool_calls = result.get("tool_calls")
        if not tool_calls:
            return result.get("content", "") or ""

        messages.append(result)

        for tc in tool_calls:
            tool_result = await execute_tool_call(bridge, tc)
            messages.append({
                "role": "tool",
                "tool_call_id": tc.get("id", ""),
                "content": tool_result,
            })

    # Exhausted tool rounds — get a final text response
    result = await llm.complete(messages, tools=None)
    return result.get("content", "") or ""
