# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Decisions namespace — log and retrieve agent decisions."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace
from ..models import Decision


class DecisionsNamespace(BaseNamespace):

    async def log(self, decision: str, reasoning: str = "", alternatives: str | None = None, outcome: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"decision": decision, "reasoning": reasoning}
        if alternatives:
            params["alternatives"] = alternatives
        if outcome:
            params["outcome"] = outcome
        return await self._call("log_decision", params)

    async def list(self, limit: int = 20) -> list[Decision]:
        data = await self._call("get_decisions", {"limit": limit})
        decisions = data.get("decisions", []) if isinstance(data, dict) else []
        return [Decision.model_validate(d) for d in decisions]
