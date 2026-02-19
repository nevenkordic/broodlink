# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""System prompt construction and memory loading."""

from __future__ import annotations

from .bridge import BridgeClient
from .config import AgentConfig


def load_system_prompt(
    config: AgentConfig, memories_text: str, display_name: str = ""
) -> str:
    """Load the system prompt template and fill placeholders."""
    display = display_name or config.agent_id
    if config.template_path.exists():
        template = config.template_path.read_text()
    else:
        template = (
            "You are {display_name} ({agent_id}), a {agent_role} agent in Broodlink.\n"
            "Use the available tools to store memory, log work, and coordinate with other agents.\n\n"
            "{memories}"
        )
    prompt = (
        template
        .replace("{agent_id}", config.agent_id)
        .replace("{display_name}", display)
        .replace("{agent_role}", config.agent_role)
        .replace("{memories}", memories_text)
    )
    if config.think_mode == "no_think":
        prompt = "/no_think\n" + prompt
    return prompt


async def load_memories(bridge: BridgeClient) -> str:
    """Fetch all memories via bridge and format as text."""
    result = await bridge.call("recall_memory", {})
    memories = result.get("memories", [])
    if not memories:
        return "No memories stored yet."
    lines = []
    for m in memories:
        topic = m.get("topic", "?")
        content = m.get("content", "")
        lines.append(f"- **{topic}**: {content}")
    return "\n".join(lines)
