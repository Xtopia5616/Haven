# 0917：收回 Agent 未消费的运行时 API 并统一生产 run 入口

## 状态

已接受并实现（2026-10-11）。

## 背景

Agent crate 根同时暴露了 App 实际使用的 ports、事件和生命周期契约，以及仅在 Agent 内部使用的实现类型：ReAct engine/loop、transcript projection DTO/helper、compactor、Memory worker、prompt provider/renderer 和 session run engine。若干类型虽没有跨 crate 消费者，却因整组 `pub use` 而成为外部 API；这让实现重组需要维护没有调用方的第二套入口。

Agent 还提供 `AgentLayer::run_session_from_id`，允许绕过正式的 dispatcher 直接运行会话。该路径有独立 admission lease、等待取消、Actor 命令与 release barrier、失败状态提交和 `SessionRunPaused` 事件；仓库内只有 Agent 集成测试使用它。App 的生产 run 始终由 `SessionSupervisor` dispatcher 接纳和启动。两个入口并存重复了运行准入与退出协调责任，也让 direct-run 相关状态进入每次生产 session 的 supervisor/actor 结构。

## 决定

- Agent crate 根只公开有跨 crate 生产消费者、或必须出现在公开签名中的契约。ReAct、transcript 投影、ContextCompactor、MemoryWorker、prompt assembly/provider/renderer 与 `SessionRunEngine` 留在 Agent 内部；`MemoryEventProcessOutcome` 不再公开。
- 保留 App 实际实现或调用的 capability ports、Session/Interaction 生命周期契约、Memory startup/service、事件与指标、`ProcessResult`、SessionStore durable boundary。`SessionRunHandler` 继续公开，因为公开 supervisor dispatcher 方法的签名需要它。
- 生产会话 run 唯一经 supervisor dispatcher。`run_session_from_id` 以及 direct-run admission lease、waiter、状态标记和 Actor direct-run 命令/屏障仅在 `cfg(test)` 构建中存在，用于保留 Agent 生命周期回归覆盖；生产 supervisor/actor 不再持有 direct-run 专属状态。
- 删除只服务 direct-run 路径的 `SessionRunPaused` supervisor 事件及其 AgentLayer/App 消费分支。将仅供测试的 compaction/prompt/Memory 辅助入口限制在测试构建；删除没有调用方的布尔结果 wrapper。
- transcript 测试仍通过 Agent 内部 `types` 模块及测试专用根 re-export 访问 fixture；这些名称不进入默认生产 API。

## 影响

这是有意破坏性的 Rust source API 收窄。全仓未发现生产跨 crate 消费者调用 direct-run 方法、读取内部 transcript projection 或使用被收回的实现类型。App 既有启动、dispatcher admission、run cancellation、错误归属和 session lifecycle 路径保持不变。仅测试构建保留 direct-run 回归夹具；生产状态不再复制其 lease/waiter/actor release 状态。`SessionRunPaused` 没有 IPC/持久化表示，也没有除 direct-run 外的生产 producer。

不改变数据库 schema、事件 payload、IPC DTO、配置、授权规则或用户可见生产行为，无需重置数据。外部 Rust 调用方若曾直接依赖这些未被 Haven 工作区使用的入口，需要改用 Agent 的正式 lifecycle/ports 契约；不提供兼容别名。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- 全仓搜索确认 App/Tools 没有内部 Agent 实现路径、direct-run 生产调用或 `SessionRunPaused` 消费者。

## 替代方案

- 保留根导出并只依赖使用约定：拒绝；没有消费者的实现仍构成可依赖 API，且外部可绕过唯一 dispatcher 入口。
- 在 production 保留 direct-run leases 和 Actor release barrier：拒绝；正式 dispatcher 已是唯一生产入口，这些状态只服务测试路径。
- 保留 `SessionRunPaused` 事件并让生产消费者忽略：拒绝；唯一 producer 随 direct-run 入口一起退役，事件会继续暗示一个生产生命周期。

## 回滚

可恢复原根导出、direct-run supervisor/actor 状态与 `SessionRunPaused` 事件分支；无需修改或恢复数据。
