# ADR 0304：Agent memory recall 查询通过 MemoryRecallStore

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryService::prompt_candidates` 与 `MemoryService::recall` 的持久化读取边界
- 关联：[ADR 0021](0021-agent-memory-embedding-boundary.md)、[ADR 0063](0063-memory-context-and-recall.md)、[ADR 0105](0105-memory-current-contract-and-retrieval.md)、[ADR 0301](0301-memory-worker-outbox-through-memory-store.md)、[ADR 0303](0303-agent-memory-embedding-store-port.md)

## 背景

ADR 0303 将 embedding 索引的持久化调度移入 `MemoryEmbeddingStore`，但 `MemoryService` 仍直接读取 `Database::memory_revision`，并在 Agent 内通过 `run_blocking` 调用 `MemoryRetriever` 和 facts 查询。Agent 因而继续掌握 keyword/vector 查询的数据库调度、事实 hydration 与可见性过滤。完整 `MemoryRetriever::retrieve` 也仍由 `MemoryService` 自行调度。

prompt 归一化、embedding provider 调用、prompt cache、候选合并和预算是 Agent 编排；keyword/vector SQL 调度、统一可见性与 recall 投影属于 Memory。该边界不需要把 prompt-specific DTO 搬进 Memory，也不要求迁移 MemoryWorker 的其他持久化路径。

## 决定

1. `haven-memory` 新增窄异步 `MemoryRecallStore`，持有共享的 `Arc<Database>`，并暴露 typed ports：keyword recall、model-scoped vector recall、可见 user facts、按 ID 读取并过滤/脱敏 facts，以及 `MemoryQuery` 加可选 vector hits 的完整 retrieve。SQL/检索操作全部由该 store 在现有 `run_blocking` 边界调度。
2. `MemoryRecallStore` 内部复用 `MemoryRetriever` 与现有 `Database` 查询，保留关键词采样、候选 limit、事实 subject/session scope、敏感事实过滤、provenance snippet 脱敏、向量模型过滤、事实重读与稳定排序。`memory_revision` 是进程内原子失效计数器，不执行 SQLite 读取；store 直接暴露该原子读取，保留现有 prompt cache revision key 语义。
3. `MemoryService::prompt_candidates` 只通过 `MemoryRecallStore` 读取候选数据。Agent 继续负责空白归一化与字符上限、缓存键和 32 项 LRU、embedding provider 调用及失败时不缓存、keyword/vector 选择、候选合并顺序和 prompt 专属 limit。query、exclude-session、candidate hydration 顺序与错误上下文保持不变。
4. `MemoryService::recall` 通过 `MemoryRecallStore` 完成 keyword/hybrid retrieve；`MemoryEmbeddingIndex` 继续经 `MemoryEmbeddingStore` 读取模型列表与向量维度，随后将 provider 获得的 vector 交给 recall store 获取 typed hits。provider 未配置、不可用、空向量、vector-space 变化或维度不匹配仍回退 keyword，数据库/retriever 错误继续传播。
5. `MemoryEmbeddingStore` 只负责 embedding 生命周期读写与 LSH 维护；移除其 vector recall 重复入口。`MemoryService::database_handle` 和 worker 兼容边界保留，MemoryWorker 的 fact extraction、maintenance、kv 和其余 Database 路径不在本切片。

这保持了 Agent → Memory 的单向依赖，不把 prompt DTO 或缓存策略放入持久层，也不以一次性全量 Worker 迁移扩大写集。

## 影响与验证

- 无 schema、IPC、配置或依赖变化，无需重置数据。
- MemoryRecallStore 测试覆盖 revision 更新、keyword/vector 查询、scope 与敏感行过滤、user facts seed/by-ID hydration、keyword fallback 和空结果 diagnostics。
- Agent 回归测试覆盖 prompt 查询归一化、敏感事实过滤、排除当前 session、缓存命中及 revision 失效，以及无 embedding index 时的 keyword recall 和敏感事实过滤。
- 验收命令：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 保留 `MemoryService` 对 Database/retriever 的 blocking 调度：仍让 Agent 知道 Memory SQL 与检索实现，拒绝。
- 将 prompt candidate DTO 或 cache 放进 Memory：混合 prompt policy 与持久层职责，拒绝。
- 将 MemoryWorker 全量迁移和 recall store 合并成一轮：超出独立可验证查询边界，且不影响本切片目标，拒绝。

## 回滚

恢复 `MemoryService` 和 `MemoryEmbeddingIndex` 现有 Database/retriever 调用，删除 `MemoryRecallStore` 及测试、本 ADR、README 索引和路线图记录即可。无数据格式或迁移要求。
