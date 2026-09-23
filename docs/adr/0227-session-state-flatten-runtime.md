# ADR 0227：扁平化 SessionActor messaging 状态

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 `SessionState` 进程内 messaging 状态布局
- 关联：[ADR 0214](0214-react-run-inside-session-actor.md)、[ADR 0219](0219-stream-identity-runtime-boundary.md)

## 背景

`SessionRuntimeState` 只包含 `SessionMessagingState`，自身没有初始化、生命周期或行为职责。通过 `state.runtime.messaging` 访问并额外默认初始化这一层包装，使会话状态的结构多了一层没有表达额外边界的间接关系。

## 决定

1. 将 `SessionMessagingState` 直接作为 `SessionState.messaging` 字段，并删除 `SessionRuntimeState` 及其默认初始化。
2. 保持 `SessionMessagingState` 的字段和默认值不变。轮询游标、watch 通知消费、标题缓存和清理行为保持不变；现有 messaging 状态测试继续覆盖这些行为。
3. `SessionActor` 仍是 `SessionState` 的唯一写入者，mailbox 命令和状态所有权不变。
4. 本 ADR 仅改变进程内 Rust 结构布局，不改变 durable events、数据库 schema 或 wire 契约。

## 替代方案

- 保留包装类型：当前包装没有自己的字段或行为，继续保留只会增加访问和初始化层级。
- 将 messaging 状态放到 actor 外部：这会改变现有会话状态所有权及其 mailbox 边界，超出本切片范围。

## 影响

`SessionState` 直接持有 messaging 轮询状态和标题缓存。运行时初始化及读写路径少一层访问；轮询间隔、watch 订阅、标题缓存以及清理的可观察行为不变。actor mailbox、事件流、持久化和前后端契约均不变。

## 验证

- 复用 `messaging_poll_state_stays_on_the_session_and_clears` 覆盖标题缓存、按步数轮询、watch 通知和清理。
- `cargo fmt -p haven-agent -- --check`
- `cargo check --locked -p haven-agent`
- `cargo test --locked -p haven-agent`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `git diff --check`

## 回滚与重置

无持久化数据或契约变化，不需要数据重置。回滚时恢复 `SessionRuntimeState` 包装及其字段访问，并移除此 ADR 和目录索引项。
