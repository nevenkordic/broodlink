# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Messaging namespace — send/read inter-agent messages."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace
from ..models import Message


class MessagingNamespace(BaseNamespace):

    async def send(self, to: str, content: str, subject: str = "direct") -> dict[str, Any]:
        return await self._call("send_message", {"to": to, "content": content, "subject": subject})

    async def read(self, unread_only: bool = False, limit: int = 20) -> list[Message]:
        data = await self._call("read_messages", {"unread_only": unread_only, "limit": limit})
        messages = data.get("messages", []) if isinstance(data, dict) else []
        return [Message.model_validate(m) for m in messages]
