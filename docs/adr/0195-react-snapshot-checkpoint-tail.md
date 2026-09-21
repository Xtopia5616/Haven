# ADR 0195：ReAct snapshot 仅保留 checkpoint 与有限事件尾部

## 状态

已接受（2026-09-21）

## 背景

`session_events` 已是会话 transcript、branch point、resume、rollback 和 live
replay 的 append-only 权威来源。此前 `ReActSnapshot.events` 又把完整 transcript
压缩写入 `sessions.react_state`，导致每次 checkpoint 都重复存储并重写长事件流；
恢复时还需要在 snapshot、event sequence 与消息投影时间之间解释多个边界。

## 决定

- `ReActSnapshot.events` 仅作为进程内恢复/投影 scratch，不再由当前 serializer
  写入数据库。
- 新 snapshot 保存 `event_cursor`、运行时检查点元数据、interaction/budget 状态、
  ingress/projection 游标，以及最多 32 条 `event_tail` 诊断缓存。
- resume 与 rollback 在存在事件行时始终从 `SessionEventStore` 读取完整 active
  transcript；snapshot 不能覆盖或修复 durable event stream。
- 旧的、仍包含完整 `events` 的 snapshot 只在该 session 没有事件行时一次性导入。
  只有尾部缓存而 `event_cursor` 更大的新 snapshot 不可作为完整恢复源，并应要求
  durable `session_events` 或按发布重置策略处理。
- `event_cursor` 是 active transcript 的投影索引，`event_sequence` 是包含控制事件
  的 append-only 高水位，`last_msg_at` 只用于物化消息投影截断；它们属于不同边界，
  不互相推导，也不承载 transcript 内容。

## 影响与重置

这是 snapshot wire/storage 契约的破坏性收窄，不需要新增运行时数据库迁移；当前测试版
已有 schema 版本继续使用。升级前已存在的旧完整 snapshot 可由一次性导入路径处理，
无法导入或缺少对应 `session_events` 的数据按 `docs/release-and-reset.md` 删除
`haven.db`、WAL 和 SHM 重建。

## 验证

- Agent 单元测试验证 snapshot JSON 不含完整 `events`、保留 cursor、尾部上限为 32，
  并保留旧完整 snapshot 导入行为。
- Memory tests 验证 checkpoint 从显式 `event_cursor` 读取，而不是从缓存尾部长度推断。
- 回归门禁：`cargo test --locked -p haven-memory --lib`、
  `cargo test --locked -p haven-agent --lib`。
