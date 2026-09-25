# ADR 0363：SessionSupervisor 构造使用 typed SessionStore

> 后续 [ADR 0367](0367-memory-runtime-application-ownership.md) 将 MemoryRuntime 长期 owner 与启动/live task 移至 ApplicationRuntime；SessionSupervisor 与 Agent/ReAct 使用同一 supervisor SessionStore 的 live sender 关系不变。

- 状态：已采纳（2026-09-26）
- 范围：`haven-agent::SessionSupervisor::{new, new_with_session_tool_overlay_port}` 的持久化构造边界
- 基线：HEAD `11df072`；工作区干净
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0251](0251-partial-stream-session-store-port.md)、[ADR 0295](0295-session-action-step-writes-through-session-store.md)、[ADR 0361](0361-final-architecture-acceptance-audit.md)

## 背景

`SessionSupervisor` 已只保存 `SessionStore`，其会话持久化路径也已迁移到该 typed boundary；但 `new` 和 `new_with_session_tool_overlay_port` 仍接收 `Arc<Database>`，并在构造内部创建 `SessionStore`。这让 public production constructor 继续暴露低层存储句柄，且把 store 的创建隐藏在 Agent 内部。

## 决定

1. `SessionSupervisor::new` 与 crate-private `new_with_session_tool_overlay_port` 均接收现成的 `SessionStore`。`new` 将同一实例传给带 overlay port 的构造路径；supervisor 字段及 `PartialStore` 仍共享该 store 的既有 clone。
2. `AppState` 在组合根显式创建 supervisor 使用的 `SessionStore` 并传入构造函数。App command/read store 继续是独立的 `SessionStore` 实例，以保留原有独立 live broadcast sender；supervisor 内部的 store 仍由其现有路径共享给 AgentLayer、MemoryRuntime 和 PartialStore。
3. Agent 单元与集成测试通过 `#[cfg(test)] SessionSupervisor::new_for_test` 保留 Database fixture 便利性。该 helper 先构造 `SessionStore`，再调用新的 typed constructor；带 overlay 的测试直接传入 `SessionStore`。生产代码不编译该 raw-Database helper。
4. `SessionStore` 加入 `haven-agent` crate-root 的既有持久化 boundary re-export，便于使用 `SessionSupervisor::new` 的调用方引用参数类型。没有增加其它 `SessionSupervisor` 构造器或兼容入口。
5. 不改变 `AgentLayer::new`、`MemoryRuntime`、`ToolsManager` 或其他 raw Database 路径。未更改 store 实现、X12、event replay、rollback、ID、排序、生命周期或测试行为。

## Clone 与生命周期审查

`SessionStore` 是 cloneable boundary，clone 共享同一 `Arc<Database>` 与 `broadcast::Sender<SessionEvent>`。此次把此前在 `SessionSupervisor::new` 内创建的同一 store 移至 `AppState` 组合根创建，没有合并它与 App command/read store，也没有额外复制 supervisor store。`SessionSupervisor` 仍在构造时将该 store clone 给 `PartialStore`；`AgentLayer` 仍通过 `session_store()` 获取同一广播域的 clone 并建立 `MemoryRuntime`。因此 dispatcher start、恢复订阅、shutdown 和 live event sender 的所有权关系保持不变，无需改为审计-only。

## 未迁移的 raw Database 边界

- `AppState` 仍将 `Arc<Database>` 交给 `AgentLayer::new`，用于创建 `MemoryService` / memory typed stores；生产 `AgentLayer` 不保留 Database 字段，`MemoryService` 私有保留 backing handle 用于构造 stores 与 embedding index（ADR 0299、0312、0362）。
- `MemoryService::new` 和 `SystemPromptBuilder::new` 仍公开接收 `Arc<Database>`；这两个入口与 supervisor 构造无关，本轮不改变。
- 工具运行时、其它 App/Agent persistence adapter 及仍明确依赖 raw Database 的路径继续由各自 ADR 追踪。本 ADR 不声称全局 raw Database 穿透已经清零。

## 验证

直接 typed-constructor 回归确保传入的 `SessionStore` 可观察 supervisor 创建的 session；带 overlay 的 resume 测试编译并使用 typed 构造入口。完整 Rust workspace 验证命令按本轮任务要求执行：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

UI 未修改，不运行 UI 门禁。没有 schema、wire、IPC 或用户数据变化，无需数据库重置。

## 回滚

回滚代码可恢复 `SessionSupervisor` 构造内部创建 store 的旧方式，并一并回滚对应 wiring、测试、路线图、架构与本 ADR 更新；不涉及数据库或用户数据重置。
