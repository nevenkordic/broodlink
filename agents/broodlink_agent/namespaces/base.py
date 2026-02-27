# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Base namespace class for type-safe tool wrappers."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .._transport import AsyncTransport


class BaseNamespace:
    """Base class for all namespace wrappers."""

    def __init__(self, transport: AsyncTransport) -> None:
        self._t = transport

    async def _call(self, tool: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        return await self._t.call(tool, params)
