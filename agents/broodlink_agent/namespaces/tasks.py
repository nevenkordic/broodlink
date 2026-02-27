# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tasks namespace — create, claim, complete, fail tasks."""

from __future__ import annotations

from typing import Any, Optional

from .base import BaseNamespace
from ..models import Task


class TasksNamespace(BaseNamespace):

    async def create(self, title: str, description: str = "", priority: int | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"title": title, "description": description}
        if priority is not None:
            params["priority"] = priority
        return await self._call("create_task", params)

    async def get(self, task_id: str) -> Task:
        data = await self._call("get_task", {"task_id": task_id})
        return Task.model_validate(data)

    async def list(self, status: str | None = None, limit: int = 50) -> list[Task]:
        params: dict[str, Any] = {"limit": limit}
        if status:
            params["status"] = status
        data = await self._call("list_tasks", params)
        tasks = data.get("tasks", []) if isinstance(data, dict) else []
        return [Task.model_validate(t) for t in tasks]

    async def claim(self, task_id: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {}
        if task_id:
            params["task_id"] = task_id
        return await self._call("claim_task", params)

    async def complete(self, task_id: str) -> dict[str, Any]:
        return await self._call("complete_task", {"task_id": task_id})

    async def fail(self, task_id: str) -> dict[str, Any]:
        return await self._call("fail_task", {"task_id": task_id})
