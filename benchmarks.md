# Broodlink System Benchmark

**Date:** 2026-02-28
**Host:** Mac Studio — Apple M4 Max, 16 cores (12P + 4E), 128 GB unified memory
**Disk:** 6.5 GB/s sequential write

---

## Ollama Models (inference)

| Model | Role | Size | Prompt | Generation | Total |
|-------|------|------|--------|------------|-------|
| qwen3:30b-a3b | Primary chat | 18 GB | 104 tok/s | 95.7 tok/s | 4.5s |
| deepseek-r1:14b | Verifier | 9 GB | 156 tok/s | 46.2 tok/s | 7.0s |
| qwen3:1.7b | Fallback | 1.4 GB | 606 tok/s | 191.2 tok/s | 1.4s |
| gemma3:27b | Vision | 17 GB | 78 tok/s | 25.1 tok/s | 6.5s |
| nomic-embed-text | Embeddings | 274 MB | — | 13.9 emb/s | 72ms/emb |

---

## Service Health Latency

| Service | Port | Response |
|---------|------|----------|
| beads-bridge | 3310 | 26ms |
| status-api | 3312 | 23ms |
| mcp-server | 3311 | 29ms |
| a2a-gateway | 3313 | 27ms |

---

## Bridge Tool Call Latency

| Tool | Backend | Latency |
|------|---------|---------|
| health_check | None | 36ms |
| list_tasks | Postgres | 38ms |
| semantic_search | Qdrant + Ollama | 135ms |

---

## Database Query Performance

| Store | Table | Rows | Query Time |
|-------|-------|------|------------|
| Postgres | task_queue | 402 | 1.1ms |
| Postgres | chat_messages | 285 | 0.3ms |
| Postgres | audit_log | 8,233 | 0.6ms |
| Postgres | memory_search_index | 91 | 0.3ms |

---

## Vector Store

| Collection | Points | Status |
|------------|--------|--------|
| broodlink_memory | 103 | green |
| broodlink_kg_entities | 180 | green |
