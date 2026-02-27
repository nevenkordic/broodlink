# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""BaseAgent — lifecycle hooks, LLM tool loop, custom tool registration."""

from __future__ import annotations

import asyncio
import json
from typing import Any, Callable, Awaitable

from .config import AgentConfig
from ._transport import AsyncTransport


ToolHandler = Callable[..., Awaitable[Any]]


class BaseAgent:
    """Base class for building Broodlink agents.

    Provides lifecycle hooks, an LLM-powered tool loop, and custom tool
    registration. Subclass and override hooks as needed::

        class MyAgent(BaseAgent):
            async def on_start(self):
                print("Agent started!")

            async def on_task(self, task_data: dict) -> str:
                return await self.run_tool_loop(task_data["title"])

        agent = MyAgent(config)
        await agent.start()
    """

    def __init__(self, config: AgentConfig | None = None):
        self.config = config or AgentConfig.from_env()
        self._transport = AsyncTransport(self.config)
        self._custom_tools: dict[str, ToolHandler] = {}
        self._tool_schemas: list[dict[str, Any]] = []
        self._bridge_tools: list[dict[str, Any]] = []
        self._running = False

    # ── Custom tool registration ────────────────────────────────────

    def tool(
        self,
        name: str | None = None,
        description: str = "",
        parameters: dict[str, Any] | None = None,
    ) -> Callable[[ToolHandler], ToolHandler]:
        """Decorator to register a custom tool.

        Usage::

            @agent.tool(name="greet", description="Greet a user")
            async def greet(name: str) -> str:
                return f"Hello, {name}!"
        """
        def decorator(fn: ToolHandler) -> ToolHandler:
            tool_name = name or fn.__name__
            self._custom_tools[tool_name] = fn
            self._tool_schemas.append({
                "type": "function",
                "function": {
                    "name": tool_name,
                    "description": description or fn.__doc__ or "",
                    "parameters": parameters or {"type": "object", "properties": {}},
                },
            })
            return fn
        return decorator

    # ── Lifecycle hooks (override in subclasses) ────────────────────

    async def on_start(self) -> None:
        """Called when the agent starts. Override for setup logic."""

    async def on_stop(self) -> None:
        """Called when the agent stops. Override for cleanup logic."""

    async def on_task(self, task_data: dict[str, Any]) -> str:
        """Called when a task is received. Override to handle tasks.

        Default implementation runs the tool loop with the task title.
        """
        title = task_data.get("title", "")
        description = task_data.get("description", "")
        prompt = f"{title}\n\n{description}" if description else title
        return await self.run_tool_loop(prompt)

    async def on_error(self, error: Exception, context: str = "") -> None:
        """Called on unhandled errors. Override for custom error handling."""

    # ── Tool loop ───────────────────────────────────────────────────

    async def run_tool_loop(
        self,
        user_message: str,
        max_rounds: int | None = None,
        system_prompt: str = "You are a helpful Broodlink agent.",
    ) -> str:
        """Run an LLM tool-calling loop and return the final text response.

        Sends the user message to an OpenAI-compatible LLM with bridge tools +
        custom tools, executes tool calls, and iterates until the LLM produces
        a text response or max_rounds is reached.
        """
        import aiohttp

        rounds = max_rounds or self.config.max_tool_rounds
        all_tools = self._bridge_tools + self._tool_schemas

        messages: list[dict[str, Any]] = [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_message},
        ]

        async with aiohttp.ClientSession() as session:
            for _ in range(rounds):
                payload: dict[str, Any] = {
                    "model": self.config.lm_model,
                    "messages": messages,
                    "temperature": 0.7,
                    "max_tokens": 4096,
                    "stream": False,
                }
                if all_tools:
                    payload["tools"] = all_tools
                    payload["tool_choice"] = "auto"

                async with session.post(
                    f"{self.config.lm_url}/v1/chat/completions",
                    json=payload,
                    timeout=aiohttp.ClientTimeout(total=self.config.request_timeout),
                ) as resp:
                    if resp.status != 200:
                        text = await resp.text()
                        return f"[LLM error: HTTP {resp.status}] {text[:200]}"
                    data = await resp.json()
                    choices = data.get("choices")
                    if not choices:
                        return data.get("error", {}).get("message", "[LLM returned no choices]")
                    msg = choices[0]["message"]

                tool_calls = msg.get("tool_calls")
                if not tool_calls:
                    return msg.get("content", "") or ""

                messages.append(msg)

                for tc in tool_calls:
                    func = tc.get("function", {})
                    tc_name = func.get("name", "")
                    try:
                        tc_args = json.loads(func.get("arguments", "{}"))
                    except json.JSONDecodeError:
                        tc_args = {}

                    if tc_name in self._custom_tools:
                        result = await self._custom_tools[tc_name](**tc_args)
                        result_str = json.dumps(result, default=str) if not isinstance(result, str) else result
                    else:
                        result = await self._transport.call(tc_name, tc_args)
                        result_str = json.dumps(result, default=str)

                    messages.append({
                        "role": "tool",
                        "tool_call_id": tc.get("id", ""),
                        "content": result_str,
                    })

            # Exhausted rounds — get final text without tools
            payload = {
                "model": self.config.lm_model,
                "messages": messages,
                "temperature": 0.7,
                "max_tokens": 4096,
                "stream": False,
            }
            async with session.post(
                f"{self.config.lm_url}/v1/chat/completions",
                json=payload,
                timeout=aiohttp.ClientTimeout(total=self.config.request_timeout),
            ) as resp:
                if resp.status != 200:
                    text = await resp.text()
                    return f"[LLM error: HTTP {resp.status}] {text[:200]}"
                data = await resp.json()
                choices = data.get("choices")
                if not choices:
                    return data.get("error", {}).get("message", "[LLM returned no choices]")
                return choices[0]["message"].get("content", "") or ""

    # ── Start / stop ────────────────────────────────────────────────

    async def start(self) -> None:
        """Initialize transport, fetch tools, register agent, call on_start."""
        await self._transport.__aenter__()

        self._bridge_tools = await self._transport.fetch_tools()

        if self.config.agent_id:
            await self._transport.call("agent_upsert", {
                "agent_id": self.config.agent_id,
                "display_name": self.config.agent_id,
                "role": self.config.agent_role,
                "cost_tier": self.config.cost_tier,
                "active": True,
            })

        self._running = True
        await self.on_start()

    async def stop(self) -> None:
        """Deactivate agent, close transport, call on_stop."""
        self._running = False
        await self.on_stop()

        if self.config.agent_id:
            try:
                await self._transport.call("agent_upsert", {
                    "agent_id": self.config.agent_id,
                    "display_name": self.config.agent_id,
                    "role": self.config.agent_role,
                    "cost_tier": self.config.cost_tier,
                    "active": False,
                })
            except Exception:
                pass

        await self._transport.close()

    async def __aenter__(self) -> "BaseAgent":
        await self.start()
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.stop()
