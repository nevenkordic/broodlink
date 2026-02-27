# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Batch operations for bulk data loading."""

from __future__ import annotations

import asyncio
from typing import Any

from .._transport import AsyncTransport


async def batch_store(
    transport: AsyncTransport,
    items: list[dict[str, Any]],
    tool_name: str = "store_memory",
    concurrency: int = 5,
) -> list[dict[str, Any]]:
    """Store multiple items concurrently via beads-bridge.

    Useful for bulk-loading memories, entities, or other records::

        async with AsyncBroodlinkClient() as client:
            items = [
                {"topic": "fact-1", "content": "Python is great"},
                {"topic": "fact-2", "content": "Rust is fast"},
            ]
            results = await batch_store(client._transport, items)

    Args:
        transport: An AsyncTransport instance.
        items: List of param dicts, one per tool call.
        tool_name: The bridge tool to call for each item.
        concurrency: Max concurrent requests.
    """
    sem = asyncio.Semaphore(concurrency)
    results: list[dict[str, Any]] = []

    async def _store_one(params: dict[str, Any]) -> dict[str, Any]:
        async with sem:
            return await transport.call(tool_name, params)

    tasks = [_store_one(item) for item in items]
    results = await asyncio.gather(*tasks)
    return list(results)
