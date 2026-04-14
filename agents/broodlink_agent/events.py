# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Formalized streaming event protocol for Broodlink agents.

Provides typed event classes matching the Rust ``broodlink-events`` crate so
that Python agents can both emit and consume the same event stream as the
Rust services.

Typical event lifecycle for a tool call::

    StreamStart → ToolMatch → ToolExecStart → MessageDelta* → ToolExecEnd → StreamEnd

Typical event lifecycle for a task::

    StreamStart → RouteStart → AgentMatch → TaskDispatched → MessageDelta* → TaskCompleted → StreamEnd
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from enum import Enum
from typing import Any, AsyncIterator, Iterator


# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------

class EventSource(str, Enum):
    BEADS_BRIDGE = "beads_bridge"
    COORDINATOR = "coordinator"
    A2A_GATEWAY = "a2a_gateway"
    STATUS_API = "status_api"
    EMBEDDING_WORKER = "embedding_worker"
    HEARTBEAT = "heartbeat"

    @classmethod
    def agent(cls, agent_id: str) -> dict[str, str]:
        """Return a source dict for an agent (matches Rust ``Agent(String)`` variant)."""
        return {"agent": agent_id}


class StreamStatus(str, Enum):
    SUCCESS = "success"
    PARTIAL = "partial"
    FAILED = "failed"
    CANCELLED = "cancelled"


class EventType(str, Enum):
    """All event type discriminants matching the Rust ``EventKind`` variants."""
    STREAM_START = "stream_start"
    STREAM_END = "stream_end"
    STREAM_ERROR = "stream_error"
    ROUTE_START = "route_start"
    AGENT_MATCH = "agent_match"
    TASK_DISPATCHED = "task_dispatched"
    TASK_COMPLETED = "task_completed"
    TASK_FAILED = "task_failed"
    TOOL_MATCH = "tool_match"
    TOOL_EXEC_START = "tool_exec_start"
    TOOL_EXEC_END = "tool_exec_end"
    MESSAGE_DELTA = "message_delta"
    MESSAGE_COMPLETE = "message_complete"
    PERMISSION_DENIED = "permission_denied"
    GUARDRAIL_TRIGGERED = "guardrail_triggered"
    BUDGET_DEBIT = "budget_debit"
    BUDGET_EXHAUSTED = "budget_exhausted"


# ---------------------------------------------------------------------------
# Event envelope
# ---------------------------------------------------------------------------

@dataclass
class StreamEvent:
    """Universal event envelope matching the Rust ``StreamEvent`` struct."""

    event_id: str
    stream_id: str
    sequence: int
    timestamp: str
    source: str | dict[str, str]
    kind: dict[str, Any]

    def to_json(self) -> str:
        """Serialize to JSON matching the Rust serde output."""
        return json.dumps(asdict(self), default=str)

    @property
    def event_type(self) -> str:
        """Extract the ``type`` field from the kind payload."""
        return self.kind.get("type", "unknown")

    @classmethod
    def from_json(cls, raw: str | bytes) -> StreamEvent:
        """Deserialize from a JSON string (SSE data line or NATS payload)."""
        data = json.loads(raw)
        return cls(
            event_id=data["event_id"],
            stream_id=data["stream_id"],
            sequence=data["sequence"],
            timestamp=data["timestamp"],
            source=data["source"],
            kind=data["kind"],
        )


# ---------------------------------------------------------------------------
# Stream builder — mirrors Rust StreamBuilder
# ---------------------------------------------------------------------------

class StreamBuilder:
    """Tracks sequence numbering for a single event stream.

    Usage::

        builder = StreamBuilder("my-stream", EventSource.BEADS_BRIDGE)
        start = builder.emit(EventType.STREAM_START, {
            "operation": "tool:store_memory",
            "agent_id": "worker-1",
        })
        # send start.to_json() over NATS / SSE
    """

    def __init__(self, stream_id: str, source: str | dict[str, str]):
        self.stream_id = stream_id
        self.source = source
        self._sequence = 0

    def emit(self, event_type: EventType | str, data: dict[str, Any] | None = None) -> StreamEvent:
        """Create the next event in sequence."""
        kind = {"type": str(event_type.value if isinstance(event_type, EventType) else event_type)}
        if data:
            kind["data"] = data

        event = StreamEvent(
            event_id=str(uuid.uuid4()),
            stream_id=self.stream_id,
            sequence=self._sequence,
            timestamp=datetime.now(timezone.utc).isoformat(),
            source=self.source,
            kind=kind,
        )
        self._sequence += 1
        return event

    @property
    def next_sequence(self) -> int:
        return self._sequence


# ---------------------------------------------------------------------------
# Async event stream consumer
# ---------------------------------------------------------------------------

async def parse_sse_stream(lines: AsyncIterator[str | bytes]) -> AsyncIterator[StreamEvent]:
    """Parse an SSE byte/text stream into typed StreamEvent objects.

    Handles the standard SSE format::

        event: tool_match
        data: {"event_id":"...","stream_id":"...","sequence":0,...}

    Yields ``StreamEvent`` instances. Silently skips malformed lines.
    """
    async for line in lines:
        text = line.decode("utf-8") if isinstance(line, bytes) else line
        text = text.strip()
        if text.startswith("data:"):
            payload = text[5:].strip()
            if not payload:
                continue
            try:
                yield StreamEvent.from_json(payload)
            except (json.JSONDecodeError, KeyError):
                continue


def parse_sse_stream_sync(lines: Iterator[str | bytes]) -> Iterator[StreamEvent]:
    """Synchronous version of :func:`parse_sse_stream`."""
    for line in lines:
        text = line.decode("utf-8") if isinstance(line, bytes) else line
        text = text.strip()
        if text.startswith("data:"):
            payload = text[5:].strip()
            if not payload:
                continue
            try:
                yield StreamEvent.from_json(payload)
            except (json.JSONDecodeError, KeyError):
                continue
