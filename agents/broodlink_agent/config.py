# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Agent configuration loaded from environment variables."""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class AgentConfig:
    """All configuration for a Broodlink agent."""

    agent_id: str = ""
    agent_jwt: str = ""
    bridge_url: str = "http://localhost:3310/api/v1/tool"
    lm_url: str = "http://localhost:1234"
    lm_model: str = "default"
    lm_fallback_url: str = ""
    lm_fallback_model: str = ""
    max_tool_rounds: int = 5
    max_history: int = 20
    max_context_tokens: int = 8000
    request_timeout: int = 120
    nats_url: str = "nats://localhost:4222"
    nats_prefix: str = "broodlink"
    env: str = "local"
    think_mode: str = ""
    agent_role: str = "worker"
    cost_tier: str = "medium"
    enable_streaming: bool = True
    template_path: Path = field(default_factory=lambda: Path(__file__).resolve().parent.parent.parent / "templates" / "agent-system-prompt.md")

    @property
    def tools_url(self) -> str:
        """Derive tools metadata URL from bridge URL."""
        return self.bridge_url.rsplit("/tool", 1)[0] + "/tools"

    @classmethod
    def from_env(cls) -> AgentConfig:
        """Load configuration from environment variables."""
        return cls(
            agent_id=os.environ.get("BROODLINK_AGENT_ID", ""),
            agent_jwt=os.environ.get("BROODLINK_AGENT_JWT", ""),
            bridge_url=os.environ.get("BROODLINK_BRIDGE_URL", "http://localhost:3310/api/v1/tool"),
            lm_url=os.environ.get("LM_STUDIO_URL", "http://localhost:1234"),
            lm_model=os.environ.get("LM_MODEL", "default"),
            lm_fallback_url=os.environ.get("LM_FALLBACK_URL", ""),
            lm_fallback_model=os.environ.get("LM_FALLBACK_MODEL", ""),
            max_tool_rounds=int(os.environ.get("MAX_TOOL_ROUNDS", "5")),
            max_history=int(os.environ.get("MAX_HISTORY", "20")),
            max_context_tokens=int(os.environ.get("MAX_CONTEXT_TOKENS", "8000")),
            request_timeout=int(os.environ.get("REQUEST_TIMEOUT", "120")),
            nats_url=os.environ.get("BROODLINK_NATS_URL", "nats://localhost:4222"),
            nats_prefix=os.environ.get("BROODLINK_NATS_PREFIX", "broodlink"),
            env=os.environ.get("BROODLINK_ENV", "local"),
            think_mode=os.environ.get("BROODLINK_THINK_MODE", ""),
            agent_role=os.environ.get("BROODLINK_AGENT_ROLE", "worker"),
            cost_tier=os.environ.get("BROODLINK_COST_TIER", "medium"),
            enable_streaming=os.environ.get("BROODLINK_ENABLE_STREAMING", "true").lower() == "true",
        )
