# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Core Pydantic models for Broodlink API responses.

All models use ``extra="allow"`` for forward compatibility with new fields.
"""

from __future__ import annotations

from typing import Any, Optional

from pydantic import BaseModel, Field


class _Base(BaseModel):
    model_config = {"extra": "allow"}


# ── Memory ───────────────────────────────────────────────────────────

class Memory(_Base):
    topic: str
    content: str
    tags: Optional[Any] = None  # str when storing, list[str] when retrieving
    agent_name: Optional[str] = None
    updated_at: Optional[str] = None


class SemanticResult(_Base):
    topic: str = ""
    content: str = ""
    score: float = 0.0
    agent_name: Optional[str] = None


class MemoryStats(_Base):
    total_count: int = 0
    most_recent: Optional[str] = None
    top_topics: list[dict[str, Any]] = Field(default_factory=list)


# ── Tasks ────────────────────────────────────────────────────────────

class Task(_Base):
    task_id: Optional[str] = Field(None, alias="id")
    title: str = ""
    description: str = ""
    status: str = "pending"
    priority: Optional[int] = None
    assigned_to: Optional[str] = None
    created_at: Optional[str] = None
    completed_at: Optional[str] = None


# ── Projects ─────────────────────────────────────────────────────────

class Project(_Base):
    id: Optional[int] = None
    name: str = ""
    description: str = ""
    status: str = "planning"
    tags: Optional[str] = None
    created_at: Optional[str] = None
    updated_at: Optional[str] = None


# ── Agents ───────────────────────────────────────────────────────────

class Agent(_Base):
    agent_id: str = ""
    display_name: str = ""
    role: str = ""
    active: bool = True
    capabilities: Optional[str] = None
    cost_tier: str = "medium"
    transport: str = "mcp"


# ── Decisions ────────────────────────────────────────────────────────

class Decision(_Base):
    id: Optional[int] = None
    decision: str = ""
    reasoning: str = ""
    alternatives: Optional[str] = None
    outcome: Optional[str] = None
    created_at: Optional[str] = None


# ── Work Log ─────────────────────────────────────────────────────────

class WorkLog(_Base):
    id: Optional[int] = None
    agent_name: str = ""
    action: str = ""
    details: str = ""
    files_changed: Optional[Any] = None  # str when storing, list[str] when retrieving
    created_at: Optional[str] = None


# ── Messages ─────────────────────────────────────────────────────────

class Message(_Base):
    model_config = {"extra": "allow", "populate_by_name": True}

    id: Optional[str] = None
    sender: Optional[str] = Field(None, alias="from")
    recipient: Optional[str] = None
    to: Optional[str] = None
    body: str = ""
    content: str = ""
    subject: str = "direct"
    status: str = "unread"
    created_at: Optional[str] = None


# ── Knowledge Graph ──────────────────────────────────────────────────

class KGEntity(_Base):
    id: Optional[int] = None
    name: str = ""
    entity_type: str = ""
    description: Optional[str] = None
    properties: Optional[dict[str, Any]] = None


class KGEdge(_Base):
    id: Optional[int] = None
    source_entity: str = ""
    target_entity: str = ""
    relation_type: str = ""
    weight: float = 1.0


# ── Beads / Issues ───────────────────────────────────────────────────

class Issue(_Base):
    id: Optional[str] = None
    title: str = ""
    description: str = ""
    status: str = "open"
    assignee: Optional[str] = None
    convoy_id: Optional[str] = None
    formula: Optional[str] = None
    created_at: Optional[str] = None


# ── Formulas ─────────────────────────────────────────────────────────

class Formula(_Base):
    id: Optional[str] = None
    name: str = ""
    display_name: str = ""
    description: str = ""
    definition: Optional[dict[str, Any]] = None
    is_system: bool = False
    enabled: bool = True
    version: int = 1
    usage_count: int = 0


# ── Proactive Skills ────────────────────────────────────────────────

class Notification(_Base):
    id: Optional[int] = None
    rule_id: Optional[int] = None
    channel: str = ""
    target: str = ""
    message: str = ""
    status: str = "pending"
    created_at: Optional[str] = None


class ScheduledTask(_Base):
    id: Optional[int] = None
    title: str = ""
    description: str = ""
    priority: int = 0
    formula_name: Optional[str] = None
    next_run_at: Optional[str] = None
    recurrence_secs: Optional[int] = None
    run_count: int = 0
    max_runs: Optional[int] = None
    created_at: Optional[str] = None
