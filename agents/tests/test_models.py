# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Tests for Pydantic models."""

import pytest

from broodlink_agent.models import (
    Memory,
    SemanticResult,
    MemoryStats,
    Task,
    Project,
    Agent,
    Decision,
    WorkLog,
    Message,
    KGEntity,
    KGEdge,
    Issue,
    Formula,
    Notification,
    ScheduledTask,
)


class TestMemory:
    def test_basic(self):
        m = Memory(topic="test", content="hello")
        assert m.topic == "test"
        assert m.content == "hello"
        assert m.tags is None

    def test_extra_fields_allowed(self):
        m = Memory(topic="t", content="c", some_new_field="value")
        assert m.some_new_field == "value"

    def test_optional_fields(self):
        m = Memory(topic="t", content="c", tags="a,b", agent_name="claude")
        assert m.tags == "a,b"
        assert m.agent_name == "claude"


class TestSemanticResult:
    def test_defaults(self):
        r = SemanticResult()
        assert r.topic == ""
        assert r.score == 0.0

    def test_with_score(self):
        r = SemanticResult(topic="x", content="y", score=0.95)
        assert r.score == 0.95


class TestMemoryStats:
    def test_defaults(self):
        s = MemoryStats()
        assert s.total_count == 0
        assert s.top_topics == []

    def test_from_dict(self):
        s = MemoryStats.model_validate({"total_count": 42, "most_recent": "2026-01-01"})
        assert s.total_count == 42


class TestTask:
    def test_alias_id(self):
        t = Task.model_validate({"id": "abc-123", "title": "Do thing"})
        assert t.task_id == "abc-123"
        assert t.title == "Do thing"

    def test_defaults(self):
        t = Task()
        assert t.status == "pending"
        assert t.task_id is None


class TestProject:
    def test_basic(self):
        p = Project(name="Broodlink", description="AI orchestration")
        assert p.name == "Broodlink"
        assert p.status == "planning"


class TestAgent:
    def test_basic(self):
        a = Agent(agent_id="claude", display_name="Claude", role="worker")
        assert a.agent_id == "claude"
        assert a.active is True


class TestDecision:
    def test_basic(self):
        d = Decision(decision="use Rust", reasoning="performance")
        assert d.decision == "use Rust"


class TestWorkLog:
    def test_basic(self):
        w = WorkLog(agent_name="claude", action="created", details="built feature X")
        assert w.action == "created"


class TestMessage:
    def test_alias_from(self):
        m = Message.model_validate({"from": "claude", "to": "qwen", "content": "hello"})
        assert m.sender == "claude"

    def test_bridge_response_format(self):
        """Message model must handle actual bridge response format (UUID id, body, recipient, status)."""
        m = Message.model_validate({
            "id": "81d75598-537c-452c-9800-4827f6de28a8",
            "sender": "claude",
            "recipient": "qwen",
            "body": "hello there",
            "subject": "greeting",
            "status": "read",
            "created_at": "2026-02-24 04:40:52.947642+00",
        })
        assert m.id == "81d75598-537c-452c-9800-4827f6de28a8"
        assert m.sender == "claude"
        assert m.recipient == "qwen"
        assert m.body == "hello there"
        assert m.status == "read"


class TestKGEntity:
    def test_basic(self):
        e = KGEntity(name="Python", entity_type="language")
        assert e.entity_type == "language"


class TestKGEdge:
    def test_defaults(self):
        e = KGEdge(source_entity="A", target_entity="B", relation_type="uses")
        assert e.weight == 1.0


class TestIssue:
    def test_defaults(self):
        i = Issue(title="Fix bug")
        assert i.status == "open"


class TestFormula:
    def test_defaults(self):
        f = Formula(name="deploy")
        assert f.is_system is False
        assert f.version == 1


class TestNotification:
    def test_basic(self):
        n = Notification(channel="telegram", target="123456", message="Service down")
        assert n.status == "pending"
        assert n.target == "123456"

    def test_from_bridge_response(self):
        n = Notification.model_validate({"id": 1, "channel": "slack", "target": "hook-url", "message": "alert", "status": "pending"})
        assert n.id == 1


class TestScheduledTask:
    def test_basic(self):
        s = ScheduledTask(title="daily review", next_run_at="2026-03-01T09:00:00Z")
        assert s.title == "daily review"
        assert s.run_count == 0

    def test_from_bridge_response(self):
        s = ScheduledTask.model_validate({
            "id": 5, "title": "heartbeat", "description": "", "priority": 0,
            "next_run_at": "2026-03-01T09:00:00Z", "recurrence_secs": 3600,
            "run_count": 3, "max_runs": 10,
        })
        assert s.recurrence_secs == 3600
        assert s.max_runs == 10
