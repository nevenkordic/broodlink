# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Shared test fixtures for Broodlink Python SDK tests."""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest

from broodlink_agent.config import AgentConfig
from broodlink_agent._transport import AsyncTransport


@pytest.fixture
def config() -> AgentConfig:
    """Test config with dummy values."""
    return AgentConfig(
        agent_id="test-agent",
        agent_jwt="test-jwt-token",
        bridge_url="http://localhost:3310/api/v1/tool",
        lm_url="http://localhost:1234",
        lm_model="test-model",
        request_timeout=10,
        nats_url="nats://localhost:4222",
        nats_prefix="broodlink",
        env="test",
    )


@pytest.fixture
def mock_transport() -> AsyncTransport:
    """AsyncTransport with call/fetch_tools mocked."""
    t = AsyncTransport.__new__(AsyncTransport)
    t.config = AgentConfig(agent_id="test-agent")
    t._session = None
    t._cb = None
    t.call = AsyncMock(return_value={"ok": True})
    t.fetch_tools = AsyncMock(return_value=[])
    t.close = AsyncMock()
    return t
