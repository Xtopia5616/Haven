# ADR 0196：SessionActor 独占运行态，SessionStore 负责事件边界

## 状态

已接受（2026-09-22）。ADR 0209 删除公开 `ReActSnapshot` 与 `sessions.react_state`，并把 session-local inbox 轮询状态放进 `SessionState`。

## 背景

一个 session 曾同时由 `SessionSupervisor`、`ReActEngine`、
`sessions.react_state`、`messages`、`session_steps` 和前端 reducer 保存运行态。
恢复因此需要解释 snapshot、projection 和多个 cursor 的不一致，交互请求还可能在
内存 registry 与 JSON snapshot 之间发生丢失或覆盖。

## 决定

- `SessionSupervisor` 只管理 session registry、并发 admission 和生命周期。
- `SessionActor` 独占一个 `SessionState`，包括 follow-up/steering/action 队列、
  interaction registry、run budget、usage、stream identity、token estimate、运行状态，
  以及 inbox 通知游标、轮询节拍和标题缓存。进程级 heartbeat 合并不进入 `SessionState`。
  外部输入统一通过 `Submit`、`Steer`、`ResolveInteraction`、`Cancel`、
  `BackgroundResult` 等 mailbox command 进入。
- interaction lifecycle 以 `interaction_requested`、`interaction_resolved`、
  `interaction_cleared` domain event 持久化；actor 启动时只 replay 这些事件。
  `messages` 和 `session_steps` 不参与交互或 ReAct 状态恢复。
- `SessionStore` 是唯一的 event/projection clock boundary。transcript 批次在一个事务内
  append event、更新 messages/steps projection 并在 commit 后广播 UI/live event；
  `last_msg_at`、`event_cursor`、`step_seq` 和 `message_ingress_seq` 不再由 ReAct
  层维护 sidecar。
- `ReActState` 只作为进程内投影 scratch；生产恢复边界由 `session_events` 的 event
  cursor 和 projection cutoff 表达，不另建 checkpoint 表，也不再保留 `sessions.react_state`。
  resume/rollback 不从 snapshot 导入，也不以 projection 修复 event stream。
- `TurnEngine` 是单 turn 的协调边界，返回按序执行的 `EffectBatch`；run budget 和
  生命周期仍由 run driver/actor 持有。

## 影响

恢复、回滚、并发输入和崩溃恢复都以同一条 event replay 路径为准。旧 JSON snapshot
不会再阻断新会话或覆盖 durable events；使用旧数据库仍按发布 reset contract 重建。
测试构造器若需建立交互，必须调用 actor command，而不是直接写 snapshot。

## 验证

- actor unit/integration tests 覆盖 interaction event replay、resolve、clear 和重启。
- Agent 回归覆盖 fresh run、resume、rollback、ask/confirm、取消和错误继续路径。
- 门禁：`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。
