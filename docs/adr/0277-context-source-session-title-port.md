# ADR 0277：ContextSource 通过 SessionStore 读取消息心跳标题

- 状态：Accepted
- 日期：2026-09-24
- 范围：Agent ReAct context assembly 的 session title read
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0272](0272-usage-runtime-session-store-port.md)

## 背景

`ContextSource` 的消息心跳已经把轮询游标和标题缓存放在 `SessionActor`，但缓存
缺失时仍直接持有 `Arc<Database>`，只为读取一个 session title。这样 ReAct context
assembly 越过 `SessionStore` 了解 blocking 调度和 session repository 细节。

## 决策

在 `SessionStore` 增加窄的异步 `session_title` 读取端口；`ContextSource` 只持有
`SessionStore`，标题缺失时通过该端口读取并继续写回 actor 的既有缓存。消息轮询
cadence、heartbeat 注册、title cache、inbox claim 和错误降级语义不变。

该端口只返回 `Option<String>`，不把完整 `Session` record 或 raw Database 暴露给
context assembly；不改变数据库 schema、IPC、消息协议或 durable event。

## 影响与验证

- ReAct context source 删除一处 raw `Database` 依赖；
- SessionStore 单测覆盖有标题和缺失 session；
- 通过 context/agent focused tests、workspace tests 和严格 Clippy。

## 回滚

恢复 `ContextSource` 的 `Arc<Database>` 字段和原有 `run_blocking` 读取即可，无数据
迁移；`SessionStore::session_title` 可随后删除。
