# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""NATS helper — task subscriber, notification listener, event publisher."""

from __future__ import annotations

import asyncio
import json
from typing import Any, Callable, Awaitable

from .config import AgentConfig

MessageHandler = Callable[[dict[str, Any]], Awaitable[None]]


class NATSHelper:
    """Manages NATS subscriptions for an agent.

    Provides typed helpers for subscribing to task dispatches,
    delegations, and notifications::

        nats = NATSHelper(config)
        await nats.connect()
        await nats.subscribe_tasks(handle_task)
        await nats.subscribe_notifications(handle_notification)
        # ... later
        await nats.close()
    """

    def __init__(self, config: AgentConfig):
        self.config = config
        self._nc: Any = None
        self._subs: list[Any] = []

    async def connect(self) -> None:
        """Connect to NATS server."""
        import nats as nats_client
        self._nc = await nats_client.connect(self.config.nats_url)

    async def close(self) -> None:
        """Drain and close NATS connection."""
        for sub in self._subs:
            try:
                await sub.unsubscribe()
            except Exception:
                pass
        self._subs.clear()
        if self._nc:
            await self._nc.drain()
            self._nc = None

    def _subject(self, suffix: str) -> str:
        """Build a NATS subject from config prefix + env + suffix."""
        return f"{self.config.nats_prefix}.{self.config.env}.{suffix}"

    async def subscribe_tasks(self, handler: MessageHandler) -> None:
        """Subscribe to task dispatches for this agent."""
        subject = self._subject(f"agent.{self.config.agent_id}.task")
        sub = await self._nc.subscribe(subject)
        self._subs.append(sub)
        asyncio.create_task(self._listen(sub, handler))

    async def subscribe_delegations(self, handler: MessageHandler) -> None:
        """Subscribe to delegation messages for this agent."""
        subject = self._subject(f"agent.{self.config.agent_id}.delegation")
        sub = await self._nc.subscribe(subject)
        self._subs.append(sub)
        asyncio.create_task(self._listen(sub, handler))

    async def subscribe_notifications(self, handler: MessageHandler) -> None:
        """Subscribe to notification broadcasts."""
        subject = self._subject("notifications.>")
        sub = await self._nc.subscribe(subject)
        self._subs.append(sub)
        asyncio.create_task(self._listen(sub, handler))

    async def subscribe(self, subject_suffix: str, handler: MessageHandler) -> None:
        """Subscribe to an arbitrary subject suffix."""
        subject = self._subject(subject_suffix)
        sub = await self._nc.subscribe(subject)
        self._subs.append(sub)
        asyncio.create_task(self._listen(sub, handler))

    async def publish(self, subject_suffix: str, data: dict[str, Any]) -> None:
        """Publish a JSON message to a NATS subject."""
        subject = self._subject(subject_suffix)
        payload = json.dumps(data).encode()
        await self._nc.publish(subject, payload)

    async def _listen(self, sub: Any, handler: MessageHandler) -> None:
        """Background listener that dispatches messages to the handler."""
        try:
            async for msg in sub.messages:
                try:
                    data = json.loads(msg.data.decode())
                    await handler(data)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                except Exception:
                    continue
        except Exception:
            pass

    async def __aenter__(self) -> "NATSHelper":
        await self.connect()
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.close()
