# ADR 0232：Memory vector-space identity 唯一解析边界

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 embedding 索引与 prompt-memory cache identity
- 关联：[ADR 0021](0021-agent-memory-embedding-boundary.md)、[ADR 0063](0063-memory-context-and-recall.md)、[ADR 0105](0105-memory-current-contract-and-retrieval.md)、[ADR 0169](0169-memory-prompt-boundaries.md)、[ADR 0201](0201-unify-fact-storage-and-memory-worker-naming.md)

## 背景

`MemoryEmbeddingIndex` 已负责检查 embedding 路由配置，并把 provider、wire style、endpoint 和 model 名解析为持久向量空间指纹。`MemoryService` 为 prompt-memory cache key 又单独执行同一检查和解析，造成同一业务事实有两处权威实现。

## 决定

1. `MemoryEmbeddingIndex` 是当前向量空间 identity 的唯一解析者，并继续复用索引生命周期使用的 `configured_identity`。
2. `MemoryService::current_embedding_model` 只委托给索引；没有索引或 identity 不可用时返回空字符串。
3. 缺少 embedding 配置或模型名为空时，prompt memory 继续走 keyword fallback；缓存 key 的字段、格式和失效维度保持不变。
4. `MemoryService` 保留 Router，用于 prompt embedding 请求与 context-window 查询；MemoryWorker 装配和调用方式保持不变。

## 替代方案

- 把指纹解析移入 `MemoryService`：会让向量空间规则脱离管理 embedding 生命周期的索引组件，拒绝。
- 新建共享 identity helper 或配置快照：当前只存在两个调用点，额外抽象会增加组件和装配，而不能形成更清晰的运行时所有权，拒绝。

## 影响与验证

配置判定与指纹解析只有一个 Agent 侧实现。数据库 schema、embedding 请求、prompt cache key 格式、keyword fallback 和 MemoryWorker 编排均不变。测试覆盖未配置、空模型名、同模型名更换 endpoint 后 identity 改变，以及 Service 与 Index 的一致性。

验证：`cargo fmt -p haven-agent -- --check`、`cargo test --locked -p haven-agent`、`cargo check --workspace --locked`、`cargo clippy --locked -p haven-agent -- -D warnings`。

## 回滚

恢复 `MemoryService` 中原有的 router 配置检查与指纹计算，并删除 `MemoryEmbeddingIndex::current_vector_space_identity`。不涉及持久数据、schema 或缓存格式，无需重置。
