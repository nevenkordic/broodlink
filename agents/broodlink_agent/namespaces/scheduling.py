# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Scheduling namespace — scheduled tasks."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace


class SchedulingNamespace(BaseNamespace):

    async def schedule(
        self,
        title: str,
        run_at: str,
        description: str = "",
        priority: int = 0,
        formula_name: str | None = None,
        recurrence_secs: int | None = None,
        max_runs: int | None = None,
    ) -> dict[str, Any]:
        params: dict[str, Any] = {"title": title, "run_at": run_at}
        if description:
            params["description"] = description
        if priority:
            params["priority"] = priority
        if formula_name:
            params["formula_name"] = formula_name
        if recurrence_secs is not None:
            params["recurrence_secs"] = recurrence_secs
        if max_runs is not None:
            params["max_runs"] = max_runs
        return await self._call("schedule_task", params)

    async def list(self, limit: int = 50) -> list[dict[str, Any]]:
        data = await self._call("list_scheduled_tasks", {"limit": limit})
        return data.get("scheduled_tasks", []) if isinstance(data, dict) else []

    async def cancel(self, task_id: int) -> dict[str, Any]:
        return await self._call("cancel_scheduled_task", {"id": task_id})
