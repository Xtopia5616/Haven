# ADR 1077：按事件边界 cursor check 命名 ReAct 指标

## 状态

已接受（2026-10-13）

## 背景

ReAct lifecycle 路径在提交交互/终态边界时从 `session_events` 读取 `event_boundary_cursor`，用于完整性检查；当前没有序列化 ReActState snapshot。该阶段和失败计数却叫 `snapshot` / `snapshot_failures`，与 `docs/naming.md` 对 snapshot 的定义和实际存储行为冲突。

## 决定

- 将 metrics phase 改名为 `event_boundary_check`，失败计数改名为 `event_boundary_check_failures`。
- 同步更新 diagnostics runtime validator、生成的 IPC shape 和测试夹具；日志将 cursor 读取错误称为事件边界检查失败。
- `MetricsSnapshot` 仍表示只读的性能诊断结果；本决定只重命名其内部阶段与计数语义。

## 替代方案

- 保留 `snapshot` 作为历史指标名：不采纳，因为消费者会继续把 cursor 完整性查询误解为持久快照写入。
- 移除该指标：不采纳，因为事件边界读取失败仍需可观测。

## 影响

Diagnostics command 的 JSON 字段由 `snapshot_failures` 改为 `event_boundary_check_failures`，不涉及数据库或事件持久化。旧 UI/诊断消费者需与同一应用版本更新；仓库内 generated contract 与 UI validator 同步更新。

## 验证

- Rust event-boundary metrics tests 覆盖阶段和失败计数。
- IPC contract generator/check、Rust workspace tests、Clippy、UI 类型检查与 UI 测试通过。

## 回滚与数据重置

性能 metrics 不持久化，无需重置。若回滚该字段名，应同时回滚 generated command shape、UI validator 和诊断测试夹具。
