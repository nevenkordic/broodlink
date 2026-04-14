# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Broodlink Python SDK — typed clients, namespaces, and agent framework."""

__version__ = "0.11.0"

from .config import AgentConfig
from .client import AsyncBroodlinkClient, BroodlinkClient
from .agent import BaseAgent
from .nats_helper import NATSHelper
from .transcript import TranscriptManager
from .deferred_init import TrustGate, TrustLevel, DeferredInitManager
from .events import (
    StreamEvent,
    StreamBuilder,
    EventSource,
    EventType,
    StreamStatus,
    parse_sse_stream,
    parse_sse_stream_sync,
)
from .models import (
    Memory,
    SemanticResult,
    MemoryStats,
    Task,
    Project,
    Agent,
    Decision,
    WorkLog,
    Message,
    KGEntity,
    KGEdge,
    Issue,
    Formula,
    Notification,
    ScheduledTask,
)

__all__ = [
    # Clients
    "AsyncBroodlinkClient",
    "BroodlinkClient",
    # Agent framework
    "BaseAgent",
    "NATSHelper",
    # Streaming event protocol
    "StreamEvent",
    "StreamBuilder",
    "EventSource",
    "EventType",
    "StreamStatus",
    "parse_sse_stream",
    "parse_sse_stream_sync",
    # Transcript compaction
    "TranscriptManager",
    # Trust-gated deferred initialization
    "TrustGate",
    "TrustLevel",
    "DeferredInitManager",
    # Config
    "AgentConfig",
    # Models
    "Memory",
    "SemanticResult",
    "MemoryStats",
    "Task",
    "Project",
    "Agent",
    "Decision",
    "WorkLog",
    "Message",
    "KGEntity",
    "KGEdge",
    "Issue",
    "Formula",
    "Notification",
    "ScheduledTask",
]
