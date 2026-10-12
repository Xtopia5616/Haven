# ADR 1087：Memory 身份与事件边界校验失败即拒绝

## 状态

已接受（2026-10-13）

## 背景

Memory 的持久契约有三个跨字段守卫缺口：负数 `session_events.run_id` 在读取时被静默转换成 `None`；标为 non-transcript 的 domain-event append API 可以接收 `transcript` 类型，绕过 transcript commit/projector 边界；`messages` 允许把共享 `step-*` 身份写到非 assistant role 行。

## 决定

- `run_id` 持久值只能是 `NULL` 或非负 SQLite integer；负值读回时作为损坏行返回转换错误，不降级成无 run 关联。
- domain-event API 拒绝 `transcript` 类型；transcript 记录必须经拥有事件与投影事务的 commit 路径追加。
- `msg-*` 可用于所有 canonical role；共享 `step-*` 消息身份只允许用于 assistant role，写入和读取都验证该组合。

## 影响

正常生产数据和 wire shape 不变；被直接篡改或违反事件边界写入的数据 fail closed。无需 schema 版本变更或数据重置。

## 验证

- Memory 单测覆盖负数 `run_id` 拒绝、domain event 对 transcript 的拒绝与事务不变性，以及 step 身份的角色约束。
- 完整 Rust workspace 门禁。

## 回滚

不得恢复静默丢弃运行身份、domain append 旁路 transcript commit 或允许不同 role 共用 assistant step 身份的行为。
