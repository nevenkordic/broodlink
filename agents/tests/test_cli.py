# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for Click CLI commands."""

from click.testing import CliRunner

from broodlink_agent.cli import cli


def test_cli_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["--help"])
    assert result.exit_code == 0
    assert "Broodlink Python SDK" in result.output


def test_agent_list_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["agent", "list", "--help"])
    assert result.exit_code == 0
    assert "List registered agents" in result.output


def test_memory_search_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["memory", "search", "--help"])
    assert result.exit_code == 0
    assert "Semantic search" in result.output


def test_memory_store_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["memory", "store", "--help"])
    assert result.exit_code == 0
    assert "Store a memory entry" in result.output


def test_memory_stats_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["memory", "stats", "--help"])
    assert result.exit_code == 0
    assert "Show memory statistics" in result.output


def test_tool_call_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["tool", "call", "--help"])
    assert result.exit_code == 0
    assert "Call a bridge tool by name" in result.output


def test_tool_list_help():
    runner = CliRunner()
    result = runner.invoke(cli, ["tool", "list", "--help"])
    assert result.exit_code == 0
    assert "List available bridge tools" in result.output
