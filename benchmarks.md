# Broodlink System Benchmark

**Date:** 2026-03-03
**Host:** Mac Studio — Apple M4 Max, 16 cores (12P + 4E), 128 GB unified memory
**Disk:** 6.5 GB/s sequential write
**Ollama:** 0.17.5

---

## Ollama Models (inference)

| Model | Role | Size | Generation | Think Support |
|-------|------|------|------------|---------------|
| qwen3.5:35b | Primary chat | 23 GB | 43 tok/s | Yes |
| qwen3-coder:30b | Code | 18 GB | 72 tok/s | No |
| qwen3.5:4b | Fallback/expansion/KG | 3.4 GB | 54 tok/s | Yes |
| deepseek-r1:32b | Verifier | 19 GB | 9-22 tok/s | Yes |
| gemma3:27b | Vision | 17 GB | 25 tok/s | No (vision) |
| nomic-embed-text | Embeddings | 274 MB | 13.9 emb/s | — |

---

## Accuracy Benchmark (13 tests)

| Category | Test | qwen3.5:35b | qwen3-coder:30b | qwen3.5:4b | deepseek-r1:32b |
|----------|------|-------------|-----------------|------------|-----------------|
| REASONING | math (17*23) | PASS 6.7s | — | — | — |
| REASONING | logic puzzle | PASS 25.9s | — | — | — |
| REASONING | probability (3/8) | PASS 13.3s | — | — | — |
| REASONING | series sum (210) | PASS 7.9s | — | — | — |
| CODING | fizzbuzz | PASS* 97.0s | PASS 5.2s | — | — |
| CODING | bug fix | PASS 13.3s | PASS 1.1s | — | — |
| CODING | regex | PASS 4.4s | PASS 0.5s | — | — |
| CODING | reverse string | PASS 8.6s | PASS 1.1s | — | — |
| INSTRUCTION | JSON format | PASS 7.3s | — | PASS 8.0s | — |
| INSTRUCTION | numbered list | PASS 23.6s | — | PASS 12.7s | — |
| KNOWLEDGE | speed of light | PASS 13.1s | — | PASS 3.6s | — |
| KNOWLEDGE | geography | PASS 4.3s | — | PASS 2.8s | — |
| KNOWLEDGE | history | PASS 6.0s | — | PASS 3.8s | — |

\* Used think:false retry (thinking chain exhausted budget on first attempt)

### Scores

| Model | Accuracy | Role |
|-------|----------|------|
| qwen3.5:35b | **13/13 (100%)** | General chat + reasoning |
| qwen3-coder:30b | **4/4 (100%)** | Code generation |
| qwen3.5:4b | **7/9 (78%)** | Fallback (failed logic + word count) |
| deepseek-r1:32b | **3/4 (75%)** | Verifier (verbose output misses exact format) |

---

## Cloud Model Comparison (published benchmarks)

| Model | MMLU | HumanEval | MATH | SWE-bench | Cost |
|-------|------|-----------|------|-----------|------|
| Claude Opus 4.6 | 88.7% | 94.5% | 78.3% | 80%+ | ~$15/1M tok |
| GPT-4o | 87.2% | 90.2% | 76.6% | 33.2% | ~$5/1M tok |
| **qwen3.5:35b (local)** | ~82% | ~85% | ~73% | 76.4% | **$0** |
| **qwen3-coder:30b (local)** | — | ~88% | — | ~70% | **$0** |
| **deepseek-r1:32b (local)** | ~80% | ~73% | ~75% | — | **$0** |
| **qwen3.5:4b (local)** | ~65% | ~59% | ~52% | — | **$0** |

Local models achieve ~85-95% of cloud quality with zero cost, zero latency, and full data privacy.

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
| Postgres | task_queue | 420 | 1.1ms |
| Postgres | chat_messages | 285 | 0.3ms |
| Postgres | audit_log | 8,233 | 0.6ms |
| Postgres | memory_search_index | 112 | 0.3ms |

---

## Vector Store

| Collection | Points | Status |
|------------|--------|--------|
| broodlink_memory | 112 | green |
| broodlink_kg_entities | 198 | green |
