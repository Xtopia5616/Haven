# ADR 0207：SessionStore 统一恢复边界与持久化 UI 序号

- 状态：Accepted
- 日期：2026-09-22
- 范围：`haven-memory`、`haven-agent`、Tauri/UI 事件契约

## 背景

Transcript 写入已经由 `SessionStore` 在 SQLite 事务内完成，但 UI 事件此前
仍由 Agent 在写入后单独生成，Tauri 还使用进程级 `AtomicU64` 作为事件序号。
这会让数据库提交顺序与 UI 事件身份脱钩；重启、并发提交或重复投影时，UI
无法把事件和 durable timeline 精确关联。

同时，resume/rollback 分别读取 cursor、branch point 和 event sequence，再
自行截断 projection，容易把 event clock、transcript cursor 和 message clock
重新混在一起。

## 决定

- `SessionStore::load_replay_state` 在一个只读 SQLite 边界内返回 active
  transcript、active branch points 和 `SessionCursor`。Agent 的恢复路径只从
  这个聚合结果重建状态。
- `SessionStore::rollback_to` 接收 transcript cursor、目标 step 和 projection
  boundary 意图，在一个 `BEGIN IMMEDIATE` 事务内重新解析 active timeline 与
  branch point，并解析目标消息或 branch point 的 projection cutoff。event
  high-water 若已变化、transcript cursor 无法映射，或目标消息不属于该 session，
  都会 fail closed，不追加 marker、不截断 projection。事务内截断 projection、
  追加 `timeline_rollback`，并可追加压缩摘要替换根；提交后按 durable sequence
  广播事件。Agent 不再直接解析 branch payload 或决定 projection 时间戳。
- Transcript 投影只有在事务提交成功后才构造 UI 事件；Action、Observation、
  Supplement、MediaPlan、Compaction 携带对应 `session_events.sequence`。Tauri
  不再生成进程级 durable event sequence，前端以该序号去重并关联恢复事件。发布时序由 ADR 0210 收紧：这些事件在提交成功后由 `CommittedUiPublisher` 从已提交行发布，而不是等可失败的投影完成后再发。Thought 同样携带 `event_seq`。并行工具卡不能只按序号去重。
- `event_cursor`、`event_sequence`、`last_msg_at` 和 `message_ingress_seq` 仍
  是独立时钟；本次改动只集中读取和传递边界，不允许互相推导。

## 替代方案

继续在 Agent/Tauri 层分配事件序号会使序号在重启后失效，并且无法证明 UI
事件对应哪个已提交的 durable row。让 Agent 各自加载 cursor、branch point
和 projection cutoff 则会保留跨查询竞态窗口。

## 影响、重置与回滚

不新增数据库 schema，也不要求重置数据库。旧事件仍通过 append-only log
保留；只有新的恢复与 rollback 调用改用 SessionStore 聚合 API。若 UI 暂时
不识别 `event_seq`，字段会被省略或忽略，不影响旧 payload 的读取。

## 验证

- `cargo test --locked -p haven-memory --lib`
- `cargo test --locked -p haven-agent --lib`
- `cargo test --locked -p haven-app-binary --lib`
- `corepack pnpm run check` 与 `corepack pnpm run test:run`（`ui/`）
