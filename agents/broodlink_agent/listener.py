# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""NATS listener mode — receives tasks from coordinator and processes via LLM."""

from __future__ import annotations

import asyncio
import json
import signal

import aiohttp
import nats as nats_client

from .bridge import BridgeClient
from .config import AgentConfig
from .llm import LLMClient
from .memory import summarize_history
from .prompt import load_memories, load_system_prompt
from .tools import chat_turn


async def _start_stream(bridge: BridgeClient, task_id: str) -> str | None:
    """Start a real-time event stream for a task. Returns stream_id or None."""
    try:
        result = await bridge.call("start_stream", {
            "tool_name": "task_execution",
            "task_id": task_id,
        })
        stream_id = result.get("stream_id")
        if stream_id:
            return stream_id
    except Exception as e:
        print(f"  WARNING: start_stream failed: {e}")
    return None


async def _emit_event(
    bridge: BridgeClient, stream_id: str, event_type: str, data: dict
) -> None:
    """Emit an event to an active stream (best-effort)."""
    try:
        await bridge.call("emit_stream_event", {
            "stream_id": stream_id,
            "event_type": event_type,
            "data": json.dumps(data),
        })
    except Exception as e:
        print(f"  WARNING: emit_stream_event failed: {e}")


async def handle_task(
    config: AgentConfig,
    bridge: BridgeClient,
    llm: LLMClient,
    system_prompt: str,
    tools: list[dict],
    task_data: dict,
) -> None:
    """Process a single dispatched task via LLM and report result."""
    task_id = task_data.get("task_id", "unknown")
    title = task_data.get("title", "Untitled task")
    description = task_data.get("description", "")
    priority = task_data.get("priority", 0)
    assigned_by = task_data.get("assigned_by", "coordinator")

    print(f"\n--- Task received: {title} (id={task_id}, priority={priority}) ---")
    if description:
        print(f"    {description}")

    # Start a real-time stream for this task (if streaming enabled)
    stream_id = None
    if config.enable_streaming:
        stream_id = await _start_stream(bridge, task_id)
        if stream_id:
            await _emit_event(bridge, stream_id, "progress", {
                "step": "started", "title": title,
            })

    user_msg = f"Task assigned by {assigned_by} (priority {priority}):\n\n{title}"
    if description:
        user_msg += f"\n\n{description}"
    user_msg += (
        "\n\nComplete this task using the available tools. "
        "Log your work with log_work when done."
    )

    history = [{"role": "user", "content": user_msg}]

    # Trim history to fit context window
    history = summarize_history(history, config.max_context_tokens)

    try:
        response = await chat_turn(config, bridge, llm, system_prompt, history, tools)
        print(f"  Agent response: {response[:200]}...")

        if stream_id:
            await _emit_event(bridge, stream_id, "progress", {
                "step": "completing", "response_preview": response[:200],
            })

        result = await bridge.call("complete_task", {"task_id": task_id})
        if "error" in result:
            print(f"  WARNING: complete_task failed: {result['error']}")
        else:
            print(f"  Task {task_id} completed.")

        if stream_id:
            await _emit_event(bridge, stream_id, "complete", {
                "task_id": task_id, "status": "completed",
            })
    except Exception as e:
        print(f"  ERROR processing task {task_id}: {e}")
        if stream_id:
            await _emit_event(bridge, stream_id, "error", {
                "task_id": task_id, "error": str(e),
            })
        try:
            await bridge.call("fail_task", {"task_id": task_id})
            print(f"  Task {task_id} marked as failed.")
        except Exception as fail_err:
            print(f"  WARNING: fail_task also failed: {fail_err}")


async def listen_mode(config: AgentConfig) -> None:
    """Subscribe to NATS for task dispatches and process them via LLM."""
    shutdown_event = asyncio.Event()

    def _signal_handler() -> None:
        print("\nShutdown signal received...")
        shutdown_event.set()

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, _signal_handler)

    async with aiohttp.ClientSession() as session:
        bridge = BridgeClient(session, config)
        llm = LLMClient(session, config)

        print(f"Fetching tools from {config.tools_url}...")
        tools = await bridge.fetch_tools()
        print(f"  Loaded {len(tools)} tools.")

        print("Loading memories...")
        memories_text = await load_memories(bridge)
        system_prompt = load_system_prompt(config, memories_text)

        print(f"Registering agent '{config.agent_id}'...")
        reg = await bridge.call("agent_upsert", {
            "agent_id": config.agent_id,
            "display_name": config.agent_id,
            "role": config.agent_role,
            "cost_tier": config.cost_tier,
            "active": True,
        })
        if "error" in reg:
            print(f"  WARNING: Registration failed: {reg['error']}")
        else:
            print("  Registered.")

        task_subject = f"{config.nats_prefix}.{config.env}.agent.{config.agent_id}.task"
        deleg_subject = f"{config.nats_prefix}.{config.env}.agent.{config.agent_id}.delegation"
        print(f"Connecting to NATS at {config.nats_url}...")
        nc = await nats_client.connect(config.nats_url)
        print(f"  Connected. Subscribing to: {task_subject}")
        print(f"  Also subscribing to: {deleg_subject}")

        task_sub = await nc.subscribe(task_subject)
        deleg_sub = await nc.subscribe(deleg_subject)
        print(f"\nBroodlink Agent [{config.agent_id}] listening for tasks. Ctrl+C to stop.\n")

        try:
            while not shutdown_event.is_set():
                # Poll both subscriptions
                for sub in (task_sub, deleg_sub):
                    try:
                        msg = await asyncio.wait_for(sub.next_msg(), timeout=2.5)
                    except asyncio.TimeoutError:
                        continue

                    try:
                        task_data = json.loads(msg.data.decode())
                    except (json.JSONDecodeError, UnicodeDecodeError) as e:
                        print(f"WARNING: Bad NATS payload: {e}")
                        continue

                    # Handle delegation messages: auto-accept and process
                    if sub is deleg_sub:
                        delegation_id = task_data.get("delegation_id")
                        if delegation_id:
                            print(f"  Accepting delegation: {delegation_id}")
                            await bridge.call("accept_delegation", {
                                "delegation_id": delegation_id,
                            })

                    await handle_task(config, bridge, llm, system_prompt, tools, task_data)
        finally:
            print("Unsubscribing from NATS...")
            await task_sub.unsubscribe()
            await deleg_sub.unsubscribe()
            await nc.drain()
            print("NATS connection closed.")

            await bridge.call("agent_upsert", {
                "agent_id": config.agent_id,
                "display_name": config.agent_id,
                "role": config.agent_role,
                "cost_tier": config.cost_tier,
                "active": False,
            })
            print(f"Agent '{config.agent_id}' deactivated. Goodbye.")
