#!/usr/bin/env python3
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Backward-compatible wrapper — delegates to broodlink_agent package.

Usage unchanged:
    python agents/broodlink-agent.py           # interactive mode
    python agents/broodlink-agent.py --listen   # NATS listener mode

Or use the package directly:
    python -m broodlink_agent
    python -m broodlink_agent --listen
"""

from broodlink_agent.cli import main

main()
