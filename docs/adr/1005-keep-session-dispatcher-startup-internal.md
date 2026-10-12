# 1005：将 Session dispatcher 启动入口限制在 Agent crate 内

## 状态

已采纳并实施。

## 背景

`SessionSupervisor` 的 dispatcher 启动方法以及 `SessionRunHandler` 曾作为公开 Rust API 暴露。全仓调用图显示，只有 Agent crate 内的 `AgentLayer` 和生命周期测试调用这些方法；App 的唯一生产启动入口是 `AgentLayer::start_after_memory_ready`。这些 dispatcher 方法用于 Agent 内部组装 recovery、cancellation 与 run handler，不是 App 应直接编排的生命周期契约。

ADR 0917 当时保留 `SessionRunHandler` 的原因是公开 dispatcher 方法的签名需要它。调用图复核后，这个前提已不成立：dispatcher 方法可限制为 crate 内部，handler 类型也可一起收窄。

## 决定

- 将包含 cancellation token 的两个 `SessionSupervisor::start_dispatcher*` 方法收窄为 `pub(crate)`，供 Agent 内部生产组装调用；无 cancellation 参数的两个便利包装仅在 `cfg(test)` 下编译。
- 将 `SessionRunHandler` 收窄为 `pub(crate)`，并从 Agent crate 根导出中移除。
- 保持 `AgentLayer::start_after_memory_ready` 为跨 crate 的生产启动入口；恢复策略、取消 token 与 run handler 的组装由 Agent 内部负责。
- 保留 Agent crate 内生命周期测试对 dispatcher 的直接覆盖。

## 影响

这收回了未被 Haven 工作区外部消费者使用的 Rust source API，不改变 App 启动流程、run admission、恢复、取消、错误归属、数据库 schema、事件 payload 或 IPC 契约。外部 Rust 使用者应通过 AgentLayer 生命周期入口启动生产 dispatcher。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- 搜索确认 dispatcher 启动方法与 handler 在 crate 外没有生产消费者。

## 回滚

若未来确有跨 crate dispatcher 编排需求，应先定义包含恢复与取消语义的稳定 Agent lifecycle 契约，再重新公开入口；无需数据迁移。
