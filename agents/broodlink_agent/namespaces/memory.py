# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Memory namespace — store, recall, search, delete memories."""

from __future__ import annotations

from typing import Any, Optional

from .base import BaseNamespace
from ..models import Memory, SemanticResult, MemoryStats


class MemoryNamespace(BaseNamespace):

    async def store(self, topic: str, content: str, tags: str | None = None) -> dict[str, Any]:
        params: dict[str, Any] = {"topic": topic, "content": content}
        if tags:
            params["tags"] = tags
        return await self._call("store_memory", params)

    async def recall(self, topic_search: str | None = None) -> list[Memory]:
        params: dict[str, Any] = {}
        if topic_search:
            params["topic_search"] = topic_search
        data = await self._call("recall_memory", params)
        if isinstance(data, dict):
            result = data.get("memories", data.get("result", []))
        elif isinstance(data, list):
            result = data
        else:
            result = []
        if isinstance(result, list):
            return [Memory.model_validate(m) for m in result]
        return []

    async def delete(self, topic: str) -> dict[str, Any]:
        return await self._call("delete_memory", {"topic": topic})

    async def search(self, query: str, limit: int = 5) -> list[SemanticResult]:
        data = await self._call("semantic_search", {"query": query, "limit": limit})
        results = data.get("results", []) if isinstance(data, dict) else []
        return [SemanticResult.model_validate(r) for r in results]

    async def stats(self) -> MemoryStats:
        data = await self._call("get_memory_stats")
        return MemoryStats.model_validate(data)
