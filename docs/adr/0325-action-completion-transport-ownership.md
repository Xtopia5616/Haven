# ADR 0325：Action completion transport ownership

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` 内 background/scheduled action completion broadcast、scheduled fire recovery claim 与 receiver
- 关联：[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)、[ADR 0321](0321-action-owned-session-cancellation-traversal.md)

## 现状与不变量

`ActionService` 同时承载 action board、background process 与 scheduled timer 生命周期，并在同一文件中定义 completion DTO、broadcast receiver、scheduled pending-fire recovery map 和 claim lease。transport 与生命周期已是不同职责：ActionService 决定何时完成、何时写持久层及如何恢复 timer；completion transport 负责把已提交的 background result 或 scheduled fire 交给 Agent consumer。

本切片保持以下语义：

1. Background terminal row 与 completion outbox 继续由 ActionStore 在原子事务中提交；transient broadcast 丢失时，receiver 通过 durable outbox reconcile 恢复。Agent 的 transcript/event projection durable 后才调用 `acknowledge_background_completion`；queue admission 不会确认 outbox。
2. Scheduled fire 的 durable `Waiting → Running` CAS 仍先于进程内状态更新和 broadcast。pending fire 保留至 ActionService 处理 scheduled terminal transition；一个 service 内的所有 receiver 共用同一个 claim map。有效 claim lease 为 15 分钟，过期后可重新 claim，以恢复离开的 consumer。
3. Background event 直接交给 background receiver，不参与 scheduled claim。Scheduled receiver 仅认领 scheduled event；同一 action 的并发 receiver 不能同时得到有效 fire。
4. Broadcast lag 仍记录 warning 并继续接收；background receiver 同时按既有间隔及 lag 路径 reconcile durable outbox。Scheduled receiver 在等待 broadcast 前先尝试 claim pending recovery fire，lag 后也继续该恢复循环。Background stream 关闭时执行一次最终 outbox claim；scheduled stream 关闭且没有 pending recovery fire 时返回 `None`。
5. Scheduled broadcast 没有 consumer 时，ActionService 仍先清理本地 claim/pending，再按原重试次数尝试 durable `Running → Waiting`。rollback 成功后恢复内存 Waiting 并重装 timer；rollback 未成功则重新保留 pending fire，供迟到 receiver 恢复。
6. Claim map 在获取 pending fire map 之前加锁；claim、清理均保持 claim → pending 的 lock order。ActionService 继续拥有业务状态 map、spawn/kill、timer、ActionStore 调用、terminal transition 与 completion outbox ack。

## 决定

新增 crate-private `action_completion` 模块，拥有 `BackgroundActionCompletion`、`ScheduledActionFired`、`ActionCompletion`、`ActionCompletionReceiver`、`ScheduledFireClaim`、broadcast sender，以及 scheduled pending-fire/claim recovery map。DTO 和 receiver 仍从原 crate root 重导出；receiver 的公开方法、序列化字段和调用形态不变。

`ActionService` 组合一个 `ActionCompletionBus`，并只在原生命周期边界调用其 subscribe/send/retain/claim/clear 操作。ActionService 继续拥有 action 状态、进程与 timer、持久化、retry 和终态策略。ActionStore 接口与 schema、IPC JSON、事件 payload、lease 时长、lag/closed 处理、background durable ack 与 scheduled no-consumer rollback 均不变。

此项只收敛 completion transport ownership。background 与 scheduled 尚未共享完整的持久化 claim/lease、timeout、retry 或 UI projection 策略。

## 验证

- 新模块单测覆盖 scheduled claim 去重、lease 过期后重新 claim、background completion 直接通过，以及 broadcast lag 后继续读取和 channel closed 行为。
- ActionService 原集成测试继续覆盖 durable background outbox reconcile/ack、scheduled no-consumer rollback/rearm、rollback 失败后的迟到 consumer recovery 与跨 receiver claim。
- 通过 `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 及 `git diff --cached --check`。

## 替代方案

- 只把 DTO 移出文件、把 receiver/maps 留在 ActionService：transport claim、broadcast 和 payload 定义仍由 lifecycle 文件拥有，拒绝。
- 把 outbox ack 或 scheduled durable CAS 搬进 transport：会让传输模块拥有持久化和业务状态转换，超出职责拆分目标，拒绝。
- 在本切片统一 background/scheduled 的 Job claim、timeout、retry 或 UI projection：需要独立的持久化及消费者契约，超出本次移动边界，拒绝。

## 回滚

回退 ActionCompletionBus/module 及本 ADR/路线图记录，并将原 receiver/DTO 恢复至 `action_service.rs` 即可。没有 schema、配置、IPC 或用户数据迁移。
