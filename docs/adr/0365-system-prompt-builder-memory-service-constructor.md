# ADR 0365：SystemPromptBuilder 构造注入 MemoryService

- 状态：已采纳（2026-09-26）
- 范围：`SystemPromptBuilder` 的公开构造边界
- 基线：HEAD `cb2f2a6`；开始时工作区干净
- 关联：[ADR 0364](0364-agent-layer-memory-service-constructor.md)、[ADR 0363](0363-session-supervisor-typed-store-constructor.md)

## 背景与调用审计

`SystemPromptBuilder::new` 接收 `Arc<Database>`，并在内部以 `router=None`、embedding chunk size `64` 新建 `MemoryService`。生产 `AgentLayer` 已由 ADR 0364 从组合根接收唯一的 `MemoryService`，并通过 `with_memory_service` 将同一实例交给 prompt builder、memory worker 和 memory runtime；raw Database 构造入口只被 `haven-agent` 内的单元/集成测试使用。

工作区搜索未发现 `haven-agent` 外部的调用点。不过 `SystemPromptBuilder` 已从 crate root 公开导出，仓库审计无法判定未知下游是否依赖它。Haven 当前为 `0.1.0` 测试版，开发标准允许移除没有明确兼容价值的 API；下游若存在，需显式创建 `MemoryService` 并调用 typed constructor。

## 决定

1. 删除 `SystemPromptBuilder::new(tools, Arc<Database>)`。`SystemPromptBuilder::with_memory_service(tools, Arc<MemoryService>)` 是唯一公开 builder 构造入口；不保留 raw Database overload、deprecated compatibility wrapper 或第二个 production constructor。
2. prompt 单元与集成测试从 fixture Database 创建 `MemoryService::new(db, None, 64)`，然后使用 `with_memory_service`。这些参数与旧便利构造函数一致，不改变测试行为。
3. 保留 AgentLayer 既有的共享 service 组合方式。PromptBuilder 通过 `PromptContextProvider` 持有注入 service 的 `Arc`，不创建新的 service；因此它与 Agent memory、Worker 共享同一 prompt-memory cache、embedding index 和 typed stores。
4. 不改变 `PromptContextProvider`、prompt cache/retrieval、router、embedding chunk size 来源、prompt 输出、AgentLayer、MemoryRuntime、SessionSupervisor、DB/X12/wire/UI 或 runtime semantics。`MemoryService::new` 自身的 raw Database 构造边界不在本 ADR 范围内。

## 边界与共享测试

保留 `agent_memory_consumers_share_the_injected_memory_service`：测试用 `Arc::ptr_eq` 验证 Agent memory、Worker 与 PromptBuilder 指向同一个注入实例，并验证 MemoryRuntime 持有同一个 Worker。该身份断言同时证明 PromptBuilder 与其它消费者共享 `MemoryService` 所拥有的 cache/index，而非在 builder 内创建第二份服务。

## 兼容性与回滚

这是 Rust source API 收窄，不涉及持久数据、配置、wire 或数据库 schema。未知下游调用方需要迁移到 `MemoryService::new` + `SystemPromptBuilder::with_memory_service`；不增加兼容 wrapper，避免重新暴露独立 cache owner 的 raw Database 入口。

回滚时可恢复旧 `new`，但会重新允许 builder 静默构造与 Agent memory 分离的 service/cache。没有数据库或用户数据重置要求。

## 验证

按本轮任务要求运行：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

UI 未修改，不运行 UI 门禁。

四项 Rust 门禁均通过。测试构建仅报告现有 Windows linker stdout 信息，无失败测试。
