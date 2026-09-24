# ADR 0303：Agent embedding 索引通过 MemoryEmbeddingStore 持久化

- 状态：Accepted
- 日期：2026-09-25
- 范围：`MemoryEmbeddingIndex` 的向量索引读写、维度检查、LSH 维护与 typed vector recall
- 关联：[ADR 0021](0021-agent-memory-embedding-boundary.md)、[ADR 0063](0063-memory-context-and-recall.md)、[ADR 0105](0105-memory-current-contract-and-retrieval.md)、[ADR 0301](0301-memory-worker-outbox-through-memory-store.md)、[ADR 0302](0302-tools-memory-fact-store-port.md)

## 背景

ADR 0021 将 embedding provider 调用、有限 catch-up、模型切换、向量召回和 LSH 重建集中到 Agent 的 `memory_index.rs`。provider 编排属于 Agent，但 `MemoryEmbeddingIndex` 仍直接持有 `Arc<Database>`，并为模型列表、待嵌入行与文本、维度、向量保存、LSH 检查/重建和 vector recall 自行调度 `run_blocking`。这些 SQL 仓库操作和 SQLite blocking-pool 调度属于 `haven-memory`。

## 决定

1. `haven-memory` 新增窄异步 `MemoryEmbeddingStore`，内部持有共享 `Arc<Database>`，只暴露 embedding 生命周期需要的 typed 操作：列出存储模型、清空向量索引、读取有界待嵌入项及其可见文本、列出模型维度、批量保存 provider 向量、检查并重建落后的 LSH，以及按 `MemoryQuery` 执行 typed vector recall。所有 DB 工作由 store 在现有 `run_blocking` 边界执行。
2. 待嵌入结果使用封闭的事实/episode 实体类型。store 保留 Database 现有的每域 backlog 上限与顺序，并在文本离开 Memory 层前调用统一可见性策略剔除敏感内容；缺失行、单行保存失败和数据库错误保持现有结果行为。批保存报告各行失败，Agent 保留最多三条逐行警告与汇总日志。
3. `MemoryEmbeddingIndex` 删除 Database 字段和所有 `run_blocking` 闭包，仅依赖 `MemoryEmbeddingStore`。它继续拥有 embedding request routing、批大小、provider 返回行数/模型/维度校验、模型或维度变化时清空索引的决策、错误降级策略与维护互斥门控。
4. `MemoryEmbeddingStore::vector_recall` 复用 `MemoryRetriever::vector`，故模型过滤、事实重读与敏感过滤、session/subject scope 和确定性排序不变。provider 不可用、空向量、向量空间切换或维度不匹配仍使 Agent 采用原 keyword fallback；持久层/typed retriever 错误继续传播为错误。
5. `MemoryService` 从已有共享 Database 创建 store 并注入 index；不创建新连接，不改变 schema、持久化格式、embedding 身份散列或 prompt-cache key。

将 provider 请求移进 Memory 会反转依赖并混合 provider 与持久层职责；继续让 Agent 调度 raw Database 则会暴露 SQLite 细节。该窄 store 保持现有边界并清除直接数据库依赖。

## 影响与验证

- 无 schema、IPC、配置或依赖变更，无需数据重置。
- MemoryEmbeddingStore 测试覆盖敏感内容排除、事实 backlog 上限、模型/维度读写、清空、LSH 重建、typed vector recall、逐行保存失败后继续及数据库错误传播；已有 Agent 测试继续覆盖向量空间身份与 database error fail-closed 行为。
- 验收命令：`cargo fmt --all -- --check`、`cargo check -p haven-memory -p haven-agent --locked`、`cargo test --locked -p haven-memory`、`cargo test --locked -p haven-agent`。

## 回滚

恢复 `MemoryEmbeddingIndex` 持有 Database 并在 Agent 内调度原嵌入 SQL 操作，删除 `MemoryEmbeddingStore` 与测试、本 ADR、README 索引、路线图和架构更新即可。无需数据库迁移或重置。
