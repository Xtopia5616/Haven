# ADR 0159：SessionEventStore 与 append-only 会话事件流

## 状态

已接受（2026-09-15）

## 背景

ReAct 的 transcript 过去保存在 `sessions.react_state` 的整块 snapshot 中。
`messages`、`session_steps` 和实时 `AgentEvent` 又分别承载投影与通知，导致
恢复、rollback 和实时状态没有共同的 durable cursor；长会话还会反复重写整个
snapshot。

## 决策

- 新增版本化 `session_events` 表，按 `(session_id, sequence)` 唯一排序，保存
  `event_type`、`event_version`、JSON payload、时间和可选 run/step identity。
- `haven_memory::SessionEventStore` 是唯一 append/replay 边界。写入在
  `BEGIN IMMEDIATE` 下分配序号，事务提交后才发布 live broadcast；消费者从
  最后 sequence replay，遇到 lag 必须重新 replay。
- transcript 与 branch point 作为 durable events 写入；compaction 通过新的
  `compact_summary` transcript root 取代 active prefix；rollback 只追加
  `timeline_rollback { to_sequence }` marker，不删除历史行。
- `ReActSnapshot.events` 降级为 checkpoint/cache。resume 优先重放事件流；只有
  没有事件行的旧会话才从有效 snapshot 做一次性导入。事件流存在时损坏的 snapshot
  不阻断恢复。
- `messages` / `session_steps` 继续是物化投影，ReAct 的 `apply_transcript` 先
  提交事件再更新热 projection；Tauri/UI 现有 `AgentEvent` bridge 保持不变，
  后续可在同一 envelope 上迁移更多 live projection。

## 不变量与验证

- 新事件只能 INSERT；事件表不提供 update/delete repository API，rollback 不会
  破坏审计历史；session 删除仍由外键级联清理整个本地会话。
- 事件序号在单会话内严格递增，payload 必须是 JSON，未知 event version 在 active
  replay 时拒绝。
- `cargo test --locked -p haven-memory --lib` 覆盖序号、批量提交、replay、compaction、
  rollback marker、branch point、live broadcast 与 payload 校验；Agent 测试覆盖
  transcript append、resume 优先事件流和 rollback。

## 影响、重置与回滚

数据库 schema 从 v20 升为 v21，旧数据库按测试版策略删除 `haven.db`、WAL 和 SHM
后重建；不提供运行时 schema migration。若回滚代码，必须同时回滚 v21 schema、
`SessionEventStore`、snapshot checkpoint 游标以及 Agent 的 resume/rollback 读取路径，
不能只恢复单个模块。
