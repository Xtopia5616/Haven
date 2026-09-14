# ADR 0157：按 Supervisor、Actor、RunEngine 拆分会话运行时

## 状态

已接受（2026-09-14）

## 背景

`SessionExecutor` 同时承担全局调度、单会话状态、输入队列、交互请求、运行 slot 和跨层 callback。多个 `HashMap`、锁和 callback 的更新顺序共同决定会话行为，暂停、恢复、rollback 和结束路径很容易出现交叉竞态。

## 决策

- `SessionSupervisor` 只拥有跨会话职责：actor 注册表、FIFO pending 队列、并发 permit、调度唤醒和生命周期清理。
- `SessionActor` 通过 typed mailbox 串行拥有一个 session 的状态、follow-up/steering 队列、interaction、action completion、children 标记和 run lifecycle。调用方持有 `SessionActorHandle`，不接触 session 级 mutex。
- `RunEngine` 是一次运行的边界，dispatcher 负责 admission，actor 负责 claim/release，ReAct handler 负责执行。
- supervisor 通过 typed `SessionEvent` 广播 UI、推理和清理副作用；应用层不再注册 `on_*` callback 互相回调。
- `SessionStatus` 只表达通用生命周期；暂停原因由 `InteractionRequest` 的 kind/status 表达。status 与 run slot 分开建模，直接运行和 dispatcher 运行共享同一 release/join 语义。

## 不变量与验证

- 一个 session 的可变运行时状态只有一个 actor owner；队列和 interaction 不再分散在多个 session map。
- rollback/end/continue 在修改 durable snapshot 或投影前等待 actor 的 run slot 释放；direct run 的 guard drop 也通过 actor mailbox 发布释放命令。
- ask、confirm 和 scheduled confirm 走同一 `InteractionRequest` 生命周期，事件按 request id 可幂等恢复。
- `cargo test -p haven-agent --locked --lib` 覆盖 supervisor、actor、direct-run、resume、rollback 和 interaction 路径。

## 影响与回滚

这是运行时 owner 和内部 API 的结构性变更。对外保留 `SessionSupervisor`，旧 `SessionExecutor` 名称仅可在测试迁移期间作为 test-only type alias；生产代码不再依赖它。回滚需要同时恢复 dispatcher、session map 和 callback wiring，不应只恢复一个模块。
