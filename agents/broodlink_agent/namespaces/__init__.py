# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Type-safe namespace wrappers for beads-bridge tool categories."""

from .base import BaseNamespace
from .memory import MemoryNamespace
from .tasks import TasksNamespace
from .projects import ProjectsNamespace
from .agents import AgentsNamespace
from .beads import BeadsNamespace
from .knowledge_graph import KnowledgeGraphNamespace
from .messaging import MessagingNamespace
from .decisions import DecisionsNamespace
from .work import WorkNamespace
from .scheduling import SchedulingNamespace
from .notifications import NotificationsNamespace

__all__ = [
    "BaseNamespace",
    "MemoryNamespace",
    "TasksNamespace",
    "ProjectsNamespace",
    "AgentsNamespace",
    "BeadsNamespace",
    "KnowledgeGraphNamespace",
    "MessagingNamespace",
    "DecisionsNamespace",
    "WorkNamespace",
    "SchedulingNamespace",
    "NotificationsNamespace",
]
