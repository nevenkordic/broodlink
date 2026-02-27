# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Broodlink Python SDK — typed clients, namespaces, and agent framework."""

__version__ = "0.12.0"

from .config import AgentConfig
from .client import AsyncBroodlinkClient, BroodlinkClient
from .agent import BaseAgent
from .nats_helper import NATSHelper
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
