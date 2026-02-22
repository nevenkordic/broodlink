#!/usr/bin/env python3
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
Researcher agent: investigates a topic using Broodlink memory, knowledge graph,
and LLM reasoning, then stores findings and hands off to the writer agent.

Uses beads-bridge HTTP API directly — no SDK import required.
"""

import argparse
import asyncio
import json
import re
import sys

import aiohttp


def slugify(text: str, max_len: int = 50) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return slug[:max_len]


def step(label: str, ok: bool, detail: str = "") -> None:
    mark = "OK" if ok else "!!"
    msg = f"  [{mark}]  {label}"
    if detail:
        msg += f"  ({detail})"
    print(msg, file=sys.stderr)


async def bridge_call(
    session: aiohttp.ClientSession,
    bridge_url: str,
    jwt: str,
    tool: str,
    params: dict,
) -> dict:
    """Call a beads-bridge tool. Returns the parsed JSON response."""
    url = f"{bridge_url}/api/v1/tool/{tool}"
    headers = {
        "Authorization": f"Bearer {jwt}",
        "Content-Type": "application/json",
    }
    try:
        async with session.post(
            url,
            json={"params": params},
            headers=headers,
            timeout=aiohttp.ClientTimeout(total=30),
        ) as resp:
            body = await resp.json()
            return body
    except Exception as e:
        return {"success": False, "error": str(e)}


async def ollama_chat(
    session: aiohttp.ClientSession,
    ollama_url: str,
    model: str,
    messages: list,
) -> str:
    """Send a chat completion to Ollama and return the assistant message content."""
    url = f"{ollama_url}/v1/chat/completions"
    payload = {
        "model": model,
        "messages": messages,
        "temperature": 0.7,
        "max_tokens": 4096,
    }
    try:
        async with session.post(
            url,
            json=payload,
            timeout=aiohttp.ClientTimeout(total=300),
        ) as resp:
            body = await resp.json()
            msg = body["choices"][0]["message"]
            content = msg.get("content", "")
            # qwen3 thinking models may put output in reasoning when content is empty
            if not content and msg.get("reasoning"):
                content = msg["reasoning"]
            if not content:
                return "[LLM error: empty response]"
            return content
    except Exception as e:
        return f"[LLM error: {e}]"


async def run(args: argparse.Namespace) -> None:
    jwt = open(args.jwt_file).read().strip()
    topic = args.topic
    slug = slugify(topic)

    async with aiohttp.ClientSession() as session:
        call = lambda tool, params: bridge_call(session, args.bridge_url, jwt, tool, params)

        # 1. Register agent (uses JWT identity — qwen3)
        resp = await call("agent_upsert", {
            "agent_id": "qwen3",
            "display_name": "Researcher (qwen3)",
            "role": "researcher",
            "capabilities": "research, memory search, knowledge graph queries",
            "cost_tier": "low",
        })
        step("Register agent", resp.get("success", False))

        # 2. Search existing memories for context
        resp = await call("semantic_search", {"query": topic, "limit": 5})
        existing_memories = []
        if resp.get("success"):
            data = resp.get("data", {})
            results = data.get("results", [])
            existing_memories = [r.get("content", "") for r in results if r.get("content")]
            step("Search memories", True, f"{len(existing_memories)} results")
        else:
            step("Search memories", True, "none found (clean slate)")

        # 3. Query knowledge graph
        resp = await call("graph_search", {"query": topic, "limit": 5})
        kg_entities = []
        if resp.get("success"):
            data = resp.get("data", {})
            entities = data.get("entities", [])
            kg_entities = [
                f"{e.get('name', '?')} ({e.get('entity_type', '?')})"
                for e in entities
            ]
            step("Query knowledge graph", True, f"{len(kg_entities)} entities")
        else:
            step("Query knowledge graph", True, "none found")

        # 4. Generate research via LLM
        context_parts = []
        if existing_memories:
            context_parts.append(
                "Existing knowledge from memory:\n"
                + "\n".join(f"- {m}" for m in existing_memories[:5])
            )
        if kg_entities:
            context_parts.append(
                "Related entities from knowledge graph:\n"
                + "\n".join(f"- {e}" for e in kg_entities[:10])
            )

        context_block = "\n\n".join(context_parts) if context_parts else "No prior context found — research from scratch."

        system_prompt = (
            "You are a research analyst. Your job is to produce structured research "
            "findings on a given topic. Output exactly this format:\n\n"
            "FINDINGS:\n"
            "1. [Finding title]: [2-3 sentence explanation]\n"
            "2. ...\n"
            "(produce 3-5 findings)\n\n"
            "TAKEAWAYS:\n"
            "- [Key insight 1]\n"
            "- [Key insight 2]\n"
            "- [Key insight 3]\n\n"
            "Be specific and substantive. No filler."
        )

        user_prompt = (
            f"Research topic: {topic}\n\n"
            f"Available context:\n{context_block}\n\n"
            "Produce your findings and takeaways now."
        )

        step("Generating research", True, f"model={args.model}")
        research_output = await ollama_chat(
            session, args.ollama_url, args.model,
            [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
        )

        if research_output.startswith("[LLM error"):
            step("LLM generation", False, research_output)
            sys.exit(1)

        step("LLM generation", True, f"{len(research_output)} chars")

        # Split into findings and takeaways
        findings = research_output
        takeaways = ""
        if "TAKEAWAYS:" in research_output:
            parts = research_output.split("TAKEAWAYS:", 1)
            findings = parts[0].strip()
            takeaways = parts[1].strip()

        # 5. Store findings
        resp = await call("store_memory", {
            "topic": f"research:{slug}:findings",
            "content": findings,
            "tags": "research,example",
        })
        step("Store findings", resp.get("success", False))

        # 6. Store takeaways
        if takeaways:
            resp = await call("store_memory", {
                "topic": f"research:{slug}:takeaways",
                "content": takeaways,
                "tags": "research,example,takeaways",
            })
            step("Store takeaways", resp.get("success", False))
        else:
            step("Store takeaways", True, "inline with findings")

        # 7. Send handoff message to writer
        handoff = json.dumps({
            "topic": topic,
            "slug": slug,
            "findings_key": f"research:{slug}:findings",
            "takeaways_key": f"research:{slug}:takeaways",
        })
        resp = await call("send_message", {
            "to": "claude",
            "content": handoff,
            "subject": "research-handoff",
        })
        step("Send handoff to writer", resp.get("success", False))

        # 8. Log work
        resp = await call("log_work", {
            "agent_name": "qwen3",
            "action": "researched",
            "details": f"Researched topic '{topic}', produced {len(findings)} chars of findings",
        })
        step("Log work", resp.get("success", False))

        # Print summary
        print("", file=sys.stderr)
        print("--- Research Output Preview ---", file=sys.stderr)
        preview = research_output[:500]
        if len(research_output) > 500:
            preview += "..."
        print(preview, file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(description="Broodlink researcher agent")
    parser.add_argument("--bridge-url", default="http://localhost:3310")
    parser.add_argument("--ollama-url", default="http://localhost:11434")
    parser.add_argument("--model", default="qwen3:1.7b")
    parser.add_argument("--jwt-file", default="~/.broodlink/jwt-qwen3.token")
    parser.add_argument("--topic", default="the benefits and challenges of multi-agent AI systems")
    args = parser.parse_args()
    args.jwt_file = str(args.jwt_file).replace("~", str(__import__("pathlib").Path.home()))
    asyncio.run(run(args))


if __name__ == "__main__":
    main()
