# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""HTTP client for beads-bridge tool calls with retry and circuit breaker."""

from __future__ import annotations

import asyncio
import time
import uuid

import aiohttp

from .config import AgentConfig

MAX_RETRIES = 3
RETRY_DELAY = 1.0

# Circuit breaker settings
CB_FAILURE_THRESHOLD = 5
CB_RECOVERY_TIMEOUT = 30.0  # seconds


class CircuitBreaker:
    """Simple circuit breaker: opens after N consecutive failures, auto-recovers."""

    def __init__(
        self,
        failure_threshold: int = CB_FAILURE_THRESHOLD,
        recovery_timeout: float = CB_RECOVERY_TIMEOUT,
    ) -> None:
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.failures = 0
        self.last_failure_time = 0.0
        self.state = "closed"  # closed, open, half-open

    def record_success(self) -> None:
        self.failures = 0
        self.state = "closed"

    def record_failure(self) -> None:
        self.failures += 1
        self.last_failure_time = time.monotonic()
        if self.failures >= self.failure_threshold:
            self.state = "open"

    def allow_request(self) -> bool:
        if self.state == "closed":
            return True
        if self.state == "open":
            elapsed = time.monotonic() - self.last_failure_time
            if elapsed >= self.recovery_timeout:
                self.state = "half-open"
                return True
            return False
        # half-open: allow one probe request
        return True


class BridgeClient:
    """Calls beads-bridge tools with JWT auth, retry, and circuit breaker."""

    def __init__(self, session: aiohttp.ClientSession, config: AgentConfig) -> None:
        self.session = session
        self.config = config
        self.circuit_breaker = CircuitBreaker()

    async def call(self, tool_name: str, params: dict) -> dict:
        """Call a beads-bridge tool with retry + circuit breaker. Returns response data dict."""
        if not self.circuit_breaker.allow_request():
            return {"error": "circuit breaker open — bridge unreachable"}

        for attempt in range(MAX_RETRIES):
            try:
                trace_id = str(uuid.uuid4())
                async with self.session.post(
                    f"{self.config.bridge_url}/{tool_name}",
                    json={"params": params},
                    headers={
                        "Authorization": f"Bearer {self.config.agent_jwt}",
                        "Content-Type": "application/json",
                        "X-Trace-Id": trace_id,
                    },
                    timeout=aiohttp.ClientTimeout(total=30),
                ) as resp:
                    body = await resp.json()
                    if resp.status != 200:
                        self.circuit_breaker.record_failure()
                        return {"error": body.get("error", f"HTTP {resp.status}")}
                    self.circuit_breaker.record_success()
                    return body.get("data", body)
            except (aiohttp.ClientError, asyncio.TimeoutError) as e:
                self.circuit_breaker.record_failure()
                if attempt < MAX_RETRIES - 1:
                    await asyncio.sleep(RETRY_DELAY * (attempt + 1))
                else:
                    return {"error": f"bridge unreachable after {MAX_RETRIES} attempts: {e}"}
        return {"error": "unexpected retry exhaustion"}

    async def fetch_tools(self) -> list[dict]:
        """Fetch tool definitions from the bridge's public /api/v1/tools endpoint."""
        try:
            async with self.session.get(
                self.config.tools_url,
                timeout=aiohttp.ClientTimeout(total=10),
            ) as resp:
                if resp.status != 200:
                    print(f"WARNING: Could not fetch tools (HTTP {resp.status}).")
                    return []
                body = await resp.json()
                return body.get("tools", [])
        except (aiohttp.ClientError, asyncio.TimeoutError) as e:
            print(f"WARNING: Could not fetch tools: {e}")
            return []
