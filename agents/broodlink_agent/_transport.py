# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Low-level async & sync transports for beads-bridge API calls."""

from __future__ import annotations

import asyncio
import json
import threading
import uuid
from typing import Any

import aiohttp

from .config import AgentConfig


class CircuitBreaker:
    """Simple circuit breaker: closed → open (after N failures) → half-open."""

    def __init__(self, failure_threshold: int = 5, recovery_timeout: float = 30.0):
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self._failures = 0
        self._state = "closed"
        self._last_failure: float = 0.0

    def record_success(self) -> None:
        self._failures = 0
        self._state = "closed"

    def record_failure(self) -> None:
        import time
        self._failures += 1
        self._last_failure = time.time()
        if self._failures >= self.failure_threshold:
            self._state = "open"

    def allow_request(self) -> bool:
        if self._state == "closed":
            return True
        if self._state == "open":
            import time
            if time.time() - self._last_failure > self.recovery_timeout:
                self._state = "half-open"
                return True
            return False
        # half-open: allow one probe
        return True


class AsyncTransport:
    """Async HTTP transport for beads-bridge tool calls.

    Usage::

        async with AsyncTransport(config) as t:
            result = await t.call("store_memory", {"topic": "x", "content": "y"})
    """

    def __init__(self, config: AgentConfig | None = None):
        self.config = config or AgentConfig.from_env()
        self._session: aiohttp.ClientSession | None = None
        self._cb = CircuitBreaker()

    async def __aenter__(self) -> "AsyncTransport":
        self._session = aiohttp.ClientSession(
            timeout=aiohttp.ClientTimeout(total=self.config.request_timeout)
        )
        return self

    async def __aexit__(self, *args: Any) -> None:
        if self._session:
            await self._session.close()
            self._session = None

    async def _ensure_session(self) -> aiohttp.ClientSession:
        if self._session is None:
            self._session = aiohttp.ClientSession(
                timeout=aiohttp.ClientTimeout(total=self.config.request_timeout)
            )
        return self._session

    async def call(self, tool_name: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """Call a beads-bridge tool by name. Returns the response data dict."""
        if not self._cb.allow_request():
            return {"error": "circuit breaker open"}

        session = await self._ensure_session()
        url = f"{self.config.bridge_url}/{tool_name}"
        headers = {"Content-Type": "application/json", "X-Trace-Id": str(uuid.uuid4())}
        if self.config.agent_jwt:
            headers["Authorization"] = f"Bearer {self.config.agent_jwt}"

        payload = {"params": params or {}}
        last_err = None

        for attempt in range(3):
            try:
                async with session.post(url, json=payload, headers=headers) as resp:
                    data = await resp.json()
                    if resp.status >= 400:
                        self._cb.record_failure()
                        return {"error": data.get("error", f"HTTP {resp.status}")}
                    self._cb.record_success()
                    return data.get("data", data)
            except Exception as e:
                last_err = e
                if attempt < 2:
                    await asyncio.sleep(1 * (attempt + 1))

        self._cb.record_failure()
        return {"error": str(last_err)}

    async def fetch_tools(self) -> list[dict[str, Any]]:
        """Fetch available tool definitions from beads-bridge."""
        session = await self._ensure_session()
        try:
            async with session.get(
                self.config.tools_url,
                headers={"Authorization": f"Bearer {self.config.agent_jwt}"} if self.config.agent_jwt else {},
                timeout=aiohttp.ClientTimeout(total=10),
            ) as resp:
                data = await resp.json()
                return data.get("tools", [])
        except Exception:
            return []

    async def close(self) -> None:
        if self._session:
            await self._session.close()
            self._session = None


class SyncTransport:
    """Synchronous wrapper around AsyncTransport using a background event loop.

    Safe to use in Jupyter notebooks and other environments where an event loop
    may already be running.

    Usage::

        t = SyncTransport()
        result = t.call("store_memory", {"topic": "x", "content": "y"})
        t.close()
    """

    def __init__(self, config: AgentConfig | None = None):
        self.config = config or AgentConfig.from_env()
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._loop.run_forever, daemon=True)
        self._thread.start()
        self._async_transport = asyncio.run_coroutine_threadsafe(
            self._create_transport(), self._loop
        ).result(timeout=10)

    async def _create_transport(self) -> AsyncTransport:
        t = AsyncTransport(self.config)
        await t.__aenter__()
        return t

    def call(self, tool_name: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        future = asyncio.run_coroutine_threadsafe(
            self._async_transport.call(tool_name, params), self._loop
        )
        return future.result(timeout=self.config.request_timeout + 5)

    def fetch_tools(self) -> list[dict[str, Any]]:
        future = asyncio.run_coroutine_threadsafe(
            self._async_transport.fetch_tools(), self._loop
        )
        return future.result(timeout=15)

    def close(self) -> None:
        asyncio.run_coroutine_threadsafe(
            self._async_transport.close(), self._loop
        ).result(timeout=5)
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=5)

    def __enter__(self) -> "SyncTransport":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()
