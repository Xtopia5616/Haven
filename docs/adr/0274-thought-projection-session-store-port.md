# ADR 0274：thought projection 只依赖 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent EventDispatcher 的 thought step projection
- 关联：[ADR 0206](0206-session-event-projection.md)、[ADR 0255](0255-transcript-batch-session-store-port.md)、[ADR 0273](0273-memory-trigger-session-store-port.md)

## 背景

X12 中 assistant thought 的消息内容已经由 committed transcript event 投影到
`messages`，对应的 `session_steps` 行只是执行态投影。EventDispatcher 仍直接接收
`Arc<Database>`，自行把同步 `create_thought_step` 调度到 blocking pool，造成事件层越过
SessionStore 了解存储调度细节。

## 决策

由 `SessionStore::create_thought_step` 负责 thought step materialized projection 的 blocking
调度；EventDispatcher 的 thought helper 和 ReAct transcript projection 只传递 SessionStore。
消息内容仍只保存在 `messages`，step row 不写 thought 文本。事件发布顺序、失败后由 resume
修复 projection 的语义、ID 关联和数据库 schema 均不变。

## 影响与验证

- EventDispatcher thought projection 不再接收 raw Database；
- recovery thought 与正常 transcript thought 两条路径共用同一 SessionStore 端口；
- 通过 event bus、transcript projection focused tests、workspace check 和严格 Clippy；
- 无 IPC、事件 payload 或数据库迁移。

## 回滚

可回退 helper 参数和 SessionStore 方法，恢复 EventDispatcher 内部的 blocking Database 闭包；
无需数据迁移。
