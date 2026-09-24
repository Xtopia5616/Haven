# ADR 0278：ReActState 独占 stream identity

- 状态：Accepted
- 日期：2026-09-24
- 范围：ReAct 流式 thought/reasoning 消息 ID 的进程内复用
- 关联：[ADR 0219](0219-stream-identity-runtime-boundary.md)、[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0276](0276-react-state-owns-token-estimate.md)

## 背景

`IdentityMap` 只服务于一次 ReAct run：流式 chunk、thought/reasoning 快照、错误
partial 和最终 transcript projection 必须复用同一个消息 ID。原实现却把 map 放在
共享的 `ReActEngine` 上，用 `(session_id, step, run, kind)` 作为 key，并在 run
开始/结束时额外清理。这使一个只属于单次运行的事实拥有了跨 session 的锁、session
key 和 RAII 清理路径。

## 决策

将 `IdentityMap` 放入每次新建的 `ReActState`，并把 key 缩小为
`(step, run, kind)`。流式主请求、压缩/响应重试、错误 partial 和最终投影都从同一
份 `ReActState` 取得 map；`StreamSession` 只持有该状态 map 的 `Arc`，以便流式
forwarder 在异步请求期间复用相同 ID。

删除 `ReActEngine` 的 identity facade、跨 session map 和 `RunMsgIdGuard`。状态自然
释放即完成清理，不改变 ID 前缀、复用规则、`block_msg_id` 未命中时生成新 ID 的
语义，也不把 identity 写入 durable event、projection 或恢复数据。

## 影响与验证

- 不同 ReAct run 的 identity map 天然隔离，不再依赖 session key 或结束清理；
- `thought` 继续使用 `step-`，`reasoning` 继续使用 `msg-`；
- 保留主请求、重试、partial 与最终投影的同一 ID 关系；
- 通过 `haven-agent` 494 项测试、`cargo check`、严格 Clippy，以及 IdentityMap/
  ReActState/流式重试聚焦测试；workspace 全量门禁在提交前复核。

## 回滚

可恢复 `ReActEngine` 上的 map 和 facade，无数据库、事件或 IPC 迁移；回滚只改变
进程内 identity 生命周期。
