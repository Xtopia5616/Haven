# ADR 0332：后台与定时任务共用 ActionLease claim core

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-common`、`haven-memory`、`haven-tools` 的 background completion 与 scheduled fire claim
- 关联：[ADR 0305](0305-action-service-action-store-port.md)、[ADR 0317](0317-action-terminal-lifecycle-kernel.md)、[ADR 0321](0321-action-owned-session-cancellation-traversal.md)、[ADR 0325](0325-action-completion-transport-ownership.md)

## 背景与现有语义

两类 action 的外层生命周期不同，但 claim 的有效期判断此前各自实现：

1. Background terminal completion 由 `action_completion_outbox` 持久化。`ActionStore` 调用 SQLite `BEGIN IMMEDIATE`，选择尚未送达且 `claimed_until` 为空或已过期的最早结果，再写入 30 秒 lease。Action row 与 outbox 由原子事务提交；transcript/event projection durable 后，消费者按稳定 `action_result_id` ack。ack 不检查 claim 是否仍有效，且 outbox reconcile 可从 completed/failed action row 重建缺失记录。
2. Scheduled fire 先由 `ActionStore` 对持久 action 做 `Waiting → Running` 条件更新，再更新进程内 map 并发布 completion。所有 receiver 共享 pending-fire map 与 15 分钟 `Instant` lease；lease 到期后，同一进程内的迟到 receiver 可以重新 claim。无 consumer 时按既有顺序清 lease、durable `Running → Waiting`、恢复内存状态并重装 timer；重排失败则保留 pending fire 供迟到 receiver 处理。
3. 当前没有独立的 claimant owner token，也没有 lease renewal 操作。Background 使用 `action_result_id` 作为稳定结果/ack identity，scheduled 使用 `action_id` 作为 fire identity。它们不是消费者身份。Background lease 会在进程重启后继续存在并在 30 秒到期后可恢复；scheduled pending fire 与 lease 都是进程内状态，重启后不会恢复。启动恢复只读取 durable `Waiting` scheduled rows，因此已提交为 `Running` 的 fire 不会由本 ADR 自动重放。

## 决定

1. 在 `haven-common::action_lease::ActionLease<T>` 放置纯 claim/lease 状态核心。泛型 deadline 支持 scheduled 的单调 `Instant` 与 outbox 从 SQLite 读取的 UTC datetime；每个实例只比较同一时钟域。
2. 两条路径共用 `ActionLease::can_claim` 的有效期判断。Scheduled fire 用 `try_claim` 生成进程内 guard；background outbox 先保留 SQL 条件选择以利用现有索引，再用相同纯判断校验当前 deadline，并由原 SQL 更新持久 claim。`matches_token` 和 `invalidate_for` 保护显式释放只作用于对应的稳定 identity；scheduled 路径依旧保留共享 claim map 及 claim → pending-fire 的锁顺序。
3. Background 的 30 秒期限、transaction 顺序、terminal CAS/outbox 原子写、durable ack 与 restart recovery 不变。Scheduled 的 15 分钟期限、fire CAS、timer rollback/rearm、pending-fire recovery、terminal cleanup 与广播 payload 不变。Action schema、ID、IPC、UI、取消/停机顺序、retry 和 terminal arbitration 均不变。
4. 不新增续租行为。Scheduled terminal transition 与 no-consumer rollback 仍按原路径清除 pending fire/lease；background terminal row 的 delivery claim 仍可在 lease 过期后重新取得，直到 transcript durable ack。这是 completion delivery lease，不是重新打开 terminal action。

## 替代方案

- 继续让 scheduled 与 background 各自实现时间点/过期判断：保留同一 claim 语义在 Rust 中分叉的风险，拒绝。
- 把 background transaction、scheduled CAS/timer 或 outbox ack 移入公共 core：会把持久化、恢复及各自 rollback 策略从 ActionStore/ActionService 搬进共享状态类型，拒绝。
- 增加消费者 owner token、续租或恢复 `Running` scheduled action：需要新的持久化/重启契约，会改变 schema 或既有行为，超出本切片。

## 验证

- `ActionLease` 单测覆盖新 claim、活跃 claim 冲突、边界过期、失效后重新 claim，以及 token mismatch 不得失效当前 lease。
- Outbox 测试覆盖同一完成结果在活跃 lease 内不能重复 claim、过期后可恢复，以及错误 `action_result_id` 不能 ack。
- Tools 测试保留 scheduled 多 receiver 去重、15 分钟过期重领、terminal 清除、广播恢复与 no-consumer timer rollback。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 影响与回滚

没有 schema、配置、IPC、ID 格式或用户数据迁移。回滚时移除 `ActionLease<T>` 与其两处调用，恢复 outbox 与 scheduled fire 各自原有的过期判断，并一并回退本 ADR、架构和路线图说明。

本切片不完成 Phase 7 Job lifecycle。trigger/execution、timeout/retry、tail output 与 UI projection 仍待单独设计和实现。
