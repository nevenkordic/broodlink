# Broodlink Examples

Working demos that show agents using Broodlink's infrastructure to accomplish
real tasks — not toy scripts, but multi-step workflows with LLM reasoning,
persistent memory, inter-agent messaging, and knowledge graph queries.

## Prerequisites

All examples assume the following are running locally:

| Service | Default URL | Check |
|---------|-------------|-------|
| beads-bridge | `http://localhost:3310` | `curl -sf http://localhost:3310/health` |
| Ollama | `http://localhost:11434` | `curl -sf http://localhost:11434/api/tags` |

You also need JWT tokens for at least two agents:

```
~/.broodlink/jwt-qwen3.token    # researcher agent
~/.broodlink/jwt-claude.token   # writer agent
```

Generate these with `scripts/onboard-agent.sh` if they don't exist.

Python 3.8+ with `aiohttp` installed (`pip install aiohttp`).

## Examples

### `research-report/` — Two Agents Collaborate on a Research Report

A researcher agent investigates a topic using memory search, knowledge graph
traversal, and LLM reasoning, then hands off findings to a writer agent that
synthesizes everything into a structured report.

```bash
cd examples/research-report
bash run.sh

# Custom topic:
TOPIC="quantum computing applications in drug discovery" bash run.sh
```

**What happens:**

1. **Researcher** (qwen3 JWT) registers itself, searches existing memories and
   the knowledge graph for relevant context, generates research findings via
   Ollama, stores them in Broodlink memory, and sends a handoff message to the
   writer agent.

2. **Writer** (claude JWT) registers itself, reads the handoff message, retrieves
   the stored findings and takeaways, searches for additional context, then uses
   Ollama to synthesize a structured report. The report is stored in memory and
   printed to stdout.

**Bridge tools used:** `agent_upsert`, `semantic_search`, `graph_search`,
`store_memory`, `recall_memory`, `send_message`, `read_messages`, `log_work`,
`log_decision` (10 distinct tools across both agents).

**Artifacts persisted:** Research findings, takeaways, final report (all in
Broodlink memory), plus work log and decision log entries.
