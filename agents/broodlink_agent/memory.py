# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Context window management — tracks token usage and summarizes history."""

from __future__ import annotations


# Rough token estimation: ~4 chars per token for English text.
CHARS_PER_TOKEN = 4


def estimate_tokens(text: str) -> int:
    """Rough token count estimate from character length."""
    return max(1, len(text) // CHARS_PER_TOKEN)


def estimate_messages_tokens(messages: list[dict]) -> int:
    """Estimate total tokens across a message list."""
    total = 0
    for msg in messages:
        content = msg.get("content", "")
        if isinstance(content, str):
            total += estimate_tokens(content)
        # Tool calls add overhead
        if msg.get("tool_calls"):
            total += 50 * len(msg["tool_calls"])
    return total


def summarize_history(
    messages: list[dict],
    max_tokens: int,
    keep_recent: int = 4,
) -> list[dict]:
    """Trim message history to fit within max_tokens.

    Strategy: keep the system message (first) and the most recent `keep_recent`
    messages. If still over budget, drop tool-call/tool-result pairs from the
    middle until it fits.
    """
    if not messages:
        return messages

    current = estimate_messages_tokens(messages)
    if current <= max_tokens:
        return messages

    # Always keep first (system) message and last keep_recent messages
    if len(messages) <= keep_recent + 1:
        return messages

    head = messages[:1]  # system prompt
    tail = messages[-keep_recent:]  # recent context
    middle = messages[1:-keep_recent] if keep_recent > 0 else messages[1:]

    # Build summary of dropped middle section
    dropped_count = len(middle)
    if dropped_count > 0:
        # Create a brief summary of what was dropped
        summary_parts = []
        for msg in middle:
            role = msg.get("role", "unknown")
            content = msg.get("content", "")
            if isinstance(content, str) and content:
                preview = content[:100].replace("\n", " ")
                summary_parts.append(f"[{role}]: {preview}...")

        summary_text = (
            f"[Context trimmed: {dropped_count} messages removed. Summary of dropped context:]\n"
            + "\n".join(summary_parts[:5])  # max 5 summaries
        )

        summary_msg = {"role": "system", "content": summary_text}
        result = head + [summary_msg] + tail
    else:
        result = head + tail

    return result


async def store_summary(bridge, topic: str, content: str) -> None:
    """Persist a conversation summary to long-term memory (best-effort)."""
    try:
        await bridge.call("store_memory", {
            "topic": topic,
            "content": content,
        })
    except Exception as e:
        print(f"  WARNING: failed to store summary: {e}")
