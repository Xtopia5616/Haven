# ADR 0276：ReActState 独占 token estimate

- 状态：Accepted
- 日期：2026-09-24
- 范围：ReAct canonical transcript 的进程内 token estimate
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0219](0219-remove-token-estimate-mailbox.md)、[ADR 0222](0222-request-policy-snapshot.md)

## 背景

token estimate 只在一次 ReAct run 内对当前 canonical transcript 做增量复用，
但原实现将它放在共享 `ReActEngine` 中，以 session id、canonical generation、
revision 和 LRU map 管理。每个 run 都会重新创建 `ReActState`，因此这层跨 session
缓存既增加并发状态，也不能跨 run 提供有意义的复用；rollback/resume/compaction 还
需要额外 reset 路径。

## 决策

将一个可选的 token estimate 直接放入 `ReActState`。首次使用时按完整 canonical
估算；canonical 正常追加时在同一状态内按最后一条消息增量更新；任何非 append
canonical 改写和 compaction 都清空缓存，由下一次读取重建。新的 `ReActState` 自然
冷启动并随 run 释放，不再需要 session key、generation、revision、LRU、engine
reset 或 mailbox/facade 转发。

token estimate 仍是进程内性能缓存，不进入 durable event、projection、usage、
rollback 时钟或 UI event；canonical transcript 仍是唯一事实来源。

## 影响与验证

- `ReActEngine` 不再持有 token estimate sidecar；
- transcript append、turn context 和 compaction 直接操作当前 `ReActState`；
- 保留初始估值、append 增量、替换失效、compaction 重建和不同状态隔离测试；
- 通过 focused agent tests、workspace check、严格 workspace Clippy 和 workspace tests。

## 回滚

可恢复 `TokenEstimateCache` 及其 engine 转发，但无需数据库或 IPC 迁移；回滚只会
恢复进程内缓存实现，不改变 canonical 或 durable 语义。
