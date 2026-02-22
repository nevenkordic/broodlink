#!/usr/bin/env python3
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
Writer agent: reads researcher handoff, retrieves findings from memory,
synthesizes a structured report via LLM, and persists the result.

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
        "temperature": 0.6,
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

        # 1. Register agent (uses JWT identity — claude)
        # Valid Dolt roles: strategist, worker, researcher, monitor
        resp = await call("agent_upsert", {
            "agent_id": "claude",
            "display_name": "Writer (claude)",
            "role": "strategist",
            "capabilities": "report writing, synthesis, memory retrieval",
            "cost_tier": "high",
        })
        step("Register agent", resp.get("success", False))

        # 2. Read messages — look for handoff from researcher
        resp = await call("read_messages", {"unread_only": False, "limit": 10})
        handoff = None
        if resp.get("success"):
            data = resp.get("data", {})
            messages = data.get("messages", [])
            for msg in messages:
                if msg.get("subject") == "research-handoff":
                    try:
                        handoff = json.loads(msg.get("body") or msg.get("content") or "{}")
                    except json.JSONDecodeError:
                        pass
                    break
            if handoff:
                step("Read handoff message", True, f"slug={handoff.get('slug', '?')}")
                slug = handoff.get("slug", slug)
            else:
                step("Read handoff message", True, "no handoff found, using topic slug")
        else:
            step("Read handoff message", False, resp.get("error", ""))

        # 3. Retrieve findings from memory
        findings_key = f"research:{slug}:findings"
        resp = await call("recall_memory", {"topic_search": findings_key})
        findings = ""
        if resp.get("success"):
            data = resp.get("data", {})
            memories = data.get("memories", [])
            if memories:
                findings = memories[0].get("content", "")
                step("Retrieve findings", True, f"{len(findings)} chars")
            else:
                step("Retrieve findings", True, "empty — will research inline")
        else:
            step("Retrieve findings", False, resp.get("error", ""))

        # 4. Retrieve takeaways from memory
        takeaways_key = f"research:{slug}:takeaways"
        resp = await call("recall_memory", {"topic_search": takeaways_key})
        takeaways = ""
        if resp.get("success"):
            data = resp.get("data", {})
            memories = data.get("memories", [])
            if memories:
                takeaways = memories[0].get("content", "")
                step("Retrieve takeaways", True, f"{len(takeaways)} chars")
            else:
                step("Retrieve takeaways", True, "none stored separately")
        else:
            step("Retrieve takeaways", False, resp.get("error", ""))

        # 5. Search for additional context
        resp = await call("semantic_search", {"query": topic, "limit": 3})
        extra_context = []
        if resp.get("success"):
            data = resp.get("data", {})
            results = data.get("results", [])
            extra_context = [
                r.get("content", "")
                for r in results
                if r.get("content") and r.get("content") != findings
            ]
            step("Search extra context", True, f"{len(extra_context)} additional")
        else:
            step("Search extra context", True, "none found")

        # 6. Synthesize report via LLM
        source_material = []
        if findings:
            source_material.append(f"RESEARCH FINDINGS:\n{findings}")
        if takeaways:
            source_material.append(f"KEY TAKEAWAYS:\n{takeaways}")
        if extra_context:
            source_material.append(
                "ADDITIONAL CONTEXT:\n" + "\n---\n".join(extra_context[:3])
            )

        if not source_material:
            source_material.append(
                f"No prior research found. Generate a report on: {topic}"
            )

        system_prompt = (
            "You are a report writer. Given research findings and context, produce "
            "a well-structured report with:\n\n"
            "# [Report Title]\n\n"
            "## Executive Summary\n"
            "[2-3 paragraph overview]\n\n"
            "## Key Findings\n"
            "[Numbered findings with explanations]\n\n"
            "## Analysis\n"
            "[Deeper analysis connecting the findings]\n\n"
            "## Recommendations\n"
            "[Actionable recommendations based on the findings]\n\n"
            "Write clearly and substantively. No filler or boilerplate."
        )

        user_prompt = (
            f"Topic: {topic}\n\n"
            + "\n\n".join(source_material)
            + "\n\nProduce the structured report now."
        )

        step("Generating report", True, f"model={args.model}")
        report = await ollama_chat(
            session, args.ollama_url, args.model,
            [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
        )

        if report.startswith("[LLM error"):
            step("LLM generation", False, report)
            sys.exit(1)

        step("LLM generation", True, f"{len(report)} chars")

        # 7. Store report in memory
        resp = await call("store_memory", {
            "topic": f"research:{slug}:report",
            "content": report,
            "tags": "research,example,report",
        })
        step("Store report", resp.get("success", False))

        # 8. Log decision
        resp = await call("log_decision", {
            "decision": f"Synthesized research report on '{topic}'",
            "reasoning": (
                f"Combined {len(findings)} chars of findings"
                + (f", {len(takeaways)} chars of takeaways" if takeaways else "")
                + (f", {len(extra_context)} extra context items" if extra_context else "")
                + " into a structured report via LLM."
            ),
            "outcome": f"Report stored as research:{slug}:report",
        })
        step("Log decision", resp.get("success", False))

        # 9. Log work
        resp = await call("log_work", {
            "agent_name": "claude",
            "action": "wrote",
            "details": f"Synthesized report on '{topic}', {len(report)} chars",
        })
        step("Log work", resp.get("success", False))

        # Output the report to stdout (captured by run.sh)
        print(report)


def main() -> None:
    parser = argparse.ArgumentParser(description="Broodlink writer agent")
    parser.add_argument("--bridge-url", default="http://localhost:3310")
    parser.add_argument("--ollama-url", default="http://localhost:11434")
    parser.add_argument("--model", default="qwen3:1.7b")
    parser.add_argument("--jwt-file", default="~/.broodlink/jwt-claude.token")
    parser.add_argument("--topic", default="the benefits and challenges of multi-agent AI systems")
    args = parser.parse_args()
    args.jwt_file = str(args.jwt_file).replace("~", str(__import__("pathlib").Path.home()))
    asyncio.run(run(args))


if __name__ == "__main__":
    main()
