# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for backward compatibility — old imports still work."""

import importlib


def test_bridge_module_importable():
    """bridge.py (BridgeClient) should still be importable."""
    mod = importlib.import_module("broodlink_agent.bridge")
    assert hasattr(mod, "BridgeClient")
    assert hasattr(mod, "CircuitBreaker")


def test_tools_module_importable():
    """tools.py should still be importable."""
    mod = importlib.import_module("broodlink_agent.tools")
    assert hasattr(mod, "execute_tool_call")
    assert hasattr(mod, "chat_turn")


def test_config_from_env():
    """AgentConfig.from_env() should still work."""
    from broodlink_agent.config import AgentConfig
    config = AgentConfig.from_env()
    assert isinstance(config.bridge_url, str)


def test_llm_client_importable():
    """LLMClient should still be importable."""
    mod = importlib.import_module("broodlink_agent.llm")
    assert hasattr(mod, "LLMClient")


def test_top_level_imports():
    """Top-level package imports should include new SDK classes."""
    import broodlink_agent
    assert hasattr(broodlink_agent, "AsyncBroodlinkClient")
    assert hasattr(broodlink_agent, "BroodlinkClient")
    assert hasattr(broodlink_agent, "BaseAgent")
    assert hasattr(broodlink_agent, "NATSHelper")
    assert hasattr(broodlink_agent, "AgentConfig")
    assert hasattr(broodlink_agent, "Memory")
    assert hasattr(broodlink_agent, "Task")


def test_version_bumped():
    """Version should be 0.12.0."""
    import broodlink_agent
    assert broodlink_agent.__version__ == "0.12.0"


def test_main_entry_point():
    """cli.main should exist for python -m broodlink_agent."""
    from broodlink_agent.cli import main
    assert callable(main)
