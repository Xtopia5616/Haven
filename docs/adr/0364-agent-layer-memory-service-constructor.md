# ADR 0364：AgentLayer 构造注入共享 MemoryService

- 状态：已采纳（2026-09-26）
- 范围：`AgentLayer::new` 的 memory service 构造边界
- 基线：HEAD `acc2845`；开始时工作区干净
- 关联：[ADR 0299](0299-react-compaction-summary-memory-store-port.md)、[ADR 0312](0312-memory-worker-summary-extraction-state-store.md)、[ADR 0362](0362-memory-runtime-app-ownership-audit.md)、[ADR 0363](0363-session-supervisor-typed-store-constructor.md)

## 背景

`AgentLayer::new` 接收 raw `Arc<Database>`，内部创建 `MemoryService`，再从它创建 typed stores、`MemoryWorker`、`MemoryRuntime` 和 `SystemPromptBuilder`。该 service 已是 prompt 与 worker 的共享能力边界，并拥有 embedding index 与有界 prompt-memory cache；构造入口继续接收 raw Database 会把组合根创建 service 的责任藏在 Agent 内部。

`MemoryService` 已由 `haven-agent` crate root 公开，生产构造调用者只有 app-binary。`MemoryRuntime` 的长期对象所有权仍受 ADR 0362 所述 readiness barrier 限制；本次无需改变 runtime 生命周期或 startup 顺序。

## 决定

1. `AgentLayer::new` 接收组合根创建的 `Arc<MemoryService>`，删除其 raw Database 参数和内部 `MemoryService::new` 调用。AgentLayer 继续从该 service 派生 `MemoryStore`、`MemoryFactStore`、`MemoryWorker`、`MemoryRuntime` 和 `SystemPromptBuilder`。
2. `AppState` 在已有 `Database`、Router 与 `ContextLimitsConfig` 后只创建一个 `MemoryService`，以 `Some(router.clone())` 和 `context_limits.embedding_chunk_size` 构造，再注入 AgentLayer。Router 用于 embedding 的配置、worker 的 inference port 与 ReActEngine 的 router 保持原有实例关系。
3. `MemoryWorker` 继续收到与 Agent memory 和 PromptBuilder 相同的 service，以及由该 service 取得的 fact store。PromptBuilder 继续收到同一 service，因此不会产生第二份 prompt cache 或 embedding index。`MemoryRuntime` 仍从 `SessionSupervisor::session_store()` 构造并长期由 AgentLayer 持有，仍使用同一 worker。
4. 所有生产与测试 `AgentLayer::new` 调用点迁移至 typed service 参数。测试 fixture 可用 `MemoryService::new` 从测试 Database、Router 和 `embedding_chunk_size` 创建 service；AgentLayer 的 test-only database handle 只从已注入 service 取出，不编译进生产。
5. 不改变 `SystemPromptBuilder::new`、`MemoryService::new`、`SessionSupervisor`、MemoryRuntime 所有权/startup barrier、schema、X12、event replay、outbox、ID、LLM routing 或 UI。MemoryService 与 SystemPromptBuilder 的 raw Database 构造 API 仍由各自边界保留。

## 行为兼容性

- Router/context limits：ReActEngine 继续使用同一个 Router 和完整 `ContextLimitsConfig`；MemoryWorker 继续用相同配置读取 transcript/fact/maintenance limits；MemoryService 仍用同一 Router 与 embedding route。
- Embedding chunk size：MemoryService 仍使用 `context_limits.embedding_chunk_size`，并沿用 `MemoryEmbeddingIndex` 的 provider-safe clamp。
- 共享关系：构造测试验证 AgentLayer memory、MemoryWorker 和 PromptBuilder 指向注入的同一个 service，并验证 MemoryRuntime 持有同一个 MemoryWorker。
- Recovery/lifecycle：MemoryRuntime prepare/replay、live consumer、dispatcher readiness、maintenance schedule 和 shutdown 顺序不变；需要把 runtime 对象移到 app 时继续按 ADR 0362 设计 readiness handoff。

## 验证

本切片按任务要求运行：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

UI 未修改，不运行 UI 门禁。没有数据库、wire 或持久化契约变化，无需数据重置。

## 回滚

回滚时恢复 `AgentLayer::new` 接收 Database 并在内部创建 MemoryService 的构造方式，同时回滚 AppState wiring、调用点、共享性测试与本 ADR/路线图/架构记录。无数据库或用户数据重置。
