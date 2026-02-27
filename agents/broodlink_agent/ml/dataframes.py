# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Convert Broodlink data to pandas DataFrames for analysis."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import pandas as pd

from ..models import Memory, WorkLog


def memories_to_dataframe(memories: list[Memory]) -> "pd.DataFrame":
    """Convert a list of Memory objects to a pandas DataFrame.

    Example::

        async with AsyncBroodlinkClient() as client:
            memories = await client.memory.recall()
            df = memories_to_dataframe(memories)
            print(df.describe())
    """
    import pandas as pd

    rows = [m.model_dump() for m in memories]
    return pd.DataFrame(rows) if rows else pd.DataFrame(columns=["topic", "content", "tags", "agent_name", "updated_at"])


def work_log_to_dataframe(entries: list[WorkLog]) -> "pd.DataFrame":
    """Convert a list of WorkLog entries to a pandas DataFrame."""
    import pandas as pd

    rows = [e.model_dump() for e in entries]
    df = pd.DataFrame(rows) if rows else pd.DataFrame(columns=["agent_name", "action", "details", "created_at"])
    if "created_at" in df.columns and not df.empty:
        df["created_at"] = pd.to_datetime(df["created_at"], errors="coerce")
    return df
