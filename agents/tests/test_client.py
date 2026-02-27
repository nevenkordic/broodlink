# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for AsyncBroodlinkClient and BroodlinkClient."""

import pytest

from broodlink_agent.config import AgentConfig
from broodlink_agent.client import AsyncBroodlinkClient, BroodlinkClient
from broodlink_agent.namespaces import (
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


class TestAsyncBroodlinkClient:
    def test_construction(self, config):
        client = AsyncBroodlinkClient(config)
        assert client.config is config
        assert isinstance(client.memory, MemoryNamespace)
        assert isinstance(client.tasks, TasksNamespace)
        assert isinstance(client.projects, ProjectsNamespace)
        assert isinstance(client.agents, AgentsNamespace)
        assert isinstance(client.beads, BeadsNamespace)
        assert isinstance(client.kg, KnowledgeGraphNamespace)
        assert isinstance(client.messaging, MessagingNamespace)
        assert isinstance(client.decisions, DecisionsNamespace)
        assert isinstance(client.work, WorkNamespace)
        assert isinstance(client.scheduling, SchedulingNamespace)
        assert isinstance(client.notifications, NotificationsNamespace)

    def test_default_config(self):
        client = AsyncBroodlinkClient()
        assert client.config.bridge_url == "http://localhost:3310/api/v1/tool"


class TestBroodlinkClientSync:
    def test_construction(self, config):
        client = BroodlinkClient(config)
        try:
            # Sync wrapper should create all namespace proxies
            assert hasattr(client, "memory")
            assert hasattr(client, "tasks")
            assert hasattr(client, "projects")
            assert hasattr(client, "agents")
            assert hasattr(client, "beads")
            assert hasattr(client, "kg")
            assert hasattr(client, "messaging")
            assert hasattr(client, "decisions")
            assert hasattr(client, "work")
            assert hasattr(client, "scheduling")
            assert hasattr(client, "notifications")
        finally:
            client.close()

    def test_context_manager(self, config):
        with BroodlinkClient(config) as client:
            assert hasattr(client, "memory")

    def test_default_config(self):
        client = BroodlinkClient()
        try:
            assert client.config.bridge_url == "http://localhost:3310/api/v1/tool"
        finally:
            client.close()
