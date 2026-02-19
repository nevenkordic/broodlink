# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""LLM client for OpenAI-compatible chat completions with fallback support."""

from __future__ import annotations

import aiohttp

from .config import AgentConfig


class LLMClient:
    """Sends chat completion requests with automatic fallback to secondary model."""

    def __init__(self, session: aiohttp.ClientSession, config: AgentConfig) -> None:
        self.session = session
        self.config = config

    async def complete(
        self,
        messages: list[dict],
        tools: list[dict] | None = None,
    ) -> dict:
        """Send a chat completion request. Falls back to secondary model on failure."""
        # Try primary model first
        try:
            return await self._send_request(
                self.config.lm_url, self.config.lm_model, messages, tools
            )
        except Exception as primary_err:
            # If fallback is configured, try it
            if self.config.lm_fallback_url and self.config.lm_fallback_model:
                print(
                    f"  WARNING: Primary LLM failed ({primary_err}), "
                    f"trying fallback: {self.config.lm_fallback_model}"
                )
                try:
                    return await self._send_request(
                        self.config.lm_fallback_url,
                        self.config.lm_fallback_model,
                        messages,
                        tools,
                    )
                except Exception as fallback_err:
                    raise RuntimeError(
                        f"Both LLMs failed. Primary: {primary_err}. Fallback: {fallback_err}"
                    ) from fallback_err
            raise

    async def _send_request(
        self,
        base_url: str,
        model: str,
        messages: list[dict],
        tools: list[dict] | None,
    ) -> dict:
        """Send a single completion request to the given endpoint."""
        payload: dict = {
            "model": model,
            "messages": messages,
            "temperature": 0.7,
            "max_tokens": 4096,
            "stream": False,
        }
        if tools:
            payload["tools"] = tools
            payload["tool_choice"] = "auto"

        async with self.session.post(
            f"{base_url}/v1/chat/completions",
            json=payload,
            timeout=aiohttp.ClientTimeout(total=self.config.request_timeout),
        ) as resp:
            if resp.status != 200:
                text = await resp.text()
                raise RuntimeError(f"LLM returned {resp.status}: {text[:200]}")
            data = await resp.json()
            return data["choices"][0]["message"]
