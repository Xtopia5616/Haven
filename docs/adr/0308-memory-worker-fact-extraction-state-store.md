# ADR 0308：MemoryWorker session 事实提取状态通过专用 Store

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryService`、`MemoryWorker::infer_facts_inner`、`MemoryFactExtractionStore`
- 关联：[ADR 0301](0301-memory-worker-outbox-through-memory-store.md)、[ADR 0307](0307-memory-worker-known-facts-through-memory-fact-store.md)

## 背景

普通 session 事实提取的 `infer_facts_inner` 仍经 `MemoryDatabase` 调度 SQLite：读取上次尝试的节流时间戳、加载 `messages` 与 `session_steps` 投影、读取 `fact_extraction.{session_id}` 用户消息游标，并在低信任消息跳过或提取成功后推进游标。Agent 因而同时持有提取策略和这些存储细节。

这条路径与 compaction-summary 的 episode cursor、维护与矛盾/谓词算法、embedding catch-up 是不同的工作单元。本 ADR 只收口普通 session 事实提取所需的持久化端口。

## 决定

1. 在 `haven-memory` 新增专用 `MemoryFactExtractionStore`，只提供有意图的操作：读取 last-attempt 时间戳、读取 transcript projections、读取 extraction cursor、写入 attempt 时间戳和推进 cursor。它在 Memory 内部使用 `Database::run_blocking`，不暴露 closure 或通用 blocking facade。
2. transcript read 在一个 blocking closure 中按原顺序调用既有 `get_session_messages` 和 `get_session_steps`，返回 typed `FactExtractionTranscript`。Store 保存现有 KV key 约定，并传播底层错误。
3. `MemoryService` 创建并持有共享 store，`MemoryWorker` 从同一 service 取得 store。Worker 继续拥有节流判定、窗口构造、低信任消息处理、LLM 调用、事实写入策略，以及仅在可接受结果后推进 cursor 的决策。
4. 保留 `MemoryWorker` 的 `MemoryDatabase` 句柄。`persist_fact_batch` 中的批量候选策略、存在性查询与事实写入，`run_memory_maintenance`、矛盾裁决与谓词合并、compaction-summary 的 episode cursor，以及 summary 路径对共享节流时间戳的直接读写都不在本切片迁移；测试 fixture 的直接 DB 操作也不迁移。embedding catch-up 继续使用 `MemoryService` 已有的 `MemoryEmbeddingStore` 边界。不能称 MemoryWorker 已无 DB。
5. 保持原有错误映射与控制流：读取或写入失败仍记录原 warning 并返回 `false`；节流返回不触碰 cursor；模型失败或事实持久化失败不推进 cursor；空事实结果有效并推进 cursor；只含低信任新用户行的空窗口仍推进到最后一个 user message。outbox 取消仍中止 live inference 并保留 durable marker；本切片不改变底层 blocking closure 的取消语义。

## 影响与验证

- 无 schema、IPC、配置或依赖变化，无需重置数据。
- `haven-memory` 的 `Database::open_in_memory()` 测试覆盖 transcript/step 读取、缺表错误、attempt 时间戳和 cursor 的往返及 KV 缺表错误。
- Agent 回归测试覆盖取消时保留 outbox marker 且不推进 cursor、缺少 transcript projection 时非致命失败、以及从持久化 message/step 投影构造增量窗口时的顺序、reasoning 排除和 tool observation 语义。
- 验收命令：`cargo fmt --all -- --check`、`cargo test -p haven-memory --locked`、`cargo test -p haven-agent --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。

## 替代方案

- 把这些方法加到负责 episode/outbox marker 的 `MemoryStore`：会混合两类不同持久化职责，拒绝。
- Agent 侧添加 `run_blocking` 包装方法：仍让 Agent 拥有 SQL 调度与 key 构造，拒绝。
- 同轮迁移 MemoryWorker 所有 `Database` 路径：会合并维护/embedding/summary 等不同策略边界，超出本切片范围，拒绝。

## 回滚

将 `infer_facts_inner` 恢复为原有 `MemoryDatabase` 读取与 KV 写入，删除 `MemoryFactExtractionStore`、`MemoryService` accessor、测试和相关文档条目即可。无数据格式或迁移要求。
