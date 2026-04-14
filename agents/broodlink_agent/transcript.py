# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Transcript compaction — context window management for long-running agents.

When an agent executes multi-turn tool loops over extended periods, the message
history can grow beyond the LLM's context window.  This module provides a
``TranscriptManager`` that:

1.  Tracks the running message list with approximate token counts.
2.  Automatically compacts older messages when the budget is approached:
    keeps the system prompt, the last N messages, and a generated summary
    of everything that was evicted.
3.  Optionally flushes evicted turns to disk for later auditing.

Usage::

    mgr = TranscriptManager(max_tokens=8000, keep_last=10)
    mgr.append({"role": "user", "content": "..."})
    mgr.append({"role": "assistant", "content": "..."})

    # When context is getting large, compact:
    messages = mgr.compacted_messages()
"""

from __future__ import annotations

import json
import os
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


# ---------------------------------------------------------------------------
# Token estimation
# ---------------------------------------------------------------------------

def estimate_tokens(text: str) -> int:
    """Rough token estimate: ~4 characters per token for English text.

    This is intentionally cheap — no tokenizer dependency required.  For
    production accuracy, swap this with tiktoken or a model-specific counter.
    """
    return max(1, len(text) // 4)


def message_tokens(msg: dict[str, Any]) -> int:
    """Estimate the token count for a single chat message."""
    content = msg.get("content", "")
    if isinstance(content, str):
        return estimate_tokens(content) + 4  # overhead for role, etc.
    # For tool_call messages or structured content, serialize and count
    return estimate_tokens(json.dumps(content, default=str)) + 4


# ---------------------------------------------------------------------------
# Transcript Manager
# ---------------------------------------------------------------------------

@dataclass
class TranscriptManager:
    """Manages message history with automatic compaction for long-running agents.

    Parameters
    ----------
    max_tokens
        Approximate token budget for the message history.
    keep_last
        Number of recent messages to always preserve during compaction.
    flush_dir
        Optional directory to write evicted messages as JSON (for audit).
        If None, evicted messages are discarded.
    """

    max_tokens: int = 8000
    keep_last: int = 10
    flush_dir: str | None = None

    # Internal state
    _messages: list[dict[str, Any]] = field(default_factory=list, repr=False)
    _token_counts: list[int] = field(default_factory=list, repr=False)
    _total_tokens: int = field(default=0, repr=False)
    _compaction_count: int = field(default=0, repr=False)
    _session_id: str = field(default_factory=lambda: str(uuid.uuid4()), repr=False)

    # -- Public API --

    def append(self, message: dict[str, Any]) -> None:
        """Add a message to the transcript and update the token estimate."""
        tokens = message_tokens(message)
        self._messages.append(message)
        self._token_counts.append(tokens)
        self._total_tokens += tokens

    def extend(self, messages: list[dict[str, Any]]) -> None:
        """Add multiple messages at once."""
        for msg in messages:
            self.append(msg)

    @property
    def messages(self) -> list[dict[str, Any]]:
        """Current message list (uncompacted)."""
        return list(self._messages)

    @property
    def total_tokens(self) -> int:
        """Approximate total tokens across all messages."""
        return self._total_tokens

    @property
    def message_count(self) -> int:
        return len(self._messages)

    @property
    def needs_compaction(self) -> bool:
        """True when the transcript exceeds 80% of the token budget."""
        return self._total_tokens > int(self.max_tokens * 0.8)

    def compacted_messages(self, summarizer: Any = None) -> list[dict[str, Any]]:
        """Return the message list, compacting if necessary.

        If the transcript exceeds the token budget, older messages (beyond
        ``keep_last``) are evicted and replaced with a summary message.

        Parameters
        ----------
        summarizer
            Optional callable ``(list[dict]) -> str`` that produces a
            summary of evicted messages.  If None, a simple metadata summary
            is generated.

        Returns
        -------
        list[dict]
            Messages suitable for sending to the LLM.
        """
        if not self.needs_compaction:
            return list(self._messages)

        return self._compact(summarizer)

    def flush_to_disk(self, messages: list[dict[str, Any]] | None = None) -> str | None:
        """Write messages to the flush directory as a JSON file.

        Returns the path of the written file, or None if flushing is disabled.
        """
        if not self.flush_dir:
            return None

        os.makedirs(self.flush_dir, exist_ok=True)
        timestamp = int(time.time())
        filename = f"transcript_{self._session_id}_{timestamp}_{self._compaction_count}.json"
        path = os.path.join(self.flush_dir, filename)

        payload = {
            "session_id": self._session_id,
            "compaction_index": self._compaction_count,
            "timestamp": timestamp,
            "messages": messages or self._messages,
        }

        with open(path, "w", encoding="utf-8") as f:
            json.dump(payload, f, indent=2, default=str)

        return path

    def clear(self) -> None:
        """Reset the transcript (e.g. for a new conversation turn)."""
        self._messages.clear()
        self._token_counts.clear()
        self._total_tokens = 0

    def usage_summary(self) -> dict[str, Any]:
        """Return a diagnostic summary of current transcript state."""
        return {
            "session_id": self._session_id,
            "message_count": len(self._messages),
            "total_tokens": self._total_tokens,
            "max_tokens": self.max_tokens,
            "utilization_pct": round(self._total_tokens / max(1, self.max_tokens) * 100, 1),
            "compactions_performed": self._compaction_count,
            "needs_compaction": self.needs_compaction,
        }

    # -- Internal --

    def _compact(self, summarizer: Any = None) -> list[dict[str, Any]]:
        """Perform compaction: evict old messages, keep system + last N."""

        # Separate system messages (always kept) from the rest
        system_msgs = [m for m in self._messages if m.get("role") == "system"]
        non_system = [m for m in self._messages if m.get("role") != "system"]

        if len(non_system) <= self.keep_last:
            # Not enough messages to compact
            return list(self._messages)

        # Split into evicted and kept
        evict_count = len(non_system) - self.keep_last
        evicted = non_system[:evict_count]
        kept = non_system[evict_count:]

        # Flush evicted to disk if configured
        if self.flush_dir:
            self.flush_to_disk(evicted)

        # Generate summary of evicted messages
        if summarizer:
            summary_text = summarizer(evicted)
        else:
            summary_text = self._default_summary(evicted)

        summary_msg: dict[str, Any] = {
            "role": "system",
            "content": (
                f"[Transcript compacted: {len(evicted)} earlier messages summarised]\n\n"
                f"{summary_text}"
            ),
        }

        # Rebuild internal state
        result = system_msgs + [summary_msg] + kept
        self._messages = list(result)
        self._token_counts = [message_tokens(m) for m in self._messages]
        self._total_tokens = sum(self._token_counts)
        self._compaction_count += 1

        return list(self._messages)

    @staticmethod
    def _default_summary(evicted: list[dict[str, Any]]) -> str:
        """Generate a simple summary of evicted messages without an LLM."""
        roles: dict[str, int] = {}
        tool_calls: list[str] = []

        for msg in evicted:
            role = msg.get("role", "unknown")
            roles[role] = roles.get(role, 0) + 1

            # Track tool calls if present
            for tc in msg.get("tool_calls", []):
                func = tc.get("function", {})
                name = func.get("name", "unknown")
                if name not in tool_calls:
                    tool_calls.append(name)

        parts = [f"Prior conversation contained {len(evicted)} messages."]

        role_summary = ", ".join(f"{count} {role}" for role, count in sorted(roles.items()))
        parts.append(f"Message breakdown: {role_summary}.")

        if tool_calls:
            parts.append(f"Tools invoked: {', '.join(tool_calls[:15])}.")

        # Include the last evicted user message as context anchor
        last_user = None
        for msg in reversed(evicted):
            if msg.get("role") == "user":
                content = msg.get("content", "")
                if isinstance(content, str) and content:
                    last_user = content[:200]
                break
        if last_user:
            parts.append(f"Last evicted user message: \"{last_user}\"")

        return " ".join(parts)
