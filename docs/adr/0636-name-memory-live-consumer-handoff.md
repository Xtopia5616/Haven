# ADR 0636：明确 Memory live consumer 的一次性 handoff

## 状态

已采纳并实施。

## 背景

ADR 0367 将 memory runtime 的 live consumer 交由 `ApplicationRuntime` 注册并 join。当前 `MemoryStartup::start_prepared` 实际只构造一个包含 future 与 `MemoryReady` proof 的一次性 handoff；它不会启动 task，也不持有 `JoinHandle`。消费方随后调用通用 task registry，注册成功才取得 readiness proof。

类型名 `MemoryLiveTask` 把 handoff package 说成了 task 本身；`start_prepared` 又暗示 future 已经启动，`register_with` 则没有说明注册的对象。实际被注册的工作是消费 Memory `SessionEvent` live stream 并处理 cursor/recovery；应用 task registry 才拥有执行与 join 生命周期。

## 决定

1. 将 `MemoryLiveTask` 改为 `MemoryLiveConsumerHandoff`，表明它携带待注册的 live consumer future 与 readiness proof，不是已运行任务或 join handle。
2. 将 `MemoryStartup::start_prepared` 改为 `MemoryStartup::prepare_live_consumer`；将 handoff 的 `register_with` 改为 `register_consumer_with`。
3. App runtime 与 lifecycle tests 同步使用新名称。注册回调接受 future 并返回 `Some(())` 时才返回 `MemoryReady`；注册失败、prepare/replay 顺序、取消和 dispatcher fail-closed 行为均保持不变。
4. `MemoryRuntime` 仍为 Agent crate 私有实现，`MemoryStartup` 仍为应用持有的 lifecycle boundary。`ApplicationRuntime` task registry 拥有真实 task 与 join handle；本 ADR 不合并这些不同 owner，也不改变 task registry。

## 替代方案

- 保留 `MemoryLiveTask` 并只补注释：拒绝。它不是注册后拥有执行/join 生命周期的 task，类型名继续把 handoff 阶段与运行阶段混为一谈。
- 改名为 `MemoryLiveConsumerTask`：拒绝。仍会把待注册 future package 误称为 task。
- 把 readiness proof 合并到 `MemoryStartup` 或直接让 AgentLayer 启动 consumer：拒绝。会破坏“注册成功后才开放 dispatcher”的 fail-closed handoff 与应用持有 task/join 生命周期的边界。

## 影响与验证

- 这是 `haven-agent` 与 `haven-app-binary` 内部 Rust API rename；没有 Tauri IPC、wire、配置或数据库变更。
- 更新 ADR 0367 的 supersession note 与当前架构/命名路线图引用；历史 ADR 的原始决策内容保留。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、ADR 索引与 diff 检查。UI 未变更。

## 回滚

恢复 `MemoryLiveTask`、`start_prepared` 和 `register_with` 名称，并同步调用方与文档；不需要数据或运行态重置。
