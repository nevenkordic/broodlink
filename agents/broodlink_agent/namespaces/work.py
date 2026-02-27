# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Work namespace — log work entries."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace
from ..models import WorkLog


class WorkNamespace(BaseNamespace):

    async def log(self, agent_name: str, action: str, details: str, files_changed: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"agent_name": agent_name, "action": action, "details": details}
        if files_changed:
            params["files_changed"] = files_changed
        return await self._call("log_work", params)

    async def list(self, limit: int = 20, agent_id: str | None = None) -> list[WorkLog]:
        params: dict[str, Any] = {"limit": limit}
        if agent_id:
            params["agent_id"] = agent_id
        data = await self._call("get_work_log", params)
        entries = data.get("entries", data.get("work_log", [])) if isinstance(data, dict) else []
        return [WorkLog.model_validate(e) for e in entries]
