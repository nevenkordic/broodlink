# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Regression tests for bugs found during v0.12.0 audit.

Each test documents a specific bug that was fixed. If any of these
fail, the corresponding bug has been reintroduced.
"""

import asyncio

import pytest
from unittest.mock import AsyncMock

from broodlink_agent.namespaces import (
    NotificationsNamespace,
    SchedulingNamespace,
)
from broodlink_agent.models import Notification, ScheduledTask
from broodlink_agent.client import BroodlinkClient, _SyncNamespace
from broodlink_agent.agent import BaseAgent
from broodlink_agent.config import AgentConfig


# ── Bug: NotificationsNamespace.send() used wrong param names ───────
# send_notification expects: channel, target, message
# Previously sent: channel, subject, body

@pytest.mark.asyncio
async def test_notification_send_uses_target_not_subject(mock_transport):
    """send_notification must use 'target' and 'message', not 'subject' and 'body'."""
    ns = NotificationsNamespace(mock_transport)
    await ns.send("telegram", "12345", "Service is down")
    args = mock_transport.call.call_args
    params = args[0][1]
    assert "target" in params, "Must use 'target' param (was 'subject')"
    assert "message" in params, "Must use 'message' param (was 'body')"
    assert "subject" not in params, "Must NOT use 'subject'"
    assert "body" not in params, "Must NOT use 'body'"


# ── Bug: NotificationsNamespace.create_rule() missing required params ─
# create_notification_rule expects: name, condition_type, channel, target
# Previously sent: condition, channel, template

@pytest.mark.asyncio
async def test_notification_create_rule_uses_correct_params(mock_transport):
    """create_notification_rule must include name, condition_type, target."""
    ns = NotificationsNamespace(mock_transport)
    await ns.create_rule("alert-rule", "budget_low", "telegram", "12345")
    args = mock_transport.call.call_args
    params = args[0][1]
    assert "name" in params, "Must include 'name'"
    assert "condition_type" in params, "Must use 'condition_type' (was 'condition')"
    assert "target" in params, "Must include 'target'"
    assert "condition" not in params, "Must NOT use 'condition'"


# ── Bug: SchedulingNamespace.cancel() used 'task_id' instead of 'id' ─

@pytest.mark.asyncio
async def test_scheduling_cancel_uses_id_not_task_id(mock_transport):
    """cancel_scheduled_task must use 'id' param, not 'task_id'."""
    ns = SchedulingNamespace(mock_transport)
    await ns.cancel(42)
    args = mock_transport.call.call_args
    params = args[0][1]
    assert "id" in params, "Must use 'id' (was 'task_id')"
    assert "task_id" not in params, "Must NOT use 'task_id'"
    assert params["id"] == 42


# ── Bug: SchedulingNamespace.schedule() used wrong param names ───────
# schedule_task expects: title, run_at
# Previously sent: task_title, schedule_type, cron_expression

@pytest.mark.asyncio
async def test_scheduling_schedule_uses_title_run_at(mock_transport):
    """schedule_task must use 'title' and 'run_at', not 'task_title' and 'schedule_type'."""
    ns = SchedulingNamespace(mock_transport)
    await ns.schedule("daily review", "2026-03-01T09:00:00Z")
    args = mock_transport.call.call_args
    params = args[0][1]
    assert "title" in params, "Must use 'title' (was 'task_title')"
    assert "run_at" in params, "Must use 'run_at' (was missing)"
    assert "task_title" not in params, "Must NOT use 'task_title'"
    assert "schedule_type" not in params, "Must NOT use 'schedule_type'"


# ── Bug: SchedulingNamespace.list() used wrong response key ──────────
# list_scheduled_tasks returns {"scheduled_tasks": [...]}
# Previously looked for "tasks"

@pytest.mark.asyncio
async def test_scheduling_list_uses_scheduled_tasks_key(mock_transport):
    """list() must parse 'scheduled_tasks' key, not 'tasks'."""
    mock_transport.call.return_value = {"scheduled_tasks": [{"id": 1, "title": "test"}]}
    ns = SchedulingNamespace(mock_transport)
    result = await ns.list()
    assert len(result) == 1
    assert result[0]["title"] == "test"


# ── Bug: Notification model used 'subject' and 'body' fields ────────

def test_notification_model_uses_target_message():
    """Notification model must have 'target' and 'message', not 'subject' and 'body'."""
    n = Notification(channel="telegram", target="12345", message="Alert")
    assert n.target == "12345"
    assert n.message == "Alert"
    assert not hasattr(n, "subject") or n.__class__.model_fields.get("subject") is None
    assert not hasattr(n, "body") or n.__class__.model_fields.get("body") is None


# ── Bug: ScheduledTask model used 'task_title' and 'schedule_type' ──

def test_scheduled_task_model_uses_title():
    """ScheduledTask model must have 'title', not 'task_title'."""
    s = ScheduledTask(title="daily review", next_run_at="2026-03-01T09:00:00Z")
    assert s.title == "daily review"
    assert "task_title" not in s.__class__.model_fields


# ── Bug: _SyncNamespace had broken loop discovery ───────────────────

def test_sync_namespace_receives_loop_directly():
    """_SyncNamespace must receive the event loop in __init__, not discover it."""
    loop = asyncio.new_event_loop()
    try:
        ns = _SyncNamespace(object(), loop)
        assert ns._loop is loop
    finally:
        loop.close()


# ── Bug: BaseAgent.run_tool_loop() created duplicate aiohttp sessions ─

def test_agent_run_tool_loop_single_session():
    """run_tool_loop should use a single aiohttp.ClientSession, not two."""
    import inspect
    source = inspect.getsource(BaseAgent.run_tool_loop)
    # Count occurrences of "aiohttp.ClientSession()" — should appear once
    count = source.count("aiohttp.ClientSession()")
    assert count == 1, f"Expected 1 aiohttp.ClientSession(), found {count} (was 2 before fix)"


# ── Bug: BaseAgent.run_tool_loop() didn't check LLM response status ─

def test_agent_run_tool_loop_checks_response_status():
    """run_tool_loop should check resp.status for errors."""
    import inspect
    source = inspect.getsource(BaseAgent.run_tool_loop)
    assert "resp.status" in source, "Must check resp.status for LLM errors"
    assert "choices" in source, "Must safely extract 'choices' from response"


# ── Bug: MemoryNamespace.recall() parsed wrong response key ──────
# recall_memory returns {"memories": [...]}
# Previously looked for "result"

@pytest.mark.asyncio
async def test_memory_recall_parses_memories_key(mock_transport):
    """recall() must parse 'memories' key, not 'result'."""
    from broodlink_agent.namespaces import MemoryNamespace
    mock_transport.call.return_value = {"memories": [{"topic": "test", "content": "hello"}]}
    ns = MemoryNamespace(mock_transport)
    result = await ns.recall(topic_search="test")
    assert len(result) == 1
    assert result[0].topic == "test"


# ── Bug: NotificationsNamespace.list_rules() parsed wrong key ────
# list_notification_rules returns {"notification_rules": [...]}
# Previously looked for "rules"

@pytest.mark.asyncio
async def test_notifications_list_rules_parses_notification_rules_key(mock_transport):
    """list_rules() must parse 'notification_rules' key, not 'rules'."""
    from broodlink_agent.namespaces import NotificationsNamespace
    mock_transport.call.return_value = {"notification_rules": [{"id": 1, "name": "test-rule"}]}
    ns = NotificationsNamespace(mock_transport)
    result = await ns.list_rules()
    assert len(result) == 1
    assert result[0]["name"] == "test-rule"


# ── Bug: Message.id was Optional[int] but bridge returns UUID strings ─

def test_message_model_accepts_uuid_id():
    """Message.id must accept UUID strings, not just int."""
    from broodlink_agent.models import Message
    m = Message.model_validate({
        "id": "81d75598-537c-452c-9800-4827f6de28a8",
        "sender": "claude",
        "recipient": "qwen",
        "body": "hello",
        "status": "read",
    })
    assert m.id == "81d75598-537c-452c-9800-4827f6de28a8"
    assert m.sender == "claude"
    assert m.body == "hello"
    assert m.status == "read"
