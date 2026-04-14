# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Trust-gated deferred initialization for agent onboarding.

When a new agent connects to Broodlink, not all capabilities should be
available immediately.  High-privilege tools (shell access, file I/O,
workflow triggers, budget management) should only be unlocked after the
agent passes a trust gate — either via explicit approval policy, a
successful probation period, or a trust tier check.

This module provides:

1.  ``TrustGate`` — classifies tools into trust tiers and filters the
    available set based on the agent's current trust level.
2.  ``DeferredInitManager`` — orchestrates the initialization pipeline,
    loading capabilities in phases as trust is established.

Usage::

    gate = TrustGate(agent_trust_level=TrustLevel.STANDARD)
    safe_tools = gate.filter_tools(all_bridge_tools)

    # Later, after the agent proves itself:
    gate.promote(TrustLevel.ELEVATED)
    more_tools = gate.filter_tools(all_bridge_tools)
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable, Awaitable


# ---------------------------------------------------------------------------
# Trust levels
# ---------------------------------------------------------------------------

class TrustLevel(IntEnum):
    """Agent trust tiers — higher values unlock more capabilities."""

    UNTRUSTED = 0
    """New agent, not yet validated. Read-only tools only."""

    PROBATION = 1
    """Agent registered but in probation period. Safe tools available."""

    STANDARD = 2
    """Verified agent. Most tools available except admin operations."""

    ELEVATED = 3
    """Trusted agent with admin-level tool access."""

    SYSTEM = 4
    """Internal system agent — unrestricted."""


# ---------------------------------------------------------------------------
# Tool classification by trust tier
# ---------------------------------------------------------------------------

# Tools requiring elevated trust (destructive, admin, or sensitive operations)
_ELEVATED_TOOLS: frozenset[str] = frozenset({
    # Budget / admin
    "budget_set", "budget_replenish", "agent_deactivate",
    # Workflow management
    "run_formula", "cancel_workflow", "delete_formula",
    # Task admin
    "cancel_task", "reassign_task",
    # Notification / webhook admin
    "create_webhook", "delete_webhook", "create_notification_rule",
})

# Tools requiring at least standard trust
_STANDARD_TOOLS: frozenset[str] = frozenset({
    # Write operations
    "store_memory", "delete_memory", "log_decision", "log_work",
    "create_task", "complete_task", "fail_task", "claim_task",
    "add_project", "update_project",
    "send_message", "agent_upsert",
    "add_kg_entity", "add_kg_edge",
    "schedule_task", "cancel_scheduled_task",
    # File operations
    "store_attachment", "delete_attachment",
    # Issue tracking
    "create_issue", "update_issue_status",
})

# Tools safe for probation agents (read-only operations)
_PROBATION_TOOLS: frozenset[str] = frozenset({
    "recall_memory", "search_memory", "memory_stats",
    "get_task", "list_tasks",
    "list_projects",
    "list_agents", "get_agent",
    "get_kg_entity", "kg_neighbors", "kg_search",
    "read_messages",
    "list_decisions",
    "list_work_logs",
    "list_issues", "get_issue",
    "list_formulas", "get_convoy",
    "list_scheduled_tasks",
    "list_notification_rules",
    "get_attachment",
})

# Tool prefixes for deny-matching (any tool starting with these is elevated)
_ELEVATED_PREFIXES: tuple[str, ...] = (
    "admin_", "system_", "debug_", "unsafe_",
)


def classify_tool_trust(tool_name: str) -> TrustLevel:
    """Determine the minimum trust level required for a tool.

    Classification order:
    1. Explicit elevated set → ELEVATED
    2. Elevated prefix match → ELEVATED
    3. Explicit standard set → STANDARD
    4. Explicit probation set → PROBATION
    5. Everything else → STANDARD (default-safe)
    """
    lower = tool_name.lower()

    if lower in _ELEVATED_TOOLS or any(lower.startswith(p) for p in _ELEVATED_PREFIXES):
        return TrustLevel.ELEVATED

    if lower in _STANDARD_TOOLS:
        return TrustLevel.STANDARD

    if lower in _PROBATION_TOOLS:
        return TrustLevel.PROBATION

    # Default: require standard trust for unclassified tools
    return TrustLevel.STANDARD


# ---------------------------------------------------------------------------
# Trust gate — filters tools based on agent trust level
# ---------------------------------------------------------------------------

