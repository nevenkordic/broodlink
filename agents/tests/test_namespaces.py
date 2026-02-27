# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for namespace wrappers — correct tool routing and param passing."""

import pytest

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


@pytest.mark.asyncio
async def test_memory_store(mock_transport):
    ns = MemoryNamespace(mock_transport)
    await ns.store("topic1", "content1", tags="a,b")
    mock_transport.call.assert_called_once_with(
        "store_memory", {"topic": "topic1", "content": "content1", "tags": "a,b"}
    )


@pytest.mark.asyncio
async def test_memory_recall(mock_transport):
    mock_transport.call.return_value = {"memories": [{"topic": "t", "content": "c"}]}
    ns = MemoryNamespace(mock_transport)
    result = await ns.recall(topic_search="test")
    mock_transport.call.assert_called_once_with("recall_memory", {"topic_search": "test"})


@pytest.mark.asyncio
async def test_memory_search(mock_transport):
    mock_transport.call.return_value = {"results": [{"topic": "t", "content": "c", "score": 0.9}]}
    ns = MemoryNamespace(mock_transport)
    results = await ns.search("query", limit=3)
    mock_transport.call.assert_called_once_with("semantic_search", {"query": "query", "limit": 3})


@pytest.mark.asyncio
async def test_memory_stats(mock_transport):
    mock_transport.call.return_value = {"total_count": 5, "most_recent": "2026-01-01", "top_topics": []}
    ns = MemoryNamespace(mock_transport)
    stats = await ns.stats()
    mock_transport.call.assert_called_once_with("get_memory_stats", None)


@pytest.mark.asyncio
async def test_tasks_create(mock_transport):
    mock_transport.call.return_value = {"task_id": "1"}
    ns = TasksNamespace(mock_transport)
    await ns.create("Build feature", description="Details here")
    mock_transport.call.assert_called_once_with(
        "create_task", {"title": "Build feature", "description": "Details here"}
    )


@pytest.mark.asyncio
async def test_tasks_list(mock_transport):
    mock_transport.call.return_value = {"tasks": []}
    ns = TasksNamespace(mock_transport)
    result = await ns.list()
    mock_transport.call.assert_called_once_with("list_tasks", {"limit": 50})


@pytest.mark.asyncio
async def test_tasks_claim(mock_transport):
    ns = TasksNamespace(mock_transport)
    await ns.claim("42")
    mock_transport.call.assert_called_once_with("claim_task", {"task_id": "42"})


@pytest.mark.asyncio
async def test_projects_add(mock_transport):
    ns = ProjectsNamespace(mock_transport)
    await ns.add("MyProject", "A great project")
    mock_transport.call.assert_called_once_with(
        "add_project", {"name": "MyProject", "description": "A great project", "status": "planning"}
    )


@pytest.mark.asyncio
async def test_agents_upsert(mock_transport):
    ns = AgentsNamespace(mock_transport)
    await ns.upsert("test-agent", "Test Agent", "worker")
    mock_transport.call.assert_called_once_with(
        "agent_upsert",
        {"agent_id": "test-agent", "display_name": "Test Agent", "role": "worker"},
    )


@pytest.mark.asyncio
async def test_beads_create_issue(mock_transport):
    ns = BeadsNamespace(mock_transport)
    await ns.create_issue("Bug title", description="Details")
    mock_transport.call.assert_called_once_with(
        "beads_create_issue", {"title": "Bug title", "description": "Details"}
    )


@pytest.mark.asyncio
async def test_kg_add_entity(mock_transport):
    ns = KnowledgeGraphNamespace(mock_transport)
    await ns.add_entity("Python", "language", description="A programming language")
    mock_transport.call.assert_called_once_with(
        "kg_add_entity",
        {"name": "Python", "entity_type": "language", "description": "A programming language"},
    )


@pytest.mark.asyncio
async def test_messaging_send(mock_transport):
    ns = MessagingNamespace(mock_transport)
    await ns.send("qwen", "hello there", subject="greeting")
    mock_transport.call.assert_called_once_with(
        "send_message", {"to": "qwen", "content": "hello there", "subject": "greeting"}
    )


@pytest.mark.asyncio
async def test_decisions_log(mock_transport):
    ns = DecisionsNamespace(mock_transport)
    await ns.log("use Rust", reasoning="performance")
    mock_transport.call.assert_called_once_with(
        "log_decision", {"decision": "use Rust", "reasoning": "performance"}
    )


@pytest.mark.asyncio
async def test_work_log(mock_transport):
    ns = WorkNamespace(mock_transport)
    await ns.log("claude", "created", "built feature")
    mock_transport.call.assert_called_once_with(
        "log_work", {"agent_name": "claude", "action": "created", "details": "built feature"}
    )


@pytest.mark.asyncio
async def test_scheduling_schedule(mock_transport):
    ns = SchedulingNamespace(mock_transport)
    await ns.schedule("daily review", "2026-03-01T09:00:00Z")
    mock_transport.call.assert_called_once_with(
        "schedule_task", {"title": "daily review", "run_at": "2026-03-01T09:00:00Z"}
    )


@pytest.mark.asyncio
async def test_scheduling_schedule_with_recurrence(mock_transport):
    ns = SchedulingNamespace(mock_transport)
    await ns.schedule("heartbeat check", "2026-03-01T09:00:00Z", recurrence_secs=3600)
    mock_transport.call.assert_called_once_with(
        "schedule_task", {"title": "heartbeat check", "run_at": "2026-03-01T09:00:00Z", "recurrence_secs": 3600}
    )


@pytest.mark.asyncio
async def test_scheduling_cancel(mock_transport):
    ns = SchedulingNamespace(mock_transport)
    await ns.cancel(42)
    mock_transport.call.assert_called_once_with(
        "cancel_scheduled_task", {"id": 42}
    )


@pytest.mark.asyncio
async def test_scheduling_list(mock_transport):
    mock_transport.call.return_value = {"scheduled_tasks": []}
    ns = SchedulingNamespace(mock_transport)
    result = await ns.list()
    mock_transport.call.assert_called_once_with("list_scheduled_tasks", {"limit": 50})
    assert result == []


@pytest.mark.asyncio
async def test_notifications_send(mock_transport):
    ns = NotificationsNamespace(mock_transport)
    await ns.send("telegram", "123456", "Service is down")
    mock_transport.call.assert_called_once_with(
        "send_notification", {"channel": "telegram", "target": "123456", "message": "Service is down"}
    )


@pytest.mark.asyncio
async def test_notifications_create_rule(mock_transport):
    ns = NotificationsNamespace(mock_transport)
    await ns.create_rule("my-rule", "budget_low", "telegram", "123456", template="Budget alert: {budget}")
    mock_transport.call.assert_called_once_with(
        "create_notification_rule", {
            "name": "my-rule",
            "condition_type": "budget_low",
            "channel": "telegram",
            "target": "123456",
            "template": "Budget alert: {budget}",
        }
    )
