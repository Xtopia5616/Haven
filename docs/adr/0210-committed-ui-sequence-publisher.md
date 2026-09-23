# ADR 0210：提交成功后按 sequence 发布 durable UI 事件

## 状态

已接受（2026-09-23）

## 背景

ADR 0207 让 Action、Observation、Supplement、MediaPlan、Compaction 携带
`session_events.sequence`，但这些 live 事件仍在 `apply_transcript` 的内存投影之后由
Agent 再发一次。投影可能失败，于是数据库已经提交的行没有对应 UI 事件；Store 订阅和
提交任务如果各发一次，还会重复或乱序。`subscribe_session_events` 此前也没有接到界面。

Thought 不在那条序号上。并行工具调用共用一条 transcript sequence，只按 `eventSeq`
去重会丢掉第二张卡片。

## 决定

- `CommittedUiPublisher` 是已提交 transcript 行的唯一 live UI 发布者，覆盖 Thought、
  Action、Observation、Supplement、ingress MediaPlan 和 Compaction。
- `apply_transcript` 在事务提交成功后立刻按返回的 `SessionEvent` 发布。进程内
  canonical 更新和 Thought 的 `session_steps` 补写发生在发布之后，失败不能丢掉
  已经发出的 live 事件。
- Store 订阅桥与提交任务共用一把按 `(session_id, sequence)` 去重的门闩，门闩覆盖
  整个 emit。桥只从 `AgentLayer::set_emitter` 启动，且仅当当前线程已有 Tokio
  runtime；它动态读取当前 emitter。广播容量仍为 256。lag 时告警，提交任务仍发布
  自己的批次。
- 卡片上不属于 canonical `TranscriptRecord` 的字段写入同一 JSON 对象的可选 `ui`
  字段。旧 payload 仍可解码，因为未知字段被忽略。不升 schema，不重置数据库。
- 不进入这条 durable 序号：流式分片（只有 `chunk_seq`）、WebSearch、Usage（live
  事件自带累计值），以及请求准备阶段的 `emit_media_plan`（`event_seq` 为空）。
- 前端去重键是 `${eventSeq}:${identity}`。Action 与 Observation 用 `stepId`，
  Supplement 用 `supplementId`，Thought 用 `messageId`。没有 `eventSeq` 不去重。
- Tauri 仍不分配持久序号。`event_cursor`、`event_sequence`、`last_msg_at` 和
  `message_ingress_seq` 继续是独立时钟。

## 替代方案

继续在投影成功后发事件，无法消除“库里已有行、实时 UI 丢失”。让 Store 订阅成为
唯一发布者，则没有桥的测试和提交任务自己的发布路径会丢事件。只按 `eventSeq`
去重会丢掉同一 sequence 上的并行工具卡。

## 影响、重置与回滚

不新增表、列或 schema 版本，不要求按 `docs/release-and-reset.md` 重置。`ui` 只是
可选 JSON。回滚本变更不会留下必须迁移的数据；旧前端忽略未知的 `event_seq`。

## 验证

- `cargo test --locked -p haven-agent --lib`
- `cargo test --locked -p haven-app-binary --lib`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `cargo clippy --locked -p haven-app-binary -- -D warnings`
- `ui/` 下 `corepack pnpm run check` 与 `corepack pnpm run test:run`
