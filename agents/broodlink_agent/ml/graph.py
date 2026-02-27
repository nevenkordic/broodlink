# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Convert Broodlink knowledge graph to NetworkX for analysis."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import networkx as nx

from ..models import KGEntity, KGEdge


def kg_to_networkx(
    entities: list[KGEntity],
    edges: list[KGEdge],
) -> "nx.DiGraph":
    """Build a NetworkX directed graph from KG entities and edges.

    Example::

        async with AsyncBroodlinkClient() as client:
            entities = await client.kg.search("python")
            # Collect edges for each entity
            all_edges = []
            for e in entities:
                neighbors = await client.kg.neighbors(e.name)
                all_edges.extend(neighbors)
            G = kg_to_networkx(entities, all_edges)
            print(f"Nodes: {G.number_of_nodes()}, Edges: {G.number_of_edges()}")
    """
    import networkx as nx

    G = nx.DiGraph()

    for entity in entities:
        G.add_node(
            entity.name,
            entity_type=entity.entity_type,
            description=entity.description or "",
            **(entity.properties or {}),
        )

    for edge in edges:
        G.add_edge(
            edge.source_entity,
            edge.target_entity,
            relation_type=edge.relation_type,
            weight=edge.weight,
        )

    return G
