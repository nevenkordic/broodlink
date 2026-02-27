# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Notifications namespace — notification rules and sending."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace


class NotificationsNamespace(BaseNamespace):

    async def send(self, channel: str, target: str, message: str) -> dict[str, Any]:
        return await self._call("send_notification", {"channel": channel, "target": target, "message": message})

    async def create_rule(
        self,
        name: str,
        condition_type: str,
        channel: str,
        target: str,
        template: str | None = None,
        cooldown_minutes: int | None = None,
        condition_config: str | None = None,
    ) -> dict[str, Any]:
        params: dict[str, Any] = {
            "name": name,
            "condition_type": condition_type,
            "channel": channel,
            "target": target,
        }
        if template:
            params["template"] = template
        if cooldown_minutes is not None:
            params["cooldown_minutes"] = cooldown_minutes
        if condition_config:
            params["condition_config"] = condition_config
        return await self._call("create_notification_rule", params)

    async def list_rules(self) -> list[dict[str, Any]]:
        data = await self._call("list_notification_rules")
        if isinstance(data, dict):
            return data.get("notification_rules", data.get("rules", []))
        return []
