# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""High-level Broodlink clients with typed namespace objects.

Usage (async)::

    async with AsyncBroodlinkClient() as client:
        await client.memory.store("topic", "content")
        results = await client.memory.search("query")

Usage (sync, safe in Jupyter)::

    client = BroodlinkClient()
    client.memory.store("topic", "content")
    results = client.memory.search("query")
    client.close()
"""

from __future__ import annotations

import asyncio
from typing import Any

from .config import AgentConfig
from ._transport import AsyncTransport, SyncTransport
from .namespaces import (
    MemoryNamespace,
    TasksNamespace,
    ProjectsNamespace,
    AgentsNamespace,
    BeadsNamespace,
    KnowledgeGraphNamespace,
    MessagingNamespace,
    DecisionsNamespace,
    WorkNamespace,
    SchedulingNamespace,
    NotificationsNamespace,
)


class AsyncBroodlinkClient:
    """Async client with typed namespace objects.

    Use as an async context manager::

        async with AsyncBroodlinkClient() as client:
            stats = await client.memory.stats()
    """

    def __init__(self, config: AgentConfig | None = None):
        self.config = config or AgentConfig.from_env()
        self._transport = AsyncTransport(self.config)

        # Wire up namespaces
        self.memory = MemoryNamespace(self._transport)
        self.tasks = TasksNamespace(self._transport)
        self.projects = ProjectsNamespace(self._transport)
        self.agents = AgentsNamespace(self._transport)
        self.beads = BeadsNamespace(self._transport)
        self.kg = KnowledgeGraphNamespace(self._transport)
        self.messaging = MessagingNamespace(self._transport)
        self.decisions = DecisionsNamespace(self._transport)
        self.work = WorkNamespace(self._transport)
        self.scheduling = SchedulingNamespace(self._transport)
        self.notifications = NotificationsNamespace(self._transport)

    async def call(self, tool_name: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """Call any beads-bridge tool directly by name."""
        return await self._transport.call(tool_name, params)

    async def fetch_tools(self) -> list[dict[str, Any]]:
        """Fetch available tool definitions."""
        return await self._transport.fetch_tools()

    async def __aenter__(self) -> "AsyncBroodlinkClient":
        await self._transport.__aenter__()
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self._transport.__aexit__(*args)

    async def close(self) -> None:
        await self._transport.close()


class BroodlinkClient:
    """Synchronous client with typed namespace objects.

    Uses a background thread event loop, safe for Jupyter notebooks::

        client = BroodlinkClient()
        stats = client.memory.stats()
        client.close()
    """

    def __init__(self, config: AgentConfig | None = None):
        self.config = config or AgentConfig.from_env()
        self._transport = SyncTransport(self.config)
        loop = self._transport._loop

        # Wire up sync-wrapped namespaces
        self.memory = _SyncNamespace(MemoryNamespace(self._transport._async_transport), loop)
        self.tasks = _SyncNamespace(TasksNamespace(self._transport._async_transport), loop)
        self.projects = _SyncNamespace(ProjectsNamespace(self._transport._async_transport), loop)
        self.agents = _SyncNamespace(AgentsNamespace(self._transport._async_transport), loop)
        self.beads = _SyncNamespace(BeadsNamespace(self._transport._async_transport), loop)
        self.kg = _SyncNamespace(KnowledgeGraphNamespace(self._transport._async_transport), loop)
        self.messaging = _SyncNamespace(MessagingNamespace(self._transport._async_transport), loop)
        self.decisions = _SyncNamespace(DecisionsNamespace(self._transport._async_transport), loop)
        self.work = _SyncNamespace(WorkNamespace(self._transport._async_transport), loop)
        self.scheduling = _SyncNamespace(SchedulingNamespace(self._transport._async_transport), loop)
        self.notifications = _SyncNamespace(NotificationsNamespace(self._transport._async_transport), loop)

    def call(self, tool_name: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """Call any beads-bridge tool directly by name."""
        return self._transport.call(tool_name, params)

    def fetch_tools(self) -> list[dict[str, Any]]:
        """Fetch available tool definitions."""
        return self._transport.fetch_tools()

    def close(self) -> None:
        self._transport.close()

    def __enter__(self) -> "BroodlinkClient":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


class _SyncNamespace:
    """Proxy that wraps async namespace methods into synchronous calls."""

    def __init__(self, async_ns: Any, loop: asyncio.AbstractEventLoop):
        self._ns = async_ns
        self._loop = loop

    def __getattr__(self, name: str) -> Any:
        attr = getattr(self._ns, name)
        if not callable(attr):
            return attr

        def wrapper(*args: Any, **kwargs: Any) -> Any:
            coro = attr(*args, **kwargs)
            future = asyncio.run_coroutine_threadsafe(coro, self._loop)
            return future.result(timeout=120)

        return wrapper
