# ADR 0321：ActionService 按 session 取消共用遍历骨架

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools::ActionService` 按 owner session 选择并串行取消 live background/scheduled action
- 关联：[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)

## 不变量与竞态

1. 按 session 清理只把 owner 匹配且仍 live 的 action 交给取消回调；种类由私有 `ActionKind` 限定。终态 action 不再次提交终态。
2. action map 只在选择快照时读锁；遍历按快照顺序串行，不跨异步取消持有 map 锁。状态或 owner 在选择与回调之间变化时，family-specific 路径仍须重新校验并仲裁。
3. Background 与 scheduled 生命周期保持分离：background 仍发送 kill channel、用 ActionStore CAS/outbox 提交并保留终态持久化重试；scheduled 仍经 `spawn_gate` / `TerminalTransitionGuard`，维持 timer/fire claim 清理、scheduled CAS/retry 与原事件顺序。
4. Background 原有 session cleanup 会从 board 移除已终态条目。live 选择之外，仍在 background 专属路径按原语义清理这类条目；这不会再次取消或发布终态。
5. 应用 teardown 的 background-only 路径仍不触碰 waiting scheduled；显式 end/delete 保持先取消 background、再取消 scheduled。callback 返回 false/错误仍不能中断后续项，family-specific 日志与重试留在原回调。

## 决定

增加私有 typed selection/traversal helper：按 `session_id`、`ActionKind` 与 live 状态生成一次 ID 快照，然后无锁、顺序调用 family-specific cancellation callback。该 helper 不拥有终态策略或持久化；它只统一选择和遍历。Background 额外拿到快照中的 terminal IDs，以保留原有 board cleanup。Scheduled 仍在 `cancel_scheduled` 中做最终 owner/kind/status 校验。

不重做 `ActionLifecycle` 或 terminal kernel，不移动 ActionStore 事务，不改 schema、IPC、ActionStore 方法签名、事件 payload、状态值或可见终态行为，也不扩大公共 API。

此项只是 cancellation skeleton，不代表完整 Job claim/lease 生命周期已统一。

## 验证

- 测试覆盖 typed 选择只把 live owned 且指定种类的 ID 交给 callback；terminal 与 non-owner 不作为取消目标；callback 顺序串行，单项 false/错误不短路后续项。
- 测试覆盖 background-only 保留 scheduled waiting、显式全量清理覆盖两类、terminal history 与其他 session 的 running action 不被取消，以及 scheduled 持久化取消失败后仍处理后续 action。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 未完成工作与回滚

共享 claim/lease、timeout、retry policy、tail output 与统一 UI projection 仍待独立切片；MemoryRuntime / MemoryWorker 生命周期迁移按 Phase 7 推进。回滚本 ADR 对应的 helper、测试、文档和索引记录即可；没有 schema、IPC 或用户数据重置要求。
