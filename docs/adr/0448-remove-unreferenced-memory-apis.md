# ADR 0448：移除未使用的 Memory API

- 状态：Implemented
- 日期：2026-10-04
- 范围：`haven-memory` 中已无 workspace 调用方的查询、投影与步骤写入入口
- 关联：ADR 0207、0217、0376、0379

## 背景

Memory 曾保留多组旧的存储入口：批量事实存在性查询、按消息 ID 删除 error snapshot 行、单独读取用户消息时间或 branch point cutoff、显式 replay 包装、单独 cursor 映射，以及不检查已存在行的 action-step identity 构造器。全仓调用点审计发现这些入口没有生产调用方；其中部分只被另一条同样未使用的 wrapper 调用，且其注释描述的 error snapshot 恢复已不符合当前 `last_msg_at` 投影截断规则。

当前 owner 已在事务化 rollback、`read_from`、`rollback_to`、`SessionStore` 的 ensure/start/finish action-step 写入，以及 typed fact stores 中直接实现仍在使用的路径。

## 决定

1. 删除 `Database::facts_exist_batch` 与仅供该方法使用的 `FactPresence` alias。
2. 删除无调用方的 `Database::delete_messages_by_ids`、`Database::last_user_message_ts` 及其 `SessionStore::last_user_message_at` wrapper。错误 partial 的投影截断仍由既有 `last_msg_at` 边界负责。
3. 删除 `SessionStore::projection_cutoff_for_step`、仅被它调用的 `branch_point_for_step`、同义包装 `replay_from` 和非事务 `sequence_for_transcript_cursor`。rollback 继续在单一事务中解析 cursor、branch point 与投影 cutoff；`read_from` 和事务内 cursor helper 保持不变。
4. 删除无调用方的 `Database::create_action_step_with_identity`，保留当前用于 resume/action lifecycle 的 `ensure_action_step_with_identity` 与事务化入口。
5. `SessionStore::read_active_branch_points` 只供本 crate 的测试使用，改为 `#[cfg(test)]`；rollback 仍使用 connection-scoped 实现。
6. 不保留 source-compatibility wrapper。Haven 当前没有承诺稳定的 Rust 下游 API；仓库内调用点和依赖 crate 的测试已完成核查。

## 替代方案

- 保留未调用的 public wrapper：会继续暴露重复查询与旁路写入口，且没有仓库消费者，拒绝。
- 把旧方法转发到新 owner：rollback/cursor 操作需要同一 SQLite 事务，单独的兼容 wrapper 会重新引入不完整的边界，拒绝。
- 保留基于 error snapshot message IDs 的删除语义：当前 error partial 不写入事件流，恢复靠 `last_msg_at` 截断；按 ID 另行删除不属于现行契约，拒绝。

## 影响与验证

- 这是 `haven-memory` Rust source API 收窄；无数据库 schema、持久数据、ID、X12、IPC、配置或用户数据变化，不需要重置数据库。
- 验证全 workspace 调用点无残留，并运行 Memory 测试与严格 Clippy；workspace 检查和测试验证其它 crate 与集成测试未依赖这些入口。
- rollback cursor 边界、error partial 截断、action identity resume 和 transcript 投影继续由现有 Memory/Agent 行为测试覆盖。

## 回滚

如需恢复调用接口，恢复对应 repository 方法即可；若重新引入跨事务 rollback/cursor 读取，必须先定义与当前事务 owner 一致的契约。无需数据重置。
