# ADR 0382：SessionState 持有 active ReAct run

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0361](0361-final-architecture-acceptance-audit.md)

## 背景

`SessionActor` 已在一个 task 内轮询 ReAct future，future 独占捕获 run-local `ReActState`；但 active-run slot 仍是 actor loop 的局部变量。这样热状态虽没有跨 task 或跨 session 共享，`SessionState` 类型本身仍未表达完整的 session owner 边界。

## 决定

1. 将 active ReAct run slot 移入 `SessionState::react_run`。该 slot 的 future 独占捕获 `ReActState`，包含当前 run 的 events、canonical、branch points、identity map、retry nudge 与 cancellation state。
2. actor task 在同一个 `select!` loop 中轮询 `SessionState::react_run` 和外部 mailbox。run future 不借用 `SessionState`，所以 pending provider/tool/storage await 不会阻止 actor 更新输入队列、交互或生命周期字段。
3. `ReActState` 保持 run-local 类型，不再把它留在 actor loop 局部 active-run slot，也不增加共享锁或 mailbox 往返。future 捕获状态是 `SessionState` 的运行槽所有权；无需为了字面字段布局，让 pending future 长期借用整个 `SessionState`。
4. run 完成后 future 与热状态一起释放；返回的 transcript event 投影和现有 durable/replay 契约不变。

## 影响与验证

没有数据库、配置、IPC、事件或用户数据变化。actor 回归覆盖 actor handler 等待期间仍可处理 submit/steer/cancel，以及 ReAct run 返回、重复启动拒绝和下一 run 恢复。

```text
cargo test --locked -p haven-agent session::actor::queue_tests --lib
```

## 回滚

回退 `SessionState::react_run` 字段及其 actor loop 使用即可；不需要数据重置。
