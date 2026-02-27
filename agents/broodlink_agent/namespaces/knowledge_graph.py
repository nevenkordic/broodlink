# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Knowledge graph namespace — entities, edges, traversal."""

from __future__ import annotations

from typing import Any

from .base import BaseNamespace
from ..models import KGEntity, KGEdge


class KnowledgeGraphNamespace(BaseNamespace):

    async def add_entity(self, name: str, entity_type: str, description: str = "", **kwargs: Any) -> dict[str, Any]:
        params: dict[str, Any] = {"name": name, "entity_type": entity_type, "description": description}
        params.update(kwargs)
        return await self._call("kg_add_entity", params)

    async def add_edge(self, source: str, target: str, relation: str, weight: float = 1.0) -> dict[str, Any]:
        return await self._call("kg_add_edge", {
            "source_entity": source, "target_entity": target,
            "relation_type": relation, "weight": weight,
        })

    async def get_entity(self, name: str) -> dict[str, Any]:
        return await self._call("kg_get_entity", {"name": name})

    async def neighbors(self, entity: str, depth: int = 1) -> dict[str, Any]:
        return await self._call("kg_neighbors", {"entity": entity, "depth": depth})

    async def search(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        data = await self._call("kg_search", {"query": query, "limit": limit})
        return data.get("results", []) if isinstance(data, dict) else []
