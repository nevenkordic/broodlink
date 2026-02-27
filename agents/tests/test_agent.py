# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for BaseAgent — tool registration, lifecycle hooks."""

import pytest
from unittest.mock import AsyncMock, patch

from broodlink_agent.agent import BaseAgent
from broodlink_agent.config import AgentConfig


class TestBaseAgent:
    def test_construction(self, config):
        agent = BaseAgent(config)
        assert agent.config is config
        assert agent._custom_tools == {}
        assert agent._running is False

    def test_tool_decorator(self, config):
        agent = BaseAgent(config)

        @agent.tool(name="greet", description="Greet someone")
        async def greet(name: str) -> str:
            return f"Hello, {name}!"

        assert "greet" in agent._custom_tools
        assert len(agent._tool_schemas) == 1
        assert agent._tool_schemas[0]["function"]["name"] == "greet"

    def test_tool_decorator_uses_function_name(self, config):
        agent = BaseAgent(config)

        @agent.tool(description="Test tool")
        async def my_tool() -> str:
            return "ok"

        assert "my_tool" in agent._custom_tools

    def test_multiple_tools(self, config):
        agent = BaseAgent(config)

        @agent.tool(name="tool1")
        async def t1() -> str:
            return "1"

        @agent.tool(name="tool2")
        async def t2() -> str:
            return "2"

        assert len(agent._custom_tools) == 2
        assert len(agent._tool_schemas) == 2


class TestBaseAgentLifecycle:
    @pytest.mark.asyncio
    async def test_start_stop(self, config):
        agent = BaseAgent(config)
        agent._transport = AsyncMock()
        agent._transport.__aenter__ = AsyncMock(return_value=agent._transport)
        agent._transport.fetch_tools = AsyncMock(return_value=[])
        agent._transport.call = AsyncMock(return_value={"ok": True})
        agent._transport.close = AsyncMock()

        await agent.start()
        assert agent._running is True
        agent._transport.call.assert_called_once()

        await agent.stop()
        assert agent._running is False

    @pytest.mark.asyncio
    async def test_context_manager(self, config):
        agent = BaseAgent(config)
        agent._transport = AsyncMock()
        agent._transport.__aenter__ = AsyncMock(return_value=agent._transport)
        agent._transport.fetch_tools = AsyncMock(return_value=[])
        agent._transport.call = AsyncMock(return_value={"ok": True})
        agent._transport.close = AsyncMock()

        async with agent:
            assert agent._running is True
        assert agent._running is False

    @pytest.mark.asyncio
    async def test_on_start_hook_called(self, config):
        called = False

        class MyAgent(BaseAgent):
            async def on_start(self):
                nonlocal called
                called = True

        agent = MyAgent(config)
        agent._transport = AsyncMock()
        agent._transport.__aenter__ = AsyncMock(return_value=agent._transport)
        agent._transport.fetch_tools = AsyncMock(return_value=[])
        agent._transport.call = AsyncMock(return_value={"ok": True})
        agent._transport.close = AsyncMock()

        await agent.start()
        assert called is True
        await agent.stop()
