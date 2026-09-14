# ADR 0158：MessagingService 接入 SessionActor mailbox

## 状态

已接受（2026-09-14）

## 背景

`MessagingService` 第一阶段已经统一 JSONL transport 的 claim、ack、retry、expiry 与
request/reply/receipt 语义，但同进程消息仍需经过文件适配器，peer spawn 与 lifecycle 还
通过可变 callback slot 反向接入 Agent。这样既浪费本地 I/O，也让消息路径和 session owner
存在两套隐式接线。

## 决定

1. `SessionActor` 通过 typed command mailbox 独占维护 session-local inbox、processing 和
   archive；`SessionSupervisor` 只负责从唯一 actor registry 找到 handle 并实现
   `SessionMailbox` port，不暴露 actor state 或新增 registry 镜像。
2. `MessagingService` 是唯一的应用层生命周期入口。`send`、`request`、`reply`、`receipt`
   共享 envelope 校验与投递路径；`claim` 返回 `MessageClaim`，由 `complete`、
   `complete_selected` 或 `retry` 统一驱动 ack、回执和 at-least-once 重投。expiry 规则由
   service 暴露的共享策略同时用于 mailbox 与 JSONL adapter。
3. 目标 session 在当前进程时优先使用 `SessionMailbox`；找不到本地 actor 时 fallback 到
   JSONL `MessageTransport`，以保留独立 Haven 进程之间的协作能力。两条路径共享稳定
   `msg-{uuid32}` identity、correlation 与 `delivery_attempt` 契约。
4. `MessagingRuntime` 同时承载 mailbox、peer spawn 和 lifecycle control。组合根只安装一个
   typed runtime，删除 `AgentSpawner` / `AgentController` 的 mutable callback slot。

## 不变量与验证

- 同一 session 的消息状态只由该 `SessionActor` 修改；service/工具/ReAct 不直接读取队列。
- request wait 只消费 authenticated matching reply，并通过 service 发送 read receipt；普通
  inbox 仍使用 claim → process → ack，未确认消息可重投。
- 过期消息不进入处理，但保留在 history/archive；重复 delivery 不改变 message identity。
- 验证覆盖工具层 request/reply/receipt 与 claim retry，以及 supervisor actor mailbox 的完整
  send → claim → retry → ack → receipt → reply → expiry 链路。

## 影响与回滚

同进程消息不再依赖文件锁和 JSONL I/O；跨进程兼容仍由 JSONL adapter 提供。同步 mailbox
操作必须运行在 service 的 `spawn_blocking` 边界（工具和 ReAct 已满足），避免在 Tokio async
worker 上执行阻塞式 actor command。回滚代码即可恢复仅 JSONL adapter 的消息路由，但应保留
processing 文件，交由旧版 retry/恢复逻辑处理。
