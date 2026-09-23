# ADR 0247：MemoryWorker 使用窄推理端口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 后台记忆抽取与维护中的 FastChat 调用
- 关联：[ADR 0029](0029-agent-fact-extraction-boundary.md)、[ADR 0107](0107-durable-memory-extraction-outbox.md)、[ADR 0201](0201-unify-fact-storage-and-memory-worker-naming.md)、[ADR 0232](0232-memory-vector-space-identity-owner.md)

## 背景

`MemoryWorker` 直接持有通用 `LlmRouter`，在事实抽取、谓词合并和矛盾裁决中重复指定
`FastChat`，并直接消费 Router 的响应 DTO。这样记忆后台编排依赖整个模型路由接口，
测试也需要构造通用 LLM 客户端，即使它只需要一个能力：检查 FastChat 是否可用并取得
一段文本。

## 决定

1. `MemoryInferencePort` 是 `MemoryWorker` 的窄推理依赖，提供 FastChat 可用性检查和
   system/user prompt 到响应文本的调用。
2. `RouterMemoryInferencePort` 独占 `LlmRouter`、`RequestKind::FastChat` 和
   `LlmResponse` 的适配。`AgentLayer` 在组合时创建并注入此端口。
3. `MemoryWorker` 继续负责提示组装、响应解析、事实准入、游标/节流、维护流程和持久化；
   本次不拆分完整 `MemoryRuntime`，也不移动事实规则到 Memory crate。
4. `MemoryService` 与 `MemoryEmbeddingIndex` 的 embedding、recall 和向量空间 identity
   职责保持现状。

## 影响与行为边界

FastChat 请求类型、提示文本、调用顺序、可用性短路、错误处理、抽取与维护结果、持久化
游标和 outbox 语义保持不变。端口不包含数据库、session 状态、重试策略或 provider 配置，
不改变 schema、IPC、用量记录和数据重置要求。

## 替代方案

- 继续让 `MemoryWorker` 直接调用 Router：代码量最少，但记忆调度仍依赖通用 Router 表面，
  三个工作流还要重复表达同一 FastChat 选择。
- 新建完整 MemoryRuntime 并迁移 durable event 消费、索引和 recall：目标更完整，但会把
  单一接口收口扩展为跨 Agent/Memory 的生命周期重构；留待阶段 7 后续独立切片。

## 验证

- 单元测试通过注入的 `MemoryInferencePort` 执行事实抽取，并验证写入结果与单次调用。
- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚

恢复 `MemoryWorker` 持有 `LlmRouter` 并直接调用 FastChat 的代码，删除端口和本 ADR；不涉及
持久化格式或用户数据，无需重置。
