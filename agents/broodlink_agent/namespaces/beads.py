# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Beads namespace — issues, convoys, formulas."""

from __future__ import annotations

from typing import Any, Optional

from .base import BaseNamespace
from ..models import Issue, Formula


class BeadsNamespace(BaseNamespace):

    async def list_issues(self, status: str | None = None, limit: int = 50) -> list[Issue]:
        params: dict[str, Any] = {"limit": limit}
        if status:
            params["status"] = status
        data = await self._call("beads_list_issues", params)
        issues = data.get("issues", []) if isinstance(data, dict) else []
        return [Issue.model_validate(i) for i in issues]

    async def get_issue(self, issue_id: str) -> Issue:
        data = await self._call("beads_get_issue", {"issue_id": issue_id})
        return Issue.model_validate(data)

    async def create_issue(self, title: str, description: str = "", **kwargs: Any) -> dict[str, Any]:
        params: dict[str, Any] = {"title": title, "description": description}
        params.update(kwargs)
        return await self._call("beads_create_issue", params)

    async def update_status(self, issue_id: str, status: str) -> dict[str, Any]:
        return await self._call("beads_update_status", {"issue_id": issue_id, "status": status})

    async def get_convoy(self, convoy_id: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {}
        if convoy_id:
            params["convoy_id"] = convoy_id
        return await self._call("beads_get_convoy", params)

    async def list_formulas(self) -> list[dict[str, Any]]:
        data = await self._call("beads_list_formulas")
        return data.get("formulas", []) if isinstance(data, dict) else []

    async def run_formula(self, formula: str, parameters: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"formula": formula}
        if parameters:
            params["parameters"] = parameters
        return await self._call("beads_run_formula", params)
