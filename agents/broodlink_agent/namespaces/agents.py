# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Agents namespace — register, list, get agent profiles."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace
from ..models import Agent


class AgentsNamespace(BaseNamespace):

    async def upsert(self, agent_id: str, display_name: str, role: str, **kwargs: Any) -> dict[str, Any]:
        params: dict[str, Any] = {"agent_id": agent_id, "display_name": display_name, "role": role}
        params.update(kwargs)
        return await self._call("agent_upsert", params)

    async def list(self) -> list[Agent]:
        data = await self._call("list_agents")
        agents = data.get("agents", []) if isinstance(data, dict) else []
        return [Agent.model_validate(a) for a in agents]

    async def get(self, agent_id: str) -> Agent:
        data = await self._call("get_agent", {"agent_id": agent_id})
        return Agent.model_validate(data)
