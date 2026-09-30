# ADR 0410：SessionActor 按 run 管理 cancellation token

- 状态：已采纳并实现（2026-09-30）
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0382](0382-session-state-owns-react-run.md)

## 背景

SessionActor 曾在 spawn 时创建单个 `CancellationToken`，中断命令取消该 token。暂停后的 Continue 会复用仍保留在内存中的 Actor；新 ReAct loop 继续读取已取消 token，因此可能在首次模型请求前直接以 Cancelled 退出。

## 决定

1. SessionActor 持有一个 actor-lifetime token；teardown 取消它。当前 run token 是其 child，actor teardown 会取消当前 run，但取消 run 不会关闭 Actor。
2. 每次 dispatcher claim 或直接 run admission 成功时，从未取消的 actor-lifetime token 派生新 run token，并通过 Actor handle 发布给 session cancellation 查询。Rejected admission 不更换 token。
3. `interrupt_session` 与 actor Cancel 命令只取消当前 run token。结束会话、删除会话与 supervisor quiesce 取消 actor-lifetime token；各自的持久化状态、等待与清理顺序不变。
4. Continue 在同一 Actor 上启动时获得新 token。ReAct loop 和工具执行继续通过现有 `cancellation_token(session_id)` 入口读取当前 run token，无 IPC、数据库、事件或 ID 变化。

## 影响与验证

新增 ReAct 集成回归：首个 provider 请求等待期间中断 run，等待它退出后对同一 session 执行 Continue，再确认同一 Actor 发起第二个 provider 请求并正常暂停。测试命令：

```text
cargo test --locked -p haven-agent interrupt_then_continue_on_same_actor_uses_a_fresh_run_token --lib
```

没有 schema、durable event、IPC 或用户数据变化。旧 run 的 token clone 仍保持 cancelled；新 run token 只影响新 run。

## 回滚

回退 actor lifetime token、run token watch 发布及本 ADR/回归即可；无需重置数据库。