@dataclass
class TrustGate:
    """Filters available tools based on the agent's trust level.

    Parameters
    ----------
    agent_trust_level
        Current trust level of the agent.
    custom_deny
        Additional tool names to always deny regardless of trust level.
    custom_allow
        Tool names to explicitly allow even if they would normally be denied
        at this trust level. Useful for granting specific exceptions.
    """

    agent_trust_level: TrustLevel = TrustLevel.STANDARD
    custom_deny: frozenset[str] = field(default_factory=frozenset)
    custom_allow: frozenset[str] = field(default_factory=frozenset)

    def allows(self, tool_name: str) -> bool:
        """Check whether the current trust level permits use of a tool."""
        lower = tool_name.lower()

        # Custom deny always takes precedence
        if lower in self.custom_deny:
            return False

        # Custom allow overrides tier-based blocking
        if lower in self.custom_allow:
            return True

        required = classify_tool_trust(tool_name)
        return self.agent_trust_level >= required

    def filter_tools(self, tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Return only the tools this agent is permitted to use.

        Expects tool dicts with a ``function.name`` key (OpenAI tool format)
        or a top-level ``name`` key.
        """
        result = []
        for tool in tools:
            name = tool.get("function", {}).get("name") or tool.get("name", "")
            if self.allows(name):
                result.append(tool)
        return result

    def denied_tools(self, tools: list[dict[str, Any]]) -> list[str]:
        """Return the names of tools that would be blocked at this trust level."""
        denied = []
        for tool in tools:
            name = tool.get("function", {}).get("name") or tool.get("name", "")
            if not self.allows(name):
                denied.append(name)
        return denied

    def promote(self, new_level: TrustLevel) -> None:
        """Promote the agent to a higher trust level."""
        if new_level > self.agent_trust_level:
            self.agent_trust_level = new_level

    def demote(self, new_level: TrustLevel) -> None:
        """Demote the agent to a lower trust level."""
        if new_level < self.agent_trust_level:
            self.agent_trust_level = new_level


# ---------------------------------------------------------------------------
# Deferred initialization manager
# ---------------------------------------------------------------------------

InitHook = Callable[[], Awaitable[None]]


@dataclass
class _DeferredPhase:
    """A named initialization phase with a trust requirement."""
    name: str
    required_trust: TrustLevel
    hook: InitHook
    completed: bool = False
    started_at: float | None = None
    completed_at: float | None = None
    error: str | None = None


class DeferredInitManager:
    """Orchestrates phased agent initialization based on trust gates.

    Register initialization hooks for different trust levels. When the
    agent's trust changes, call ``run_eligible()`` to execute any newly
    unlocked phases.

    Usage::

        mgr = DeferredInitManager(trust_gate)

        mgr.register("load_read_tools", TrustLevel.PROBATION, load_read_tools)
        mgr.register("load_write_tools", TrustLevel.STANDARD, load_write_tools)
        mgr.register("load_admin_tools", TrustLevel.ELEVATED, load_admin_tools)

        # On agent start (probation level):
        await mgr.run_eligible()  # only load_read_tools runs

        # After trust promotion:
        trust_gate.promote(TrustLevel.STANDARD)
        await mgr.run_eligible()  # load_write_tools now runs
    """

    def __init__(self, trust_gate: TrustGate):
        self._gate = trust_gate
        self._phases: list[_DeferredPhase] = []

    @property
    def trust_gate(self) -> TrustGate:
        return self._gate

    def register(self, name: str, required_trust: TrustLevel, hook: InitHook) -> None:
        """Register a deferred initialization phase."""
        self._phases.append(_DeferredPhase(
            name=name,
            required_trust=required_trust,
            hook=hook,
        ))

    async def run_eligible(self) -> list[str]:
        """Execute all phases whose trust requirement is now met.

        Returns the names of phases that were executed this call.
        """
        executed: list[str] = []

        for phase in self._phases:
            if phase.completed:
                continue
            if self._gate.agent_trust_level < phase.required_trust:
                continue

            phase.started_at = time.time()
            try:
                await phase.hook()
                phase.completed = True
                phase.completed_at = time.time()
                executed.append(phase.name)
            except Exception as e:
                phase.error = str(e)
                # Don't mark as completed — allow retry

        return executed

    def status(self) -> list[dict[str, Any]]:
        """Return diagnostic status of all registered phases."""
        return [
            {
                "name": p.name,
                "required_trust": p.required_trust.name,
                "completed": p.completed,
                "started_at": p.started_at,
                "completed_at": p.completed_at,
                "error": p.error,
                "eligible": self._gate.agent_trust_level >= p.required_trust,
            }
            for p in self._phases
        ]

    def all_complete(self) -> bool:
        """True when every registered phase has completed."""
        return all(p.completed for p in self._phases)

    def pending_phases(self) -> list[str]:
        """Names of phases that haven't completed yet."""
        return [p.name for p in self._phases if not p.completed]
