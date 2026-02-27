# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Projects namespace — add, update, list projects."""

from __future__ import annotations

from typing import Any, Optional

from .base import BaseNamespace
from ..models import Project


class ProjectsNamespace(BaseNamespace):

    async def add(self, name: str, description: str, status: str = "planning", tags: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"name": name, "description": description, "status": status}
        if tags:
            params["tags"] = tags
        return await self._call("add_project", params)

    async def update(self, project_id: int, status: str | None = None, description: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"project_id": project_id}
        if status:
            params["status"] = status
        if description:
            params["description"] = description
        return await self._call("update_project", params)

    async def list(self, status: str | None = None) -> list[Project]:
        params: dict[str, Any] = {}
        if status:
            params["status"] = status
        data = await self._call("list_projects", params)
        projects = data.get("projects", []) if isinstance(data, dict) else []
        return [Project.model_validate(p) for p in projects]
